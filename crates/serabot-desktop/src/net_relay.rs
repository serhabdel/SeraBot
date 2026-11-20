//! Desktop network relay, the PC side (0.66, `docs/specs/desktop-network-relay.md`).
//!
//! When the member turned the relay on, the server sends `open` frames for
//! provider hosts; this module opens the TCP connection from this PC and
//! moves the bytes both ways. TLS runs end to end between the server and the
//! provider, so only encrypted bytes pass through here.
//!
//! Checks made here, whatever the server says:
//! - the host is on this build's own list of provider hosts, port 443;
//! - every address the host resolves to is public (no LAN, loopback,
//!   link-local or carrier-grade NAT), and only those addresses are used.
//!
//! Two threads talk to the server: one long-polls for frames, one sends this
//! PC's frames as soon as there are some. A stopped relay closes every
//! connection.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::api::ApiClient;

/// Provider hosts this PC will open, port 443 only. Kept in step with the
/// server's list; a host missing here is refused even if the server asks.
pub const ALLOWED_HOSTS: &[&str] = &[
    "cloudcode-pa.googleapis.com",
    "daily-cloudcode-pa.googleapis.com",
    "daily-cloudcode-pa.sandbox.googleapis.com",
    "autopush-cloudcode-pa.sandbox.googleapis.com",
    "generativelanguage.googleapis.com",
    "api.anthropic.com",
    "api.openai.com",
];

const CHUNK: usize = 32 * 1024;
const MAX_STREAMS: usize = 16;

/// One message between the server and this PC (same shape as the server's).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    Open {
        id: u64,
        host: String,
        port: u16,
    },
    Opened {
        id: u64,
    },
    Data {
        id: u64,
        b64: String,
    },
    Close {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize)]
pub struct Exchange {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub frames: Vec<Frame>,
}

fn settings_client(credentials: &crate::worker::Credentials) -> Result<ApiClient, String> {
    ApiClient::new(credentials.endpoint.clone(), credentials.token.clone())
        .map_err(|error| error.to_string())
}

/// `GET /v1/relay/net` for the console card.
pub fn get_settings(credentials: &crate::worker::Credentials) -> Result<serde_json::Value, String> {
    settings_client(credentials)?
        .relay_get()
        .map_err(|error| error.to_string())
}

/// `PUT /v1/relay/net` for the console card.
pub fn put_settings(
    credentials: &crate::worker::Credentials,
    enabled: bool,
    providers: &[String],
) -> Result<serde_json::Value, String> {
    settings_client(credentials)?
        .relay_put(enabled, providers)
        .map_err(|error| error.to_string())
}

/// Public unicast only (same rule as the server's).
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
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
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| !public_address(IpAddr::V4(v4))))
        }
    }
}

/// Whether this PC opens `host:port` at all (before DNS).
pub fn host_allowed(host: &str, port: u16) -> bool {
    port == 443 && ALLOWED_HOSTS.iter().any(|h| h.eq_ignore_ascii_case(host))
}

/// The public addresses of `host`, or an error naming why none is usable.
fn resolve_public(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let all: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|_| "the host did not resolve".to_string())?
        .collect();
    let public: Vec<SocketAddr> = all
        .iter()
        .copied()
        .filter(|addr| public_address(addr.ip()))
        .collect();
    if public.is_empty() || public.len() != all.len() {
        return Err("the host resolves to a private address".into());
    }
    Ok(public)
}

/// Counters for the console card.
#[derive(Default)]
pub struct RelayStats {
    pub streams: AtomicUsize,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
    /// The server said the relay is on, at the last exchange.
    pub enabled: AtomicBool,
}

struct Shared {
    stop: AtomicBool,
    outbound: Mutex<Vec<Frame>>,
    wake: Condvar,
    streams: Mutex<HashMap<u64, TcpStream>>,
    stats: Arc<RelayStats>,
}

impl Shared {
    fn send(&self, frame: Frame) {
        self.outbound
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(frame);
        self.wake.notify_one();
    }

