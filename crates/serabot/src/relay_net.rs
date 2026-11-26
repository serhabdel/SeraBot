//! Desktop network relay (0.66, `docs/specs/desktop-network-relay.md`).
//!
//! A member can let chosen providers' model traffic leave through their own
//! desktop instead of the server's network, while the desktop app is open and
//! the switch is on. The shape:
//!
//! ```text
//! provider client ──CONNECT host:443──▶ 127.0.0.1:<port> (this module)
//!                                          │ frames over the desktop's
//!                                          │ long-poll exchange
//!                                          ▼
//!                       desktop opens TCP to host:443 and moves bytes
//! ```
//!
//! TLS runs end to end between the server and the provider, so the desktop
//! only ever carries encrypted bytes. Limits:
//!
//! - **Opt-in per member and per provider.** Off by default.
//! - **Fixed hosts.** A tunnel may only reach the hosts listed for a provider
//!   the member turned on, on port 443. The desktop also refuses private
//!   addresses after its own DNS lookup.
//! - **Loopback and a secret.** The listener binds 127.0.0.1 and each member's
//!   proxy carries a random token in `Proxy-Authorization`.
//! - **Bounded.** At most [`MAX_STREAMS`] open tunnels per member; idle
//!   tunnels close. When the desktop is not connected, a tunnel is refused at
//!   once, so the request fails fast and the route chain fails over.
//! - **Audited.** Each tunnel is recorded (host, bytes, duration; never
//!   content).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::error::{Result, SeraError};
use crate::store::Store;

/// Providers whose traffic may go through the desktop, and the hosts each
/// may reach. Port 443 only.
pub const RELAY_PROVIDERS: &[(&str, &str, &[&str])] = &[
    (
        "google-antigravity",
        "Google Antigravity",
        &[
            "cloudcode-pa.googleapis.com",
            "daily-cloudcode-pa.googleapis.com",
            "daily-cloudcode-pa.sandbox.googleapis.com",
            "autopush-cloudcode-pa.sandbox.googleapis.com",
            "businessaicode.googleapis.com",
            "businessaicode.us.rep.googleapis.com",
            "businessaicode.eu.rep.googleapis.com",
        ],
    ),
    (
        "gemini",
        "Google Gemini API",
        &["generativelanguage.googleapis.com"],
    ),
    ("anthropic", "Anthropic", &["api.anthropic.com"]),
    ("openai", "OpenAI", &["api.openai.com"]),
    ("inception", "Inception Labs", &["api.inceptionlabs.ai"]),
    (
        "kiro",
        "Kiro",
        &[
            "oidc.us-east-1.amazonaws.com",
            "q.us-east-1.amazonaws.com",
            "codewhisperer.us-east-1.amazonaws.com",
            "oidc.us-west-2.amazonaws.com",
            "q.us-west-2.amazonaws.com",
            "oidc.eu-central-1.amazonaws.com",
            "q.eu-central-1.amazonaws.com",
            "oidc.eu-west-1.amazonaws.com",
            "q.eu-west-1.amazonaws.com",
            "oidc.ap-southeast-1.amazonaws.com",
            "q.ap-southeast-1.amazonaws.com",
            "oidc.ap-northeast-1.amazonaws.com",
            "q.ap-northeast-1.amazonaws.com",
        ],
    ),
    ("devin", "Devin", &["api.devin.ai", "server.codeium.com"]),
];

/// Open tunnels per member, at most.
pub const MAX_STREAMS: usize = 16;
/// A desktop counts as connected for this long after its last exchange.
const ONLINE_FOR: Duration = Duration::from_secs(30);
/// How long a long-poll exchange waits for frames.
const EXCHANGE_WAIT: Duration = Duration::from_secs(20);
/// How long the desktop has to open a tunnel.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// A tunnel with no traffic either way closes after this.
const IDLE_CLOSE: Duration = Duration::from_secs(120);
/// Bytes per data frame.
const CHUNK: usize = 32 * 1024;
/// Bytes handed to the desktop in one exchange, at most.
const EXCHANGE_BYTES: usize = 1024 * 1024;

