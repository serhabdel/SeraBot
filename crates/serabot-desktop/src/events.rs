//! Server-Sent Events client for the owner API.
//!
//! The stream runs on a detached thread and delivers [`EventUpdate`] values over
//! a channel so the UI thread only drains updates and requests a repaint.

use crate::worker::Credentials;
use reqwest::{StatusCode, blocking::Client, redirect::Policy};
use secrecy::ExposeSecret;
use serde_json::Value;
use std::{
    io::{BufReader, ErrorKind, Read},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::Duration,
};
use url::Url;

const LIMIT: &str = "100";
const READ_CHUNK: usize = 8 * 1024;
const MAX_SSE_BUFFER: usize = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_POLL: Duration = Duration::from_millis(200);
const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// A live event stream backed by one detached thread.
pub struct EventStream {
    stop: Arc<AtomicBool>,
    receiver: Receiver<EventUpdate>,
}

impl EventStream {
    /// Spawns one detached thread that streams events from `credentials.endpoint`.
    ///
    /// `wake` runs after every queued update so the UI can request a repaint.
    /// `after` is the resume cursor; only positive cursors are sent.
    pub fn start(credentials: Arc<Credentials>, after: i64, wake: Box<dyn Fn() + Send>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        if std::thread::Builder::new()
            .name("serabot-events".into())
            .spawn(move || run(credentials, after.max(0), wake, sender, thread_stop))
            .is_err()
        {
            stop.store(true, Ordering::SeqCst);
        }
        Self { stop, receiver }
    }

    /// Pops the next pending update without blocking.
    pub fn try_recv(&self) -> Option<EventUpdate> {
        self.receiver.try_recv().ok()
    }

    /// Requests that the background thread stop; the thread is not joined.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One observable change from the event stream.
#[derive(Clone, Debug, PartialEq)]
pub enum EventUpdate {
    /// HTTP 200 stream established.
    Connected,
    /// A complete SSE frame carrying a numeric id.
    Event {
        /// Server event id, also the resume cursor.
        id: i64,
        /// Event kind from the `event:` field.
        kind: String,
        /// JSON payload from the `data:` field.
        payload: Value,
    },
    /// The stream ended or failed; the thread retries with backoff.
    Disconnected,
    /// The server returned HTTP 401; the thread stops.
    Unauthorized,
}

/// One complete SSE frame produced by [`SseParser`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseFrame {
    /// Last valid event id; SSE ids persist until replaced.
    pub id: Option<i64>,
    /// Event kind from the `event:` field, if one was present.
    pub event: Option<String>,
    /// Joined contents of all `data:` fields.
    pub data: String,
}

/// Incremental Server-Sent Events parser with a bounded internal buffer.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
    last_id: Option<i64>,
    event: Option<String>,
    data: String,
    data_seen: bool,
}

impl SseParser {
    /// Creates an empty parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a decoded chunk and return every frame completed by a blank line.
    pub fn feed(&mut self, chunk: &str) -> Vec<SseFrame> {
        let mut frames = Vec::new();
        if self.buffer.len().saturating_add(chunk.len()) > MAX_SSE_BUFFER {
            // A peer must not be able to grow this buffer without bound.
            self.buffer.clear();
            self.event = None;
            self.data.clear();
            self.data_seen = false;
            return frames;
        }
        self.buffer.push_str(chunk);
        while let Some(end) = self.buffer.find('\n') {
            let mut line = self.buffer.drain(..=end).collect::<String>();
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
            self.handle_line(&line, &mut frames);
        }
        frames
    }