    fn close_all(&self) {
        let mut streams = self.streams.lock().unwrap_or_else(|p| p.into_inner());
        for (_, stream) in streams.drain() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        self.stats.streams.store(0, Ordering::SeqCst);
    }

    fn handle(self: &Arc<Self>, frame: Frame) {
        match frame {
            Frame::Open { id, host, port } => {
                let shared = self.clone();
                let _ = std::thread::Builder::new()
                    .name("serabot-relay-stream".into())
                    .spawn(move || shared.open(id, &host, port));
            }
            Frame::Data { id, b64 } => {
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
                    return;
                };
                let stream = self
                    .streams
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&id)
                    .and_then(|stream| stream.try_clone().ok());
                match stream {
                    Some(mut stream) => {
                        self.stats
                            .bytes_up
                            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                        if stream.write_all(&bytes).is_err() {
                            self.drop_stream(id, Some("write failed"));
                        }
                    }
                    None => self.send(Frame::Close {
                        id,
                        error: Some("unknown stream".into()),
                    }),
                }
            }
            Frame::Close { id, .. } => {
                if let Some(stream) = self
                    .streams
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id)
                {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    self.stats.streams.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Frame::Opened { .. } => {}
        }
    }

    fn drop_stream(&self, id: u64, error: Option<&str>) {
        let removed = self
            .streams
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        if let Some(stream) = removed {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            self.stats.streams.fetch_sub(1, Ordering::SeqCst);
            self.send(Frame::Close {
                id,
                error: error.map(str::to_owned),
            });
        }
    }

    fn open(self: Arc<Self>, id: u64, host: &str, port: u16) {
        let refuse = |why: &str| {
            self.send(Frame::Close {
                id,
                error: Some(why.to_string()),
            })
        };
        if !host_allowed(host, port) {
            return refuse("this PC does not relay that host");
        }
        if self.streams.lock().unwrap_or_else(|p| p.into_inner()).len() >= MAX_STREAMS {
            return refuse("too many relayed connections");
        }
        let addresses = match resolve_public(host, port) {
            Ok(addresses) => addresses,
            Err(why) => return refuse(&why),
        };
        let Some(stream) = addresses
            .iter()
            .find_map(|addr| TcpStream::connect_timeout(addr, Duration::from_secs(10)).ok())
        else {
            return refuse("could not connect from this PC");
        };
        let _ = stream.set_nodelay(true);
        let Ok(mut reader) = stream.try_clone() else {
            return refuse("could not use the connection");
        };
        self.streams
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, stream);
        self.stats.streams.fetch_add(1, Ordering::SeqCst);
        self.send(Frame::Opened { id });
        let mut buffer = vec![0u8; CHUNK];
        loop {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    self.stats.bytes_down.fetch_add(n as u64, Ordering::Relaxed);
                    self.send(Frame::Data {
                        id,
                        b64: base64::engine::general_purpose::STANDARD.encode(&buffer[..n]),
                    });
                }
            }
        }
        self.drop_stream(id, None);
    }
}

/// The running relay. Stops its threads and closes every connection when
/// dropped.
pub struct NetRelay {
    shared: Arc<Shared>,
}

impl Drop for NetRelay {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        self.shared.close_all();
    }
}

impl NetRelay {
    /// Start the relay threads. They ask the server whether the relay is on;
    /// while it is off they only check back every 15 s.
    pub fn start_with_credentials(
        endpoint: url::Url,
        token: secrecy::SecretString,
    ) -> Option<Self> {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            outbound: Mutex::new(Vec::new()),
            wake: Condvar::new(),
            streams: Mutex::new(HashMap::new()),
            stats: Arc::new(RelayStats::default()),
        });
        let pull = shared.clone();
        let (pull_endpoint, pull_token) = (endpoint.clone(), token.clone());
        std::thread::Builder::new()
            .name("serabot-relay-pull".into())
            .spawn(move || {
                if let Ok(client) = ApiClient::new(pull_endpoint, pull_token) {
                    pull_loop(&client, &pull);
                }
            })
            .ok()?;
        let push = shared.clone();
        std::thread::Builder::new()
            .name("serabot-relay-push".into())
            .spawn(move || {
                if let Ok(client) = ApiClient::new(endpoint, token) {
                    push_loop(&client, &push);
                }
            })
            .ok()?;
        Some(Self { shared })
    }

    pub fn stats(&self) -> Arc<RelayStats> {
        self.shared.stats.clone()
    }
}

