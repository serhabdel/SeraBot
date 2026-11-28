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
    time::{Duration, Instant},
};
use url::Url;

const LIMIT: &str = "100";
const READ_CHUNK: usize = 8 * 1024;
const MAX_SSE_BUFFER: usize = 256 * 1024;
const CONNECT_TIMEOUT: Duration = crate::net_policy::CONNECT_TIMEOUT;
const STOP_POLL: Duration = Duration::from_millis(200);
/// The server sends a keep-alive comment every 10 s. Four missed ones mean the
/// socket is half-open (sleep/resume, Wi-Fi switch, NAT drop) and must be
/// replaced; a blocked read would otherwise never report it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(40);
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
    reconnect_wake: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    receiver: Receiver<EventUpdate>,
}

impl EventStream {
    /// Spawns one detached thread that streams events from `credentials.endpoint`.
    ///
    /// `wake` runs after every queued update so the UI can request a repaint.
    /// `after` is the resume cursor; without one the stream starts at the
    /// server's newest event (`after=latest`).
    pub fn start(credentials: Arc<Credentials>, after: i64, wake: Box<dyn Fn() + Send>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let reconnect_wake = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_wake = Arc::clone(&reconnect_wake);
        if std::thread::Builder::new()
            .name("serabot-events".into())
            .spawn(move || {
                run(
                    credentials,
                    after.max(0),
                    wake,
                    sender,
                    thread_stop,
                    thread_wake,
                )
            })
            .is_err()
        {
            stop.store(true, Ordering::SeqCst);
        }
        Self {
            stop,
            reconnect_wake,
            receiver,
        }
    }

    /// Pops the next pending update without blocking.
    pub fn try_recv(&self) -> Option<EventUpdate> {
        self.receiver.try_recv().ok()
    }

    /// Wakes a sleeping backoff immediately so the stream dials right away.
    pub fn reconnect_now(&self) {
        let (lock, cvar) = &*self.reconnect_wake;
        if let Ok(mut woken) = lock.lock() {
            *woken = true;
            cvar.notify_all();
        }
    }

    /// Requests that the background thread stop; the thread is not joined.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let (lock, cvar) = &*self.reconnect_wake;
        if let Ok(mut woken) = lock.lock() {
            *woken = true;
            cvar.notify_all();
        }
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
    overflow: bool,
}

impl SseParser {
    /// Creates an empty parser.
    pub fn new() -> Self {
        Self::default()
    }

    fn reset_frame(&mut self) {
        self.event = None;
        self.data.clear();
        self.data_seen = false;
        self.overflow = false;
    }