/// One message between the server and the desktop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    /// Server → desktop: open TCP to `host:port`.
    Open { id: u64, host: String, port: u16 },
    /// Desktop → server: the TCP connection is up.
    Opened { id: u64 },
    /// Either way: bytes, base64.
    Data { id: u64, b64: String },
    /// Either way: the stream ended.
    Close {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl Frame {
    fn size(&self) -> usize {
        match self {
            Self::Data { b64, .. } => b64.len(),
            _ => 64,
        }
    }
}

/// A member's relay settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaySettings {
    pub enabled: bool,
    pub providers: Vec<String>,
}

impl RelaySettings {
    /// Keep only known providers, without duplicates.
    pub fn normalized(mut self) -> Self {
        self.providers
            .retain(|p| RELAY_PROVIDERS.iter().any(|(kind, _, _)| kind == p));
        self.providers.sort();
        self.providers.dedup();
        self
    }

    /// Whether `kind`'s traffic goes through the desktop.
    pub fn routes(&self, kind: &str) -> bool {
        self.enabled && self.providers.iter().any(|p| p == kind)
    }

    /// Whether a tunnel to `host` is allowed under these settings.
    pub fn allows_host(&self, host: &str) -> bool {
        self.enabled
            && RELAY_PROVIDERS.iter().any(|(kind, _, hosts)| {
                self.providers.iter().any(|p| p == kind)
                    && hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
            })
    }
}