    fn handle_line(&mut self, line: &str, frames: &mut Vec<SseFrame>) {
        if line.is_empty() {
            if self.data.is_empty() {
                self.event = None;
                self.data.clear();
            } else {
                frames.push(SseFrame {
                    id: self.last_id,
                    event: self.event.take(),
                    data: std::mem::take(&mut self.data),
                });
            }
            self.data_seen = false;
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let Some((field, value)) = line.split_once(':') else {
            return;
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "id" => {
                if let Ok(id) = value.parse::<i64>() {
                    self.last_id = Some(id);
                }
            }
            "event" => self.event = Some(value.to_owned()),
            "data" => {
                if self.data_seen {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.data_seen = true;
            }
            _ => {}
        }
    }
}

enum StreamEnd {
    Disconnected,
    Unauthorized,
    Stopped,
    ReceiverGone,
}

fn run(
    credentials: Arc<Credentials>,
    after: i64,
    wake: Box<dyn Fn() + Send>,
    sender: Sender<EventUpdate>,
    stop: Arc<AtomicBool>,
) {
    let Some(client) = build_client(&credentials.endpoint) else {
        emit(&sender, &*wake, EventUpdate::Disconnected);
        return;
    };
    let mut cursor = after;
    let mut backoff = 0;
    while !stop.load(Ordering::SeqCst) {
        match stream_once(
            &client,
            &credentials,
            &mut cursor,
            &sender,
            &*wake,
            &stop,
            &mut backoff,
        ) {
            StreamEnd::Unauthorized => {
                emit(&sender, &*wake, EventUpdate::Unauthorized);
                stop.store(true, Ordering::SeqCst);
                return;
            }
            StreamEnd::Stopped | StreamEnd::ReceiverGone => return,
            StreamEnd::Disconnected => {
                if !emit(&sender, &*wake, EventUpdate::Disconnected) {
                    return;
                }
                if !sleep_backoff(&stop, BACKOFF[backoff.min(BACKOFF.len() - 1)]) {
                    return;
                }
                backoff = (backoff + 1).min(BACKOFF.len() - 1);
            }
        }
    }
}

fn stream_once(
    client: &Client,
    credentials: &Credentials,
    cursor: &mut i64,
    sender: &Sender<EventUpdate>,
    wake: &(dyn Fn() + Send),
    stop: &AtomicBool,
    backoff: &mut usize,
) -> StreamEnd {
    let Some(url) = events_url(&credentials.endpoint, *cursor) else {
        return StreamEnd::Disconnected;
    };
    let response = match client
        .get(url)
        .bearer_auth(credentials.token.expose_secret())
        .send()
    {
        Ok(response) => response,
        Err(_) => return StreamEnd::Disconnected,
    };
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED {
        return StreamEnd::Unauthorized;
    }
    if !status.is_success() {
        return StreamEnd::Disconnected;
    }
    // A successful connect resets the retry schedule for the next failure.
    *backoff = 0;
    if !emit(sender, wake, EventUpdate::Connected) {
        return StreamEnd::ReceiverGone;
    }

    let mut reader = BufReader::with_capacity(READ_CHUNK, response);
    let mut decoder = Utf8Decoder::new();
    let mut parser = SseParser::new();
    let mut chunk = [0u8; READ_CHUNK];
    loop {
        if stop.load(Ordering::SeqCst) {
            return StreamEnd::Stopped;
        }
        match reader.read(&mut chunk) {
            Ok(0) => return StreamEnd::Disconnected,
            Ok(read) => {
                let text = decoder.push(&chunk[..read]);
                if text.is_empty() {
                    continue;
                }
                for frame in parser.feed(&text) {
                    let Some(id) = frame.id else {
                        continue;
                    };
                    *cursor = (*cursor).max(id);
                    let payload = serde_json::from_str(&frame.data).unwrap_or(Value::Null);
                    if !emit(
                        sender,
                        wake,
                        EventUpdate::Event {
                            id,
                            kind: frame.event.unwrap_or_default(),
                            payload,
                        },
                    ) {
                        return StreamEnd::ReceiverGone;
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return StreamEnd::Disconnected,
        }
    }
}

fn events_url(endpoint: &Url, after: i64) -> Option<Url> {
    let mut url = endpoint.clone();
    url.path_segments_mut()
        .ok()?
        .clear()
        .extend(["v1", "events"]);
    let mut query = url.query_pairs_mut();
    if after > 0 {
        query.append_pair("after", &after.to_string());
    }
    query.append_pair("limit", LIMIT);
    drop(query);
    Some(url)
}

fn build_client(endpoint: &Url) -> Option<Client> {
    let mut builder = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .retry(reqwest::retry::never())
        .connect_timeout(CONNECT_TIMEOUT)
        // reqwest defaults to a 30 s total timeout; an SSE stream is long-lived.
        .timeout(None::<Duration>);
    if endpoint.host_str() == Some("localhost") {
        // Never send a plaintext token to a DNS-resolved non-loopback address.
        builder = builder.resolve("localhost", SocketAddr::from(([127, 0, 0, 1], 0)));
    }
    builder.build().ok()
}

fn emit(sender: &Sender<EventUpdate>, wake: &(dyn Fn() + Send), update: EventUpdate) -> bool {
    if sender.send(update).is_err() {
        return false;
    }
    wake();
    true
}

fn sleep_backoff(stop: &AtomicBool, mut remaining: Duration) -> bool {
    while !remaining.is_zero() {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let step = remaining.min(STOP_POLL);
        std::thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
    !stop.load(Ordering::SeqCst)
}

/// Decodes UTF-8 across arbitrary chunk boundaries without losing split chars.
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut decoded = String::new();
        loop {
            let (valid, invalid) = match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    decoded.push_str(text);
                    self.pending.clear();
                    break;
                }
                Err(error) => (error.valid_up_to(), error.error_len()),
            };
            if valid > 0 {
                if let Ok(text) = std::str::from_utf8(&self.pending[..valid]) {
                    decoded.push_str(text);
                }
                self.pending.drain(..valid);
            }
            if let Some(length) = invalid {
                // Malformed bytes cannot be completed by a later chunk.
                self.pending.drain(..length);
            } else {
                break;
            }
        }
        decoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::atomic::AtomicUsize,
        time::Instant,
    };

    #[test]
    fn parser_reports_a_single_frame() {
        let mut parser = SseParser::new();
        let frames = parser.feed("id: 7\nevent: task\ndata: {\"ok\":true}\n\n");
        assert_eq!(
            frames,
            vec![SseFrame {
                id: Some(7),
                event: Some("task".into()),
                data: "{\"ok\":true}".into(),
            }]
        );
    }

    #[test]
    fn parser_accepts_chunks_split_at_arbitrary_boundaries() {
        let text = "id: 42\nevent: message\ndata: héllo, 世界 🚀\ndata: second line\n\n";
        let mut parser = SseParser::new();
        let mut frames = Vec::new();
        let mut start = 0;
        let mut width = 1;
        for (offset, _) in text.char_indices().skip(1) {
            if offset - start >= width {
                frames.extend(parser.feed(&text[start..offset]));
                start = offset;
                width = width % 4 + 1;
            }
        }
        frames.extend(parser.feed(&text[start..]));
        assert_eq!(
            frames,
            vec![SseFrame {
                id: Some(42),
                event: Some("message".into()),
                data: "héllo, 世界 🚀\nsecond line".into(),
            }]
        );
    }

    #[test]
    fn parser_handles_crlf_and_multiple_data_lines() {
        let mut parser = SseParser::new();
        let frames = parser.feed("id: 3\r\nevent: task\r\ndata: one\r\ndata: two\r\n\r\n");
        assert_eq!(
            frames,
            vec![SseFrame {
                id: Some(3),
                event: Some("task".into()),
                data: "one\ntwo".into(),
            }]
        );

        let frames = parser.feed("data:\ndata: after empty\n\n");
        assert_eq!(frames[0].data, "\nafter empty");
    }

    #[test]
    fn parser_ignores_comments_and_retry_hints() {
        let mut parser = SseParser::new();
        let frames = parser.feed(": keep-alive\nretry: 5000\ndata: value\n: mid-frame\n\n");
        assert_eq!(
            frames,
            vec![SseFrame {
                id: None,
                event: None,
                data: "value".into(),
            }]
        );
    }

    #[test]
    fn parser_holds_an_incomplete_frame_until_the_blank_line() {
        let mut parser = SseParser::new();
        assert!(parser.feed("id: 1\ndata: hel").is_empty());
        assert!(parser.feed("lo").is_empty());
        assert!(parser.feed("\n").is_empty());
        let frames = parser.feed("\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "hello");

        assert!(parser.feed("id: 2\ndata: bye").is_empty());
        let frames = parser.feed("\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id, Some(2));
        assert_eq!(frames[0].data, "bye");
    }

    #[test]
    fn parser_keeps_the_last_valid_id() {
        let mut parser = SseParser::new();
        let frames = parser
            .feed("id: 4\ndata: a\n\nid: nope\ndata: b\n\nid: 99999999999999999999\ndata: c\n\n");
        assert_eq!(
            frames.iter().map(|frame| frame.id).collect::<Vec<_>>(),
            vec![Some(4), Some(4), Some(4)]
        );
        let frames = parser.feed("id: 5\ndata: d\n\n");
        assert_eq!(frames[0].id, Some(5));
    }

    #[test]
    fn parser_survives_buffer_overflow() {
        let mut parser = SseParser::new();
        let huge = "x".repeat(MAX_SSE_BUFFER + 1);
        assert!(parser.feed(&huge).is_empty());
        let frames = parser.feed("id: 9\ndata: ok\n\n");
        assert_eq!(
            frames,
            vec![SseFrame {
                id: Some(9),
                event: None,
                data: "ok".into(),
            }]
        );
    }

    #[test]
    fn decoder_keeps_multibyte_chars_split_across_chunks() {
        let text = "id: 5\ndata: héllo 世界 🚀\n\n";
        let mut decoder = Utf8Decoder::new();
        let mut parser = SseParser::new();
        let mut decoded = String::new();
        let mut frames = Vec::new();
        for byte in text.as_bytes() {
            let piece = decoder.push(std::slice::from_ref(byte));
            decoded.push_str(&piece);
            frames.extend(parser.feed(&piece));
        }
        assert_eq!(decoded, text);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id, Some(5));
        assert_eq!(frames[0].data, "héllo 世界 🚀");
    }

    #[test]
    fn stream_reconnects_and_resumes_after_last_event() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint =
            crate::validate_endpoint(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
        let token = "synthetic-events-token-0123456789abcdef";
        let server = std::thread::spawn(move || {
            let responses = [
                concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "Content-Type: text/event-stream\r\n",
                    "Connection: close\r\n",
                    "\r\n",
                    "id: 1\nevent: task\ndata: {\"n\":1}\n\n",
                    "id: 2\nevent: task\ndata: {\"n\":2}\n\n",
                ),
                concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "Content-Type: text/event-stream\r\n",
                    "Connection: close\r\n",
                    "\r\n",
                    "id: 3\nevent: task\ndata: {\"n\":3}\n\n",
                ),
            ];
            for (index, response) in responses.iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                assert!(request.starts_with("GET /v1/events?"), "{request}");
                assert!(
                    request.to_ascii_lowercase().contains(
                        "authorization: bearer synthetic-events-token-0123456789abcdef\r\n"
                    ),
                    "{request}"
                );
                if index == 0 {
                    assert!(request.contains("GET /v1/events?limit=100 "), "{request}");
                    assert!(!request.contains("after="), "{request}");
                } else {
                    assert!(request.contains("after=2"), "{request}");
                    assert!(request.contains("limit=100"), "{request}");
                }
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });

        let credentials = Arc::new(Credentials {
            endpoint,
            token: secrecy::SecretString::new(token.into()),
        });
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wakes);
        let stream = EventStream::start(
            credentials,
            0,
            Box::new(move || {
                wake_counter.fetch_add(1, Ordering::SeqCst);
            }),
        );

        let deadline = Instant::now() + Duration::from_secs(8);
        let mut updates = Vec::new();
        while Instant::now() < deadline {
            while let Some(update) = stream.try_recv() {
                updates.push(update);
            }
            if updates
                .iter()
                .any(|update| matches!(update, EventUpdate::Event { id: 3, .. }))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        stream.stop();
        server.join().unwrap();

        assert_eq!(
            updates.first(),
            Some(&EventUpdate::Connected),
            "{updates:?}"
        );
        assert!(
            updates
                .iter()
                .filter(|update| matches!(update, EventUpdate::Connected))
                .count()
                >= 2,
            "{updates:?}"
        );
        let events = updates
            .iter()
            .filter_map(|update| match update {
                EventUpdate::Event { id, kind, payload } => {
                    Some((*id, kind.as_str(), payload.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![
                (1, "task", serde_json::json!({"n": 1})),
                (2, "task", serde_json::json!({"n": 2})),
                (3, "task", serde_json::json!({"n": 3})),
            ]
        );
        std::thread::sleep(Duration::from_millis(20));
        assert!(wakes.load(Ordering::SeqCst) >= updates.len());
    }

    #[test]
    fn stream_reports_unauthorized_without_connected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint =
            crate::validate_endpoint(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
        let token = "synthetic-unauthorized-token-0123456789";
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer synthetic-unauthorized-token-0123456789\r\n"),
                "{request}"
            );
            stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            stream.flush().unwrap();
        });

        let credentials = Arc::new(Credentials {
            endpoint,
            token: secrecy::SecretString::new(token.into()),
        });
        let stream = EventStream::start(credentials, 0, Box::new(|| {}));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut updates = Vec::new();
        while Instant::now() < deadline {
            while let Some(update) = stream.try_recv() {
                updates.push(update);
            }
            if updates
                .iter()
                .any(|update| matches!(update, EventUpdate::Unauthorized))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        stream.stop();
        server.join().unwrap();

        assert!(updates.contains(&EventUpdate::Unauthorized), "{updates:?}");
        assert!(!updates.contains(&EventUpdate::Connected), "{updates:?}");
    }

    fn read_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "client closed before sending a complete request");
            request.extend_from_slice(&buffer[..read]);
            assert!(request.len() <= 16 * 1024, "request too large");
        }
        String::from_utf8(request).unwrap()
    }
}