    /// Feed a decoded chunk and return every frame completed by a blank line.
    pub fn feed(&mut self, chunk: &str) -> Vec<SseFrame> {
        let mut frames = Vec::new();
        if self.buffer.len().saturating_add(chunk.len()) > MAX_SSE_BUFFER {
            // A peer must not be able to grow this buffer without bound.
            self.buffer.clear();
            self.reset_frame();
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
            if self.overflow || self.data.is_empty() {
                self.reset_frame();
            } else {
                frames.push(SseFrame {
                    id: self.last_id,
                    event: self.event.take(),
                    data: std::mem::take(&mut self.data),
                });
                self.data_seen = false;
            }
            return;
        }
        if self.overflow || line.starts_with(':') {
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
                let extra = usize::from(self.data_seen).saturating_add(value.len());
                if self.data.len().saturating_add(extra) > MAX_SSE_BUFFER {
                    // Drop oversized multi-line frames without OOMing or returning
                    // a truncated half-payload; parsing resumes at the next blank line.
                    self.event = None;
                    self.data.clear();
                    self.data_seen = false;
                    self.overflow = true;
                    return;
                }
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

/// One frame of the UI: 60 per second at most. The coalescer never hands out
/// frames closer together than this, so a burst of events costs at most 60
/// state writes a second however many children report.
pub const FRAME: Duration = Duration::from_micros(16_667);
/// Children whose counters are kept; the oldest is forgotten past this.
const MAX_CHILDREN: usize = 64;

/// What one subagent (child task) did since its counters were last read.
/// Progress is reduced to numbers: the UI shows "step 7" and a spinner, not a
/// row per event, so 50 children cost 50 counters, not 50 event streams.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubagentProgress {
    /// Progress events folded in.
    pub events: u32,
    /// Of those, steps that finished.
    pub steps_done: u32,
    /// Newest event id folded in.
    pub last_event_id: i64,
}

/// Events gathered for one frame.
#[derive(Debug, Default, PartialEq)]
pub struct EventFrame {
    /// Events the UI must see one by one, in order: `(id, kind, payload)`.
    pub events: Vec<(i64, String, Value)>,
    /// Folded child progress, by child task id.
    pub progress: Vec<(String, SubagentProgress)>,
}

/// Batches stream events into [`FRAME`]-sized frames and folds per-subagent
/// progress into counters.
#[derive(Debug, Default)]
pub struct FrameCoalescer {
    events: Vec<(i64, String, Value)>,
    progress: std::collections::BTreeMap<String, SubagentProgress>,
    children: std::collections::VecDeque<String>,
    last_frame: Option<Instant>,
}

/// The child task id of a progress event, if `kind` is one. `known` tells
/// whether a task id was already seen as a child.
fn progress_child(kind: &str, payload: &Value, known: impl Fn(&str) -> bool) -> Option<String> {
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    match kind {
        "handoff.queued" | "handoff.started" | "handoff.resumed" => text("child_task_id"),
        "turn.step" => {
            let task = text("task_id")?;
            (payload.get("parent_task_id").is_some() || known(&task)).then_some(task)
        }
        _ => None,
    }
}

impl FrameCoalescer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one stream event. Child progress becomes a counter; anything else
    /// waits for the next frame unchanged. A handoff starting, queued or
    /// resumed does both: it folds into the child's counter AND stays an
    /// event, so the runs board sees the start (not only the completion) and
    /// refreshes the handoff ledger while the child is still working.
    pub fn push(&mut self, id: i64, kind: String, payload: Value) {
        let is_handoff_start = matches!(
            kind.as_str(),
            "handoff.queued" | "handoff.started" | "handoff.resumed"
        );
        let child = progress_child(&kind, &payload, |task| {
            self.children.iter().any(|c| c == task)
        });
        let Some(child) = child else {
            self.events.push((id, kind, payload));
            return;
        };
        if !self.children.iter().any(|known| *known == child) {
            if self.children.len() >= MAX_CHILDREN
                && let Some(oldest) = self.children.pop_front()
            {
                self.progress.remove(&oldest);
            }
            self.children.push_back(child.clone());
        }
        let counter = self.progress.entry(child).or_default();
        counter.events = counter.events.saturating_add(1);
        if kind == "turn.step" && payload.get("phase").and_then(Value::as_str) == Some("done") {
            counter.steps_done = counter.steps_done.saturating_add(1);
        }
        counter.last_event_id = counter.last_event_id.max(id);
        if is_handoff_start {
            self.events.push((id, kind, payload));
        }
    }

    /// Whether anything waits for a frame.
    pub fn is_pending(&self) -> bool {
        !self.events.is_empty() || !self.progress.is_empty()
    }

    /// The next frame, if something waits and a frame length passed since the
    /// last one. Counters are drained: each frame carries what is new.
    pub fn take_frame(&mut self, now: Instant) -> Option<EventFrame> {
        if !self.is_pending() {
            return None;
        }
        if self
            .last_frame
            .is_some_and(|last| now.saturating_duration_since(last) < FRAME)
        {
            return None;
        }
        self.last_frame = Some(now);
        Some(EventFrame {
            events: std::mem::take(&mut self.events),
            progress: std::mem::take(&mut self.progress).into_iter().collect(),
        })
    }

    /// Drops everything waiting (stream replaced or session cleared).
    pub fn clear(&mut self) {
        *self = Self::default();
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
    reconnect_wake: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
) {
    let Some(client) = build_client(&credentials.endpoint) else {
        emit(&sender, &*wake, EventUpdate::Disconnected);
        return;
    };
    let mut cursor = after;
    let mut backoff = 0;
    let mut rejected_once = false;
    while !stop.load(Ordering::SeqCst) {
        match stream_once(
            &client,
            &credentials,
            &mut cursor,
            &sender,
            &*wake,
            &stop,
            &mut backoff,
            IDLE_TIMEOUT,
        ) {
            StreamEnd::Unauthorized if !rejected_once => {
                // A proxy or auth hiccup can answer 401 once. Look again
                // before ending the session for good.
                rejected_once = true;
                if !sleep_backoff(
                    &stop,
                    &reconnect_wake,
                    crate::net_policy::jitter(Duration::from_secs(2)),
                ) {
                    return;
                }
            }
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
                let pause = crate::net_policy::jitter(BACKOFF[backoff.min(BACKOFF.len() - 1)]);
                if !sleep_backoff(&stop, &reconnect_wake, pause) {
                    return;
                }
                backoff = (backoff + 1).min(BACKOFF.len() - 1);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stream_once(
    client: &Client,
    credentials: &Credentials,
    cursor: &mut i64,
    sender: &Sender<EventUpdate>,
    wake: &(dyn Fn() + Send),
    stop: &AtomicBool,
    backoff: &mut usize,
    idle: Duration,
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
    if !emit(sender, wake, EventUpdate::Connected) {
        return StreamEnd::ReceiverGone;
    }

    // A blocking read cannot time out, so a helper thread does the reading and
    // this loop watches the clock. A stalled helper ends when its socket dies
    // (TCP keep-alive is on) or when this receiver is dropped.
    let (chunks, incoming) = mpsc::sync_channel::<ReadMsg>(16);
    let reader_stop = Arc::new(AtomicBool::new(false));
    let helper_stop = Arc::clone(&reader_stop);
    let spawned = std::thread::Builder::new()
        .name("serabot-events-read".into())
        .spawn(move || {
            let mut reader = BufReader::with_capacity(READ_CHUNK, response);
            let mut chunk = [0u8; READ_CHUNK];
            while !helper_stop.load(Ordering::SeqCst) {
                let message = match reader.read(&mut chunk) {
                    Ok(0) => ReadMsg::Eof,
                    Ok(read) => ReadMsg::Data(chunk[..read].to_vec()),
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => ReadMsg::Eof,
                };
                let last = !matches!(message, ReadMsg::Data(_));
                if chunks.send(message).is_err() || last {
                    return;
                }
            }
        });
    if spawned.is_err() {
        return StreamEnd::Disconnected;
    }
    let mut decoder = Utf8Decoder::new();
    let mut parser = SseParser::new();
    let mut last_bytes = Instant::now();
    let end = loop {
        if stop.load(Ordering::SeqCst) {
            break StreamEnd::Stopped;
        }
        let remaining = idle.saturating_sub(last_bytes.elapsed());
        if remaining.is_zero() {
            break StreamEnd::Disconnected;
        }
        match incoming.recv_timeout(remaining.min(STOP_POLL)) {
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) | Ok(ReadMsg::Eof) => {
                break StreamEnd::Disconnected;
            }
            Ok(ReadMsg::Data(bytes)) => {
                last_bytes = Instant::now();
                // Only reset reconnect backoff once the peer delivers bytes, so an
                // immediate HTTP 200 + EOF loop keeps backing off.
                *backoff = 0;
                let text = decoder.push(&bytes);
                if text.is_empty() {
                    continue;
                }
                let mut gone = false;
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
                        gone = true;
                        break;
                    }
                }
                if gone {
                    break StreamEnd::ReceiverGone;
                }
            }
        }
    };
    reader_stop.store(true, Ordering::SeqCst);
    end
}

/// What the reader thread hands to the watchdog loop.
enum ReadMsg {
    Data(Vec<u8>),
    Eof,
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
    } else {
        // A fresh start begins at the newest event; current state comes from
        // the normal REST loads, not from replaying retained history.
        query.append_pair("after", "latest");
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
        // Lets the OS notice a dead peer so a stalled reader thread ends.
        .tcp_keepalive(Duration::from_secs(30))
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

fn sleep_backoff(
    stop: &AtomicBool,
    reconnect_wake: &(std::sync::Mutex<bool>, std::sync::Condvar),
    mut remaining: Duration,
) -> bool {
    let (lock, cvar) = reconnect_wake;
    while !remaining.is_zero() {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let step = remaining.min(STOP_POLL);
        if let Ok(mut woken) = lock.lock() {
            if *woken {
                *woken = false;
                return !stop.load(Ordering::SeqCst);
            }
            let (mut guard, _) = cvar
                .wait_timeout(woken, step)
                .unwrap_or_else(|e| e.into_inner());
            if *guard {
                *guard = false;
                return !stop.load(Ordering::SeqCst);
            }
        } else {
            std::thread::sleep(step);
        }
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
    #[test]
    fn frames_are_batched_and_child_progress_folds_into_counters() {
        let mut co = FrameCoalescer::new();
        let t0 = Instant::now();
        co.push(
            1,
            "handoff.started".into(),
            serde_json::json!({"child_task_id": "k1"}),
        );
        co.push(
            2,
            "turn.step".into(),
            serde_json::json!({"task_id": "k1", "phase": "done"}),
        );
        co.push(
            3,
            "turn.completed".into(),
            serde_json::json!({"task_id": "p"}),
        );
        let frame = co.take_frame(t0).expect("first frame is immediate");
        assert_eq!(
            frame.events.len(),
            2,
            "the handoff start stays an event (the runs board needs it) as well as folding into progress"
        );
        assert_eq!(frame.progress.len(), 1);
        assert_eq!(frame.progress[0].1.events, 2);
        assert_eq!(frame.progress[0].1.steps_done, 1);
        // A second burst inside the frame length waits.
        co.push(4, "task.updated".into(), serde_json::json!({}));
        assert!(co.take_frame(t0 + Duration::from_millis(5)).is_none());
        assert!(co.take_frame(t0 + FRAME).is_some());
        assert!(
            co.take_frame(t0 + FRAME * 3).is_none(),
            "nothing pending, no frame"
        );
    }

    #[test]
    fn a_parents_own_steps_are_not_folded() {
        let mut co = FrameCoalescer::new();
        co.push(
            1,
            "turn.step".into(),
            serde_json::json!({"task_id": "p", "phase": "running"}),
        );
        let frame = co.take_frame(Instant::now()).unwrap();
        assert_eq!(frame.events.len(), 1);
        assert!(frame.progress.is_empty());
    }

    #[test]
    fn child_counters_are_bounded() {
        let mut co = FrameCoalescer::new();
        for n in 0..(MAX_CHILDREN as i64 + 20) {
            co.push(
                n,
                "handoff.started".into(),
                serde_json::json!({"child_task_id": format!("k{n}")}),
            );
        }
        let frame = co.take_frame(Instant::now()).unwrap();
        assert!(frame.progress.len() <= MAX_CHILDREN);
    }

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
                    assert!(request.contains("after=latest"), "{request}");
                    assert!(request.contains("limit=100"), "{request}");
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
        // The first 401 is re-checked once; the second ends the session.
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                assert!(
                    request.to_ascii_lowercase().contains(
                        "authorization: bearer synthetic-unauthorized-token-0123456789\r\n"
                    ),
                    "{request}"
                );
                stream
                    .write_all(
                        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
                stream.flush().unwrap();
            }
        });

        let credentials = Arc::new(Credentials {
            endpoint,
            token: secrecy::SecretString::new(token.into()),
        });
        let stream = EventStream::start(credentials, 0, Box::new(|| {}));
        let deadline = Instant::now() + Duration::from_secs(8);
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

    #[test]
    fn parser_caps_accumulated_data_across_chunks_and_recovers() {
        let mut parser = SseParser::new();
        let line = format!("data: {}\n", "a".repeat(16 * 1024));
        for _ in 0..=20 {
            assert!(parser.feed(&line).is_empty());
        }
        assert!(parser.feed("\n").is_empty(), "oversized frame is dropped");
        let frames = parser.feed("id: 12\ndata: {\"ok\":true}\n\n");
        assert_eq!(
            frames,
            vec![SseFrame {
                id: Some(12),
                event: None,
                data: "{\"ok\":true}".into(),
            }]
        );
    }

    #[test]
    fn immediate_eof_after_200_preserves_backoff() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint =
            crate::validate_endpoint(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            stream.flush().unwrap();
        });
        let credentials = Credentials {
            endpoint,
            token: secrecy::SecretString::new("synthetic-eof-token-0123456789abcdef".into()),
        };
        let client = build_client(&credentials.endpoint).unwrap();
        let (tx, _rx) = mpsc::channel();
        let stop = AtomicBool::new(false);
        let mut cursor = 0;
        let mut backoff = 3;
        let end = stream_once(
            &client,
            &credentials,
            &mut cursor,
            &tx,
            &|| {},
            &stop,
            &mut backoff,
            IDLE_TIMEOUT,
        );
        server.join().unwrap();
        assert!(matches!(end, StreamEnd::Disconnected));
        assert_eq!(backoff, 3);
    }

