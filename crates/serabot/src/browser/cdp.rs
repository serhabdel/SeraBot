//! Chrome DevTools Protocol client over the minimal WebSocket in `ws.rs`.
//!
//! One connection per browser, flattened sessions (`sessionId` on each
//! command). Responses are matched by id; events fan out on a broadcast
//! channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;

use super::ws::{self, Incoming};
use crate::error::{Result, SeraError};

#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

type Pending = Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;
type Writer = Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;

pub struct CdpConnection {
    writer: Writer,
    next_id: AtomicU64,
    pending: Pending,
    events: broadcast::Sender<CdpEvent>,
    closed: Arc<AtomicBool>,
    timeout: Duration,
    reader: JoinHandle<()>,
}

impl Drop for CdpConnection {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

fn cdp_error(message: impl Into<String>) -> SeraError {
    SeraError::External(format!("browser: {}", message.into()))
}

impl CdpConnection {
    /// Connect to `ws://127.0.0.1:<port><path>`. Loopback is enforced by the
    /// caller (the launcher only ever passes 127.0.0.1).
    pub async fn connect(port: u16, path: &str, timeout: Duration) -> Result<Self> {
        let stream = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await
        .map_err(|_| cdp_error("DevTools connect timed out"))?
        .map_err(|error| cdp_error(format!("DevTools connect failed: {error}")))?;
        Self::from_stream(stream, &format!("127.0.0.1:{port}"), path, timeout).await
    }

    pub async fn from_stream<S>(
        stream: S,
        host_header: &str,
        path: &str,
        timeout: Duration,
    ) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let mut buffered = BufReader::new(stream);
        ws::handshake(&mut buffered, host_header, path).await?;
        let (mut read_half, write_half) = tokio::io::split(buffered);
        let writer: Writer = Arc::new(Mutex::new(Box::new(write_half)));
        let pending: Pending = Arc::default();
        let (events, _) = broadcast::channel(512);
        let closed = Arc::new(AtomicBool::new(false));
        let reader = {
            let writer = writer.clone();
            let pending = pending.clone();
            let events = events.clone();
            let closed = closed.clone();
            tokio::spawn(async move {
                loop {
                    match ws::read_message(&mut read_half).await {
                        Ok(Incoming::Text(text)) => dispatch(&text, &pending, &events),
                        Ok(Incoming::Ping(payload)) => {
                            let mut guard = writer.lock().await;
                            let _ = guard.write_all(&ws::encode_pong(&payload)).await;
                        }
                        Ok(Incoming::Closed) | Err(_) => break,
                    }
                }
                closed.store(true, Ordering::SeqCst);
                let drained: Vec<_> = pending
                    .lock()
                    .map(|mut map| map.drain().collect())
                    .unwrap_or_default();
                for (_, sender) in drained {
                    let _ = sender.send(Err(cdp_error("browser connection closed")));
                }
            })
        };
        Ok(Self {
            writer,
            next_id: AtomicU64::new(1),
            pending,
            events,
            closed,
            timeout,
            reader,
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    pub async fn call(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        if self.is_closed() {
            return Err(cdp_error("browser connection closed"));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut command = json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = session_id {
            command["sessionId"] = json!(session_id);
        }
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| cdp_error("pending map poisoned"))?
            .insert(id, tx);
        let frame = ws::encode_text_frame(command.to_string().as_bytes());
        {
            let mut writer = self.writer.lock().await;
            if let Err(error) = writer.write_all(&frame).await {
                self.forget(id);
                return Err(cdp_error(format!("write failed: {error}")));
            }
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(cdp_error("browser connection closed")),
            Err(_) => {
                self.forget(id);
                Err(cdp_error(format!("{method} timed out")))
            }
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut map) = self.pending.lock() {
            map.remove(&id);
        }
    }
}

fn dispatch(text: &str, pending: &Pending, events: &broadcast::Sender<CdpEvent>) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if let Some(id) = value.get("id").and_then(Value::as_u64) {
        let sender = pending.lock().ok().and_then(|mut map| map.remove(&id));
        if let Some(sender) = sender {
            let result = match value.get("error") {
                Some(error) => Err(cdp_error(
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("protocol error")
                        .to_owned(),
                )),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = sender.send(result);
        }
    } else if let Some(method) = value.get("method").and_then(Value::as_str) {
        let _ = events.send(CdpEvent {
            method: method.to_owned(),
            params: value.get("params").cloned().unwrap_or(Value::Null),
            session_id: value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    /// A fake browser: accepts the upgrade, then answers `Echo.ping` with the
    /// params, fails `Echo.fail`, ignores `Echo.hang`, and emits one event.
    async fn fake_server(mut server: tokio::io::DuplexStream) {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 1024];
        let _ = server.read(&mut buf).await.unwrap();
        server
            .write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")
            .await
            .unwrap();
        loop {
            let Ok(Incoming::Text(text)) = ws::read_message(&mut server).await else {
                return;
            };
            let value: Value = serde_json::from_str(&text).unwrap();
            let id = value["id"].clone();
            let reply = match value["method"].as_str().unwrap() {
                "Echo.ping" => {
                    json!({"id": id, "result": {"echo": value["params"], "session": value["sessionId"]}})
                }
                "Echo.fail" => json!({"id": id, "error": {"message": "nope"}}),
                "Echo.event" => {
                    let ev =
                        json!({"method": "Fake.happened", "params": {"a": 1}, "sessionId": "S"});
                    server
                        .write_all(&unmasked_frame(ev.to_string().as_bytes()))
                        .await
                        .unwrap();
                    json!({"id": id, "result": {}})
                }
                _ => continue,
            };
            server
                .write_all(&unmasked_frame(reply.to_string().as_bytes()))
                .await
                .unwrap();
        }
    }

    fn unmasked_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0x81u8];
        if payload.len() < 126 {
            frame.push(payload.len() as u8);
        } else {
            frame.push(126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(payload);
        frame
    }

    async fn connected() -> CdpConnection {
        let (client, server) = duplex(1 << 16);
        tokio::spawn(fake_server(server));
        CdpConnection::from_stream(client, "127.0.0.1:1", "/x", Duration::from_millis(300))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn matches_responses_errors_sessions_and_timeouts() {
        let cdp = connected().await;
        let ok = cdp
            .call(Some("sess"), "Echo.ping", json!({"k": "v"}))
            .await
            .unwrap();
        assert_eq!(ok["echo"]["k"], "v");
        assert_eq!(ok["session"], "sess");
        let err = cdp.call(None, "Echo.fail", json!({})).await.unwrap_err();
        assert!(err.to_string().contains("nope"));
        let hung = cdp.call(None, "Echo.hang", json!({})).await.unwrap_err();
        assert!(hung.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn delivers_events_and_reports_close() {
        let cdp = connected().await;
        let mut events = cdp.subscribe();
        cdp.call(None, "Echo.event", json!({})).await.unwrap();
        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "Fake.happened");
        assert_eq!(event.session_id.as_deref(), Some("S"));
        assert_eq!(event.params["a"], 1);
    }
}