fn sleep_unless_stopped(shared: &Shared, total: Duration) {
    let end = std::time::Instant::now() + total;
    while !shared.stop.load(Ordering::SeqCst) {
        let left = end.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(Duration::from_millis(500)));
    }
}

fn pull_loop(client: &ApiClient, shared: &Arc<Shared>) {
    let mut backoff = Duration::from_secs(2);
    while !shared.stop.load(Ordering::SeqCst) {
        match client.relay_exchange(&[], true) {
            Ok(exchange) => {
                backoff = Duration::from_secs(2);
                shared
                    .stats
                    .enabled
                    .store(exchange.enabled, Ordering::SeqCst);
                if !exchange.enabled {
                    shared.close_all();
                    sleep_unless_stopped(shared, Duration::from_secs(15));
                    continue;
                }
                for frame in exchange.frames {
                    shared.handle(frame);
                }
            }
            Err(_) => {
                sleep_unless_stopped(shared, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

fn push_loop(client: &ApiClient, shared: &Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        let frames = {
            let mut outbound = shared.outbound.lock().unwrap_or_else(|p| p.into_inner());
            while outbound.is_empty() && !shared.stop.load(Ordering::SeqCst) {
                outbound = shared
                    .wake
                    .wait_timeout(outbound, Duration::from_millis(500))
                    .map(|(guard, _)| guard)
                    .unwrap_or_else(|p| p.into_inner().0);
            }
            let take = outbound.len().min(256);
            outbound.drain(..take).collect::<Vec<_>>()
        };
        if frames.is_empty() {
            continue;
        }
        match client.relay_exchange(&frames, false) {
            Ok(exchange) => {
                for frame in exchange.frames {
                    shared.handle(frame);
                }
            }
            Err(_) => {
                // The frames are lost: the server times the streams out.
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_addresses_are_checked_here_too() {
        assert!(host_allowed("daily-cloudcode-pa.googleapis.com", 443));
        assert!(host_allowed("API.OPENAI.COM", 443));
        assert!(!host_allowed("api.openai.com", 80));
        assert!(!host_allowed("router.local", 443));
        assert!(!host_allowed("192.168.1.1", 443));
        for private in [
            "10.1.2.3",
            "192.168.0.1",
            "127.0.0.1",
            "100.100.1.1",
            "fe80::1",
            "fd12::1",
        ] {
            assert!(!public_address(private.parse().unwrap()), "{private}");
        }
        assert!(public_address("142.250.74.10".parse().unwrap()));
    }

    #[test]
    fn frames_match_the_server_shape() {
        let open: Frame =
            serde_json::from_str(r#"{"t":"open","id":3,"host":"api.openai.com","port":443}"#)
                .unwrap();
        assert_eq!(
            open,
            Frame::Open {
                id: 3,
                host: "api.openai.com".into(),
                port: 443
            }
        );
        assert_eq!(
            serde_json::to_string(&Frame::Close { id: 3, error: None }).unwrap(),
            r#"{"t":"close","id":3}"#
        );
    }

    #[test]
    fn an_unlisted_host_is_refused_without_connecting() {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            outbound: Mutex::new(Vec::new()),
            wake: Condvar::new(),
            streams: Mutex::new(HashMap::new()),
            stats: Arc::new(RelayStats::default()),
        });
        shared.clone().open(7, "intranet.example", 443);
        let sent = shared.outbound.lock().unwrap().clone();
        assert!(matches!(
            &sent[..],
            [Frame::Close {
                id: 7,
                error: Some(_)
            }]
        ));
        assert_eq!(shared.stats.streams.load(Ordering::SeqCst), 0);
    }
}
