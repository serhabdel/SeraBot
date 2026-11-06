//! Minimal RFC 6455 client, just enough for Chromium's DevTools endpoint on
//! loopback (text frames, ping/pong, close, fragmentation). Hand-rolled
//! because no async WebSocket crate is in `Cargo.lock` (only the blocking
//! `tungstenite`, pulled in by a dependency), and adding one for a loopback
//! JSON pipe is not worth a new dependency.

use rand::RngCore;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::error::{Result, SeraError};

/// Largest message accepted from the browser (full-page JPEG as base64).
pub const MAX_MESSAGE_BYTES: usize = 24 * 1024 * 1024;

fn ws_error(message: impl Into<String>) -> SeraError {
    SeraError::External(format!("browser websocket: {}", message.into()))
}

/// Perform the client handshake on `stream` for `path` and `host_header`.
pub async fn handshake<S>(stream: &mut BufReader<S>, host_header: &str, path: &str) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if path.contains(['\r', '\n', ' ']) || host_header.contains(['\r', '\n']) {
        return Err(ws_error("invalid handshake target"));
    }
    let mut key = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut key);
    let key = base64_encode(&key);
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .get_mut()
        .write_all(request.as_bytes())
        .await
        .map_err(|e| ws_error(e.to_string()))?;
    let mut status = String::new();
    stream
        .read_line(&mut status)
        .await
        .map_err(|e| ws_error(e.to_string()))?;
    if !status.starts_with("HTTP/1.1 101") {
        return Err(ws_error(format!("upgrade refused: {}", status.trim())));
    }
    for _ in 0..64 {
        let mut line = String::new();
        let read = stream
            .read_line(&mut line)
            .await
            .map_err(|e| ws_error(e.to_string()))?;
        if read == 0 {
            return Err(ws_error("connection closed during handshake"));
        }
        if line == "\r\n" {
            return Ok(());
        }
    }
    Err(ws_error("handshake headers too long"))
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Encode one masked client text frame.
pub fn encode_text_frame(payload: &[u8]) -> Vec<u8> {
    encode_frame(0x1, payload)
}

fn encode_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x80 | opcode);
    let len = payload.len();
    if len < 126 {
        frame.push(0x80 | len as u8);
    } else if len <= u16::MAX as usize {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
    let mut mask = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut mask);
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    frame
}

/// What the reader produced for one complete message.
#[derive(Debug, PartialEq, Eq)]
pub enum Incoming {
    Text(String),
    /// A ping that needs a pong with the same payload.
    Ping(Vec<u8>),
    Closed,
}

/// Read frames until one complete text message, ping, or close arrives.
pub async fn read_message<R>(reader: &mut R) -> Result<Incoming>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut message: Vec<u8> = Vec::new();
    loop {
        let mut head = [0u8; 2];
        if reader.read_exact(&mut head).await.is_err() {
            return Ok(Incoming::Closed);
        }
        let fin = head[0] & 0x80 != 0;
        let opcode = head[0] & 0x0f;
        let masked = head[1] & 0x80 != 0;
        let mut len = (head[1] & 0x7f) as u64;
        if len == 126 {
            let mut ext = [0u8; 2];
            reader
                .read_exact(&mut ext)
                .await
                .map_err(|e| ws_error(e.to_string()))?;
            len = u16::from_be_bytes(ext) as u64;
        } else if len == 127 {
            let mut ext = [0u8; 8];
            reader
                .read_exact(&mut ext)
                .await
                .map_err(|e| ws_error(e.to_string()))?;
            len = u64::from_be_bytes(ext);
        }
        if len as usize > MAX_MESSAGE_BYTES || message.len() + len as usize > MAX_MESSAGE_BYTES {
            return Err(ws_error("message too large"));
        }
        let mut mask = [0u8; 4];
        if masked {
            reader
                .read_exact(&mut mask)
                .await
                .map_err(|e| ws_error(e.to_string()))?;
        }
        let mut payload = vec![0u8; len as usize];
        reader
            .read_exact(&mut payload)
            .await
            .map_err(|e| ws_error(e.to_string()))?;
        if masked {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
        }
        match opcode {
            0x0..=0x2 => {
                message.extend_from_slice(&payload);
                if fin {
                    let text = String::from_utf8(message)
                        .map_err(|_| ws_error("non-UTF-8 text message"))?;
                    return Ok(Incoming::Text(text));
                }
            }
            0x8 => return Ok(Incoming::Closed),
            0x9 => return Ok(Incoming::Ping(payload)),
            0xA => {}
            _ => return Err(ws_error("unsupported opcode")),
        }
    }
}