impl Store {
    pub fn relay_settings(&self, user_id: &str) -> Result<RelaySettings> {
        let connection = self.connection()?;
        let row: Option<(i64, String)> = connection
            .query_row(
                "SELECT enabled, providers_json FROM relay_settings WHERE user_id = ?1",
                params![user_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(match row {
            Some((enabled, providers)) => RelaySettings {
                enabled: enabled != 0,
                providers: serde_json::from_str(&providers).unwrap_or_default(),
            }
            .normalized(),
            None => RelaySettings::default(),
        })
    }

    pub fn set_relay_settings(&self, user_id: &str, settings: &RelaySettings) -> Result<()> {
        let settings = settings.clone().normalized();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO relay_settings(user_id, enabled, providers_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO UPDATE SET enabled = excluded.enabled,
                 providers_json = excluded.providers_json, updated_at = excluded.updated_at",
            params![
                user_id,
                settings.enabled as i64,
                serde_json::to_string(&settings.providers)?,
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }
}

struct StreamEnd {
    to_server: mpsc::Sender<Vec<u8>>,
    opened: Option<oneshot::Sender<std::result::Result<(), String>>>,
}

#[derive(Default)]
struct UserRelay {
    token: String,
    last_seen: Option<Instant>,
    outbound: VecDeque<Frame>,
    streams: HashMap<u64, StreamEnd>,
    next_id: u64,
    bytes_up: u64,
    bytes_down: u64,
}

/// Live relay status for the console.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct RelayStatus {
    pub online: bool,
    pub streams: usize,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

/// Every member's relay state, and the loopback listener.
pub struct RelayHub {
    store: Store,
    users: Mutex<HashMap<String, UserRelay>>,
    notify: Mutex<HashMap<String, Arc<Notify>>>,
    port: OnceLock<u16>,
}

static HUB: OnceLock<Arc<RelayHub>> = OnceLock::new();

/// The process-wide hub, once [`RelayHub::install`] ran.
pub fn hub() -> Option<Arc<RelayHub>> {
    HUB.get().cloned()
}

/// The proxy a provider client of `user_id` uses for `kind`, when the member
/// turned the relay on for it. `None` keeps the server's own network.
pub fn proxy_for(user_id: &str, kind: &str) -> Option<reqwest::Proxy> {
    let hub = hub()?;
    let settings = hub.store.relay_settings(user_id).ok()?;
    if !settings.routes(kind) {
        return None;
    }
    let port = *hub.port.get()?;
    let token = hub.token_for(user_id);
    reqwest::Proxy::https(format!("http://127.0.0.1:{port}"))
        .ok()
        .map(|proxy| proxy.basic_auth("serabot", &token))
}

/// Whether `kind`'s traffic for `user_id` goes through the desktop right
/// now: the member turned it on and the desktop is connected (0.67, for the
/// Usage tab).
pub fn relayed(user_id: &str, kind: &str) -> bool {
    hub().is_some_and(|hub| {
        hub.online(user_id)
            && hub
                .store
                .relay_settings(user_id)
                .is_ok_and(|settings| settings.routes(kind))
    })
}

fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    crate::util::sha256_hex(bytes)
}

impl RelayHub {
    pub fn new(store: Store) -> Arc<Self> {
        Arc::new(Self {
            store,
            users: Mutex::new(HashMap::new()),
            notify: Mutex::new(HashMap::new()),
            port: OnceLock::new(),
        })
    }

    /// Bind the loopback listener, serve it in the background, and make the
    /// hub the process-wide one.
    pub async fn install(store: Store) -> Result<Arc<Self>> {
        let hub = Self::new(store);
        hub.listen().await?;
        let _ = HUB.set(hub.clone());
        Ok(hub)
    }

    /// Bind 127.0.0.1 on a free port and accept tunnels.
    pub async fn listen(self: &Arc<Self>) -> Result<u16> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| SeraError::Internal(format!("relay listener: {error}")))?;
        let port = listener
            .local_addr()
            .map_err(|error| SeraError::Internal(format!("relay listener: {error}")))?
            .port();
        let _ = self.port.set(port);
        let hub = self.clone();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    continue;
                };
                let hub = hub.clone();
                tokio::spawn(async move {
                    if let Err(error) = hub.serve_connect(socket).await {
                        tracing::debug!(%error, "relay tunnel ended");
                    }
                });
            }
        });
        Ok(port)
    }

    pub fn port(&self) -> Option<u16> {
        self.port.get().copied()
    }

    fn token_for(&self, user_id: &str) -> String {
        let mut users = self.users.lock().unwrap_or_else(|p| p.into_inner());
        let entry = users.entry(user_id.to_string()).or_default();
        if entry.token.is_empty() {
            entry.token = random_token();
        }
        entry.token.clone()
    }

    fn user_for_token(&self, token: &str) -> Option<String> {
        if token.is_empty() {
            return None;
        }
        let users = self.users.lock().unwrap_or_else(|p| p.into_inner());
        users
            .iter()
            .find(|(_, relay)| !relay.token.is_empty() && relay.token == token)
            .map(|(user, _)| user.clone())
    }

    fn notifier(&self, user_id: &str) -> Arc<Notify> {
        self.notify
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(user_id.to_string())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    fn push(&self, user_id: &str, frame: Frame) {
        {
            let mut users = self.users.lock().unwrap_or_else(|p| p.into_inner());
            let relay = users.entry(user_id.to_string()).or_default();
            if let Frame::Data { b64, .. } = &frame {
                relay.bytes_down += b64.len() as u64 * 3 / 4;
            }
            relay.outbound.push_back(frame);
        }
        self.notifier(user_id).notify_one();
    }

    /// Whether the member's desktop exchanged frames recently.
    pub fn online(&self, user_id: &str) -> bool {
        self.users
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(user_id)
            .and_then(|relay| relay.last_seen)
            .is_some_and(|seen| seen.elapsed() < ONLINE_FOR)
    }

    pub fn status(&self, user_id: &str) -> RelayStatus {
        let users = self.users.lock().unwrap_or_else(|p| p.into_inner());
        let Some(relay) = users.get(user_id) else {
            return RelayStatus::default();
        };
        RelayStatus {
            online: relay
                .last_seen
                .is_some_and(|seen| seen.elapsed() < ONLINE_FOR),
            streams: relay.streams.len(),
            bytes_up: relay.bytes_up,
            bytes_down: relay.bytes_down,
        }
    }

    /// One exchange with the member's desktop: apply its frames, then return
    /// the frames waiting for it (waiting up to 20 s for some when `wait`).
    pub async fn exchange(&self, user_id: &str, inbound: Vec<Frame>, wait: bool) -> Vec<Frame> {
        {
            let mut users = self.users.lock().unwrap_or_else(|p| p.into_inner());
            let relay = users.entry(user_id.to_string()).or_default();
            relay.last_seen = Some(Instant::now());
        }
        for frame in inbound {
            self.apply_inbound(user_id, frame).await;
        }
        let notify = self.notifier(user_id);
        let deadline = Instant::now() + EXCHANGE_WAIT;
        loop {
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let out = self.take_outbound(user_id);
            if !out.is_empty() || !wait {
                return out;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Vec::new();
            }
            let _ = tokio::time::timeout(left, notified).await;
            let mut users = self.users.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(relay) = users.get_mut(user_id) {
                relay.last_seen = Some(Instant::now());
            }
        }
    }

    fn take_outbound(&self, user_id: &str) -> Vec<Frame> {
        let mut users = self.users.lock().unwrap_or_else(|p| p.into_inner());
        let Some(relay) = users.get_mut(user_id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut size = 0;
        while let Some(frame) = relay.outbound.front() {
            if !out.is_empty() && size + frame.size() > EXCHANGE_BYTES {
                break;
            }
            size += frame.size();
            out.push(relay.outbound.pop_front().expect("front exists"));
        }
        out
    }

    async fn apply_inbound(&self, user_id: &str, frame: Frame) {
        match frame {
            Frame::Opened { id } => {
                let opened = {
                    let mut users = self.users.lock().unwrap_or_else(|p| p.into_inner());
                    users
                        .get_mut(user_id)
                        .and_then(|relay| relay.streams.get_mut(&id))
                        .and_then(|stream| stream.opened.take())
                };
                if let Some(opened) = opened {
                    let _ = opened.send(Ok(()));
                }
            }
            Frame::Data { id, b64 } => {
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.as_bytes())
                else {
                    return;
                };
                let sender = {
                    let mut users = self.users.lock().unwrap_or_else(|p| p.into_inner());
                    let Some(relay) = users.get_mut(user_id) else {
                        return;
                    };
                    relay.bytes_up += bytes.len() as u64;
                    relay
                        .streams
                        .get(&id)
                        .map(|stream| stream.to_server.clone())
                };
                if let Some(sender) = sender {
                    let _ = tokio::time::timeout(Duration::from_secs(10), sender.send(bytes)).await;
                }
            }
            Frame::Close { id, error } => {
                let removed = {
                    let mut users = self.users.lock().unwrap_or_else(|p| p.into_inner());
                    users
                        .get_mut(user_id)
                        .and_then(|relay| relay.streams.remove(&id))
                };
                if let Some(mut stream) = removed
                    && let Some(opened) = stream.opened.take()
                {
                    let _ = opened.send(Err(error.unwrap_or_else(|| "closed".into())));
                }
            }
            // The desktop never asks the server to open anything.
            Frame::Open { .. } => {}
        }
    }

    /// One CONNECT from a provider client on this host.
    async fn serve_connect(self: Arc<Self>, mut socket: TcpStream) -> Result<()> {
        let head = read_head(&mut socket).await?;
        let request = match parse_connect(&head) {
            Ok(request) => request,
            Err(status) => {
                let _ = socket.write_all(status.as_bytes()).await;
                return Ok(());
            }
        };
        let Some(user_id) = self.user_for_token(&request.token) else {
            socket
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .ok();
            return Ok(());
        };
        let settings = self.store.relay_settings(&user_id)?;
        if request.port != 443 || !settings.allows_host(&request.host) {
            socket
                .write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n")
                .await
                .ok();
            return Ok(());
        }
        if !self.online(&user_id) {
            socket
                .write_all(b"HTTP/1.1 502 Desktop Offline\r\n\r\n")
                .await
                .ok();
            return Ok(());
        }
        let (to_server, mut from_desktop) = mpsc::channel::<Vec<u8>>(64);
        let (opened_tx, opened_rx) = oneshot::channel();
        let id = {
            let mut users = self.users.lock().unwrap_or_else(|p| p.into_inner());
            let relay = users.entry(user_id.clone()).or_default();
            if relay.streams.len() >= MAX_STREAMS {
                None
            } else {
                relay.next_id += 1;
                let id = relay.next_id;
                relay.streams.insert(
                    id,
                    StreamEnd {
                        to_server,
                        opened: Some(opened_tx),
                    },
                );
                Some(id)
            }
        };
        let Some(id) = id else {
            socket
                .write_all(b"HTTP/1.1 429 Too Many Tunnels\r\n\r\n")
                .await
                .ok();
            return Ok(());
        };
        let started = Instant::now();
        self.push(
            &user_id,
            Frame::Open {
                id,
                host: request.host.clone(),
                port: request.port,
            },
        );
        match tokio::time::timeout(OPEN_TIMEOUT, opened_rx).await {
            Ok(Ok(Ok(()))) => {}
            _ => {
                self.end_stream(&user_id, id, Some("the desktop could not open it"));
                socket
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                    .await
                    .ok();
                return Ok(());
            }
        }
        socket
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .map_err(|error| SeraError::External(error.to_string()))?;
        let (mut reader, mut writer) = socket.into_split();
        let hub = self.clone();
        let user = user_id.clone();
        let upload = tokio::spawn(async move {
            let mut buffer = vec![0u8; CHUNK];
            let mut sent = 0u64;
            loop {
                match tokio::time::timeout(IDLE_CLOSE, reader.read(&mut buffer)).await {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(n)) => {
                        sent += n as u64;
                        hub.push(
                            &user,
                            Frame::Data {
                                id,
                                b64: base64::engine::general_purpose::STANDARD.encode(&buffer[..n]),
                            },
                        );
                    }
                }
            }
            sent
        });
        let mut received = 0u64;
        while let Ok(Some(bytes)) = tokio::time::timeout(IDLE_CLOSE, from_desktop.recv()).await {
            received += bytes.len() as u64;
            if writer.write_all(&bytes).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
        upload.abort();
        let sent = upload.await.unwrap_or(0);
        self.end_stream(&user_id, id, None);
        if let Err(error) = self.store.record_audit(
            &user_id,
            "relay.tunnel",
            Some(&request.host),
            None,
            None,
            None,
            None,
            Some(&serde_json::json!({
                "host": request.host,
                "bytes_to_provider": sent,
                "bytes_from_provider": received,
                "seconds": started.elapsed().as_secs(),
            })),
        ) {
            tracing::warn!(%error, "relay audit not recorded");
        }
        Ok(())
    }

    fn end_stream(&self, user_id: &str, id: u64, error: Option<&str>) {
        let existed = {
            let mut users = self.users.lock().unwrap_or_else(|p| p.into_inner());
            users
                .get_mut(user_id)
                .and_then(|relay| relay.streams.remove(&id))
                .is_some()
        };
        if existed {
            self.push(
                user_id,
                Frame::Close {
                    id,
                    error: error.map(str::to_owned),
                },
            );
        }
    }
}