    #[test]
    fn a_silent_stream_is_dropped_by_the_idle_watchdog() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint =
            crate::validate_endpoint(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
        let (release, held) = mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK
Content-Type: text/event-stream

: hello

",
                )
                .unwrap();
            stream.flush().unwrap();
            // Hold the socket open without sending anything, like a dead NAT.
            let _ = held.recv_timeout(Duration::from_secs(10));
        });
        let credentials = Credentials {
            endpoint,
            token: secrecy::SecretString::new("synthetic-idle-token-0123456789abcdef".into()),
        };
        let client = build_client(&credentials.endpoint).unwrap();
        let (tx, rx) = mpsc::channel();
        let stop = AtomicBool::new(false);
        let mut cursor = 0;
        let mut backoff = 0;
        let started = Instant::now();
        let end = stream_once(
            &client,
            &credentials,
            &mut cursor,
            &tx,
            &|| {},
            &stop,
            &mut backoff,
            Duration::from_millis(600),
        );
        let waited = started.elapsed();
        let _ = release.send(());
        server.join().unwrap();
        assert!(matches!(end, StreamEnd::Disconnected));
        assert!(waited < Duration::from_secs(5), "{waited:?}");
        assert_eq!(rx.try_recv().ok(), Some(EventUpdate::Connected));
    }

    #[test]
    fn sleep_backoff_wakes_immediately_on_reconnect_signal() {
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let wake_thread = Arc::clone(&wake);
        let stop_thread = Arc::clone(&stop);
        let started = Instant::now();
        let handle = std::thread::spawn(move || {
            sleep_backoff(&stop_thread, &wake_thread, Duration::from_secs(30))
        });
        std::thread::sleep(Duration::from_millis(40));
        {
            let (lock, cvar) = &*wake;
            let mut guard = lock.lock().unwrap();
            *guard = true;
            cvar.notify_all();
        }
        let completed = handle.join().unwrap();
        assert!(completed);
        assert!(started.elapsed() < Duration::from_secs(2));
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