/// Encode a pong reply.
pub fn encode_pong(payload: &[u8]) -> Vec<u8> {
    encode_frame(0xA, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn server_frame(opcode: u8, fin: bool, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![(if fin { 0x80 } else { 0 }) | opcode];
        if payload.len() < 126 {
            frame.push(payload.len() as u8);
        } else {
            frame.push(126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(payload);
        frame
    }

    #[tokio::test]
    async fn reads_text_fragments_ping_and_close() {
        let (mut client, mut server) = duplex(1 << 16);
        let long = "x".repeat(300);
        server
            .write_all(&server_frame(0x1, false, b"hel"))
            .await
            .unwrap();
        server
            .write_all(&server_frame(0x0, true, b"lo"))
            .await
            .unwrap();
        server
            .write_all(&server_frame(0x9, true, b"p"))
            .await
            .unwrap();
        server
            .write_all(&server_frame(0x1, true, long.as_bytes()))
            .await
            .unwrap();
        server
            .write_all(&server_frame(0x8, true, b""))
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut client).await.unwrap(),
            Incoming::Text("hello".into())
        );
        assert_eq!(
            read_message(&mut client).await.unwrap(),
            Incoming::Ping(b"p".to_vec())
        );
        assert_eq!(
            read_message(&mut client).await.unwrap(),
            Incoming::Text(long)
        );
        assert_eq!(read_message(&mut client).await.unwrap(), Incoming::Closed);
    }

    #[tokio::test]
    async fn client_frames_are_masked_and_round_trip() {
        let frame = encode_text_frame(b"{\"id\":1}");
        assert_eq!(frame[0], 0x81);
        assert_ne!(frame[1] & 0x80, 0, "client frames must be masked");
        let (mut a, mut b) = duplex(1024);
        b.write_all(&frame).await.unwrap();
        assert_eq!(
            read_message(&mut a).await.unwrap(),
            Incoming::Text("{\"id\":1}".into())
        );
    }

    #[tokio::test]
    async fn handshake_accepts_101_and_rejects_others() {
        let (client, mut server) = duplex(4096);
        let mut reader = BufReader::new(client);
        let task = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let _ = server.read(&mut buf).await.unwrap();
            server
                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n")
                .await
                .unwrap();
        });
        handshake(&mut reader, "127.0.0.1:1", "/devtools/browser/x")
            .await
            .unwrap();
        task.await.unwrap();

        let (client, mut server) = duplex(4096);
        let mut reader = BufReader::new(client);
        let task = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let _ = server.read(&mut buf).await.unwrap();
            server
                .write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n")
                .await
                .unwrap();
        });
        assert!(handshake(&mut reader, "127.0.0.1:1", "/x").await.is_err());
        task.await.unwrap();
    }

    #[tokio::test]
    async fn oversize_messages_are_refused() {
        let (mut a, mut b) = duplex(64);
        let mut frame = vec![0x81u8, 127];
        frame.extend_from_slice(&((MAX_MESSAGE_BYTES as u64) + 1).to_be_bytes());
        b.write_all(&frame).await.unwrap();
        assert!(read_message(&mut a).await.is_err());
    }
}