struct ConnectRequest {
    host: String,
    port: u16,
    token: String,
}

async fn read_head(socket: &mut TcpStream) -> Result<String> {
    let mut head = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    let read = async {
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() > 8 * 1024 {
                return Err(SeraError::Validation("proxy request head too large".into()));
            }
            let n = socket
                .read(&mut byte)
                .await
                .map_err(|error| SeraError::External(error.to_string()))?;
            if n == 0 {
                return Err(SeraError::External("closed before CONNECT".into()));
            }
            head.push(byte[0]);
        }
        Ok(())
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .map_err(|_| SeraError::External("CONNECT timed out".into()))??;
    Ok(String::from_utf8_lossy(&head).into_owned())
}

/// Parse `CONNECT host:port HTTP/1.1` and the proxy token. An error is the
/// status line to send back.
fn parse_connect(head: &str) -> std::result::Result<ConnectRequest, &'static str> {
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    if parts.next() != Some("CONNECT") {
        return Err("HTTP/1.1 405 Method Not Allowed\r\n\r\n");
    }
    let target = parts.next().unwrap_or_default();
    let Some((host, port)) = target.rsplit_once(':') else {
        return Err("HTTP/1.1 400 Bad Request\r\n\r\n");
    };
    let Ok(port) = port.parse::<u16>() else {
        return Err("HTTP/1.1 400 Bad Request\r\n\r\n");
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if host.is_empty() || host.len() > 253 {
        return Err("HTTP/1.1 400 Bad Request\r\n\r\n");
    }
    let mut token = String::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("proxy-authorization") {
            continue;
        }
        let value = value.trim();
        let Some(encoded) = value
            .strip_prefix("Basic ")
            .or_else(|| value.strip_prefix("basic "))
        else {
            continue;
        };
        if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim())
            && let Ok(text) = String::from_utf8(decoded)
            && let Some((_, password)) = text.split_once(':')
        {
            token = password.to_string();
        }
    }
    Ok(ConnectRequest { host, port, token })
}

/// Whether an address may be reached by a desktop tunnel: public unicast
/// only. Shared with the desktop, which applies it after its own DNS lookup.
pub fn public_address(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..=127).contains(&b)))
        }
        std::net::IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| !public_address(std::net::IpAddr::V4(v4))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_user() -> (Store, String) {
        let store = Store::open_in_memory(8).unwrap();
        let user = store.admin_user_id().unwrap();
        (store, user)
    }

    #[test]
    fn settings_are_per_member_normalized_and_off_by_default() {
        let (store, admin) = store_with_user();
        assert_eq!(
            store.relay_settings(&admin).unwrap(),
            RelaySettings::default()
        );
        store
            .set_relay_settings(
                &admin,
                &RelaySettings {
                    enabled: true,
                    providers: vec![
                        "google-antigravity".into(),
                        "nope".into(),
                        "google-antigravity".into(),
                    ],
                },
            )
            .unwrap();
        let saved = store.relay_settings(&admin).unwrap();
        assert_eq!(saved.providers, vec!["google-antigravity".to_string()]);
        assert!(saved.routes("google-antigravity"));
        assert!(!saved.routes("openai"));
        assert!(saved.allows_host("daily-cloudcode-pa.googleapis.com"));
        assert!(saved.allows_host("businessaicode.googleapis.com"));
        assert!(saved.allows_host("businessaicode.us.rep.googleapis.com"));
        assert!(saved.allows_host("businessaicode.eu.rep.googleapis.com"));
        assert!(!saved.allows_host("api.openai.com"));
        assert!(!saved.allows_host("192.168.1.1"));
        let member = store
            .create_user("microsoft:relay-member", "r@example.example", "R")
            .unwrap()
            .id;
        assert!(!store.relay_settings(&member).unwrap().enabled);
    }

    #[test]
    fn kiro_and_devin_hosts_are_allowed_when_enabled() {
        let settings = RelaySettings {
            enabled: true,
            providers: vec!["kiro".into(), "devin".into(), "unknown".into()],
        }
        .normalized();
        assert_eq!(
            settings.providers,
            vec!["devin".to_string(), "kiro".to_string()]
        );
        for host in [
            "oidc.us-east-1.amazonaws.com",
            "q.us-east-1.amazonaws.com",
            "codewhisperer.us-east-1.amazonaws.com",
            "oidc.us-west-2.amazonaws.com",
            "q.us-west-2.amazonaws.com",
            "api.devin.ai",
            "server.codeium.com",
        ] {
            assert!(settings.allows_host(host), "{host}");
        }
        assert!(!settings.allows_host("api.openai.com"));
    }

    #[test]
    fn connect_heads_parse_and_refuse_garbage() {
        let auth = base64::engine::general_purpose::STANDARD.encode("serabot:tok");
        let head = format!(
            "CONNECT api.openai.com:443 HTTP/1.1\r\nHost: api.openai.com:443\r\nProxy-Authorization: Basic {auth}\r\n\r\n"
        );
        let parsed = parse_connect(&head).unwrap();
        assert_eq!(parsed.host, "api.openai.com");
        assert_eq!(parsed.port, 443);
        assert_eq!(parsed.token, "tok");
        assert!(parse_connect("GET / HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_connect("CONNECT nohost HTTP/1.1\r\n\r\n").is_err());
        assert_eq!(
            parse_connect("CONNECT a.b:443 HTTP/1.1\r\n\r\n")
                .unwrap()
                .token,
            ""
        );
    }

    #[test]
    fn only_public_addresses_are_reachable() {
        for private in [
            "10.0.0.1",
            "192.168.1.10",
            "172.16.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
        ] {
            assert!(!public_address(private.parse().unwrap()), "{private}");
        }
        for public in ["142.250.74.10", "2a00:1450:4007:80e::200a"] {
            assert!(public_address(public.parse().unwrap()), "{public}");
        }
    }

    /// End to end over loopback: a client CONNECTs through the hub, a fake
    /// desktop answers the frames, and bytes flow both ways.
    #[tokio::test]
    async fn a_tunnel_carries_bytes_both_ways_through_the_desktop() {
        let (store, admin) = store_with_user();
        store
            .set_relay_settings(
                &admin,
                &RelaySettings {
                    enabled: true,
                    providers: vec!["openai".into()],
                },
            )
            .unwrap();
        let hub = RelayHub::new(store);
        let port = hub.listen().await.unwrap();
        let token = hub.token_for(&admin);
        // The desktop is online.
        assert!(hub.exchange(&admin, Vec::new(), false).await.is_empty());

        // A desktop stand-in: echoes upper-cased bytes back.
        let desktop = {
            let hub = hub.clone();
            let admin = admin.clone();
            tokio::spawn(async move {
                let mut pending: Vec<Frame> = Vec::new();
                for _ in 0..20 {
                    let frames = hub
                        .exchange(&admin, std::mem::take(&mut pending), true)
                        .await;
                    for frame in frames {
                        match frame {
                            Frame::Open { id, host, port } => {
                                assert_eq!((host.as_str(), port), ("api.openai.com", 443));
                                pending.push(Frame::Opened { id });
                            }
                            Frame::Data { id, b64 } => {
                                let bytes = base64::engine::general_purpose::STANDARD
                                    .decode(b64)
                                    .unwrap();
                                pending.push(Frame::Data {
                                    id,
                                    b64: base64::engine::general_purpose::STANDARD
                                        .encode(bytes.to_ascii_uppercase()),
                                });
                                pending.push(Frame::Close { id, error: None });
                            }
                            Frame::Close { .. } => return,
                            Frame::Opened { .. } => {}
                        }
                    }
                    if !pending.is_empty() {
                        let done = pending.iter().any(|f| matches!(f, Frame::Close { .. }));
                        hub.exchange(&admin, std::mem::take(&mut pending), false)
                            .await;
                        if done {
                            return;
                        }
                    }
                }
            })
        };

        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("serabot:{token}"));
        client
            .write_all(
                format!(
                    "CONNECT api.openai.com:443 HTTP/1.1\r\nProxy-Authorization: Basic {auth}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut reply = vec![0u8; 64];
        let n = client.read(&mut reply).await.unwrap();
        assert!(String::from_utf8_lossy(&reply[..n]).starts_with("HTTP/1.1 200"));
        client.write_all(b"hello").await.unwrap();
        let mut echoed = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut echoed))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(echoed, b"HELLO");
        let _ = desktop.await;
        assert_eq!(hub.status(&admin).streams, 0);
    }

    #[tokio::test]
    async fn tunnels_are_refused_without_token_host_or_desktop() {
        let (store, admin) = store_with_user();
        store
            .set_relay_settings(
                &admin,
                &RelaySettings {
                    enabled: true,
                    providers: vec!["openai".into()],
                },
            )
            .unwrap();
        let hub = RelayHub::new(store);
        let port = hub.listen().await.unwrap();
        let token = hub.token_for(&admin);
        let ask = |host: &'static str, token: String| async move {
            let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let auth = base64::engine::general_purpose::STANDARD.encode(format!("serabot:{token}"));
            client
                .write_all(
                    format!("CONNECT {host} HTTP/1.1\r\nProxy-Authorization: Basic {auth}\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            let mut reply = vec![0u8; 64];
            let n = client.read(&mut reply).await.unwrap();
            String::from_utf8_lossy(&reply[..n]).into_owned()
        };
        assert!(
            ask("api.openai.com:443", "wrong".into())
                .await
                .contains("407")
        );
        assert!(ask("192.168.1.1:443", token.clone()).await.contains("403"));
        assert!(
            ask("api.openai.com:80", token.clone())
                .await
                .contains("403")
        );
        // Allowed, but the desktop never connected: refused at once.
        assert!(ask("api.openai.com:443", token).await.contains("502"));
    }
}
