//! Process supervisor for the loopback Claude Agent SDK gateway
//! (`services/claude_gateway/server.py`).
//!
//! Spawns and reaps loopback `uvicorn` child processes keyed by normalized
//! loopback `base_url`. `SERABOT_CLAUDE_GATEWAY_TOKEN` is passed strictly via
//! child environment (`cmd.env`), never on the command line and never logged.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::{ClaudeGatewayConfig, validate_loopback_gateway_url};
use crate::error::{Result, SeraError};

const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
const STARTUP_POLL_TIMEOUT: Duration = Duration::from_secs(6);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(150);
const STDERR_CAP_BYTES: usize = 4096;
const INSTALL_HINT: &str = "pip install -r services/claude_gateway/requirements.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayStartState {
    AlreadyRunning,
    Started,
}

pub struct ClaudeGatewaySupervisor {
    children: Mutex<HashMap<String, Child>>,
    start_lock: tokio::sync::Mutex<()>,
}

impl Default for ClaudeGatewaySupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeGatewaySupervisor {
    pub fn new() -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
            start_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Ensure the loopback gateway at `base_url` is reachable.
    ///
    /// Probes `/health` first; if unreachable and `cfg.auto_start` is true,
    /// locates `services/claude_gateway/server.py` and Python, spawns
    /// `uvicorn server:app` on loopback, and polls `/health` until ready.
    pub async fn ensure_running(
        &self,
        base_url: &str,
        token: &str,
        cfg: &ClaudeGatewayConfig,
    ) -> Result<GatewayStartState> {
        let normalized = validate_loopback_gateway_url(base_url)?;
        if probe_health(&normalized).await {
            return Ok(GatewayStartState::AlreadyRunning);
        }
        if !cfg.auto_start {
            return Err(SeraError::Rejected {
                code: "gateway_unreachable",
                message: "The local Claude Agent SDK gateway is not running. Start services/claude_gateway/server.py on loopback or enable [claude_gateway] auto_start = true.".into(),
            });
        }

        let _guard = self.start_lock.lock().await;
        if probe_health(&normalized).await {
            return Ok(GatewayStartState::AlreadyRunning);
        }

        // Reap any previously tracked child for this URL if it already exited.
        let has_live_child = {
            let mut map = self
                .children
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match map.get_mut(&normalized).map(|child| child.try_wait()) {
                Some(Ok(None)) => true,
                Some(Ok(Some(_))) | Some(Err(_)) => {
                    map.remove(&normalized);
                    false
                }
                None => false,
            }
        };
        if has_live_child {
            if wait_for_health(&normalized, Duration::from_secs(2)).await {
                return Ok(GatewayStartState::Started);
            }
            let mut map = self
                .children
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(mut stale) = map.remove(&normalized) {
                let _ = stale.kill();
                let _ = stale.wait();
            }
        }

        let script = locate_server_script(&cfg.script_path)?;
        let gateway_dir = script
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let python = locate_python(&cfg.python_bin, &gateway_dir)?;
        check_python_imports(&python, &gateway_dir)?;

        let (host, port) = parse_host_port(&normalized)?;
        let mut cmd = Command::new(&python);
        cmd.current_dir(&gateway_dir)
            .arg("-m")
            .arg("uvicorn")
            .arg("server:app")
            .arg("--host")
            .arg(&host)
            .arg("--port")
            .arg(port.to_string())
            .env("SERABOT_CLAUDE_GATEWAY_TOKEN", token)
            .env("SERABOT_CLAUDE_GATEWAY_HOST", &host)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|err| SeraError::Rejected {
            code: "gateway_start_failed",
            message: format!(
                "Failed to spawn the Claude Agent SDK gateway ({err}). Install dependencies with: {INSTALL_HINT}"
            ),
        })?;

        let deadline = Instant::now() + STARTUP_POLL_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait().map_err(|err| SeraError::Rejected {
                code: "gateway_start_failed",
                message: format!("Failed to check Claude Agent SDK gateway process: {err}"),
            })? {
                let stderr = read_bounded_stderr(child.stderr.take(), token);
                return Err(classify_early_exit(status.to_string(), &stderr));
            }
            if probe_health(&normalized).await {
                // Drain stderr in a background thread so a long-lived child
                // never blocks on a full pipe.
                if let Some(stderr) = child.stderr.take() {
                    std::thread::spawn(move || {
                        let mut sink = std::io::sink();
                        let mut reader = stderr;
                        let _ = std::io::copy(&mut reader, &mut sink);
                    });
                }
                let mut map = self
                    .children
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                map.insert(normalized, child);
                return Ok(GatewayStartState::Started);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let stderr = read_bounded_stderr(child.stderr.take(), token);
                let detail = if stderr.is_empty() {
                    String::new()
                } else {
                    format!(" Details: {stderr}")
                };
                return Err(SeraError::Rejected {
                    code: "gateway_start_failed",
                    message: format!(
                        "The Claude Agent SDK gateway did not become ready within {}s. Install dependencies with: {INSTALL_HINT}.{detail}",
                        STARTUP_POLL_TIMEOUT.as_secs()
                    ),
                });
            }
            tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
        }
    }
}

impl Drop for ClaudeGatewaySupervisor {
    fn drop(&mut self) {
        let mut map = self
            .children
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (_, mut child) in map.drain() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn probe_health(normalized_base_url: &str) -> bool {
    let url = format!("{}/health", normalized_base_url.trim_end_matches('/'));
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .timeout(HEALTH_PROBE_TIMEOUT)
        .build()
    else {
        return false;
    };
    match client.get(url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn wait_for_health(normalized_base_url: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if probe_health(normalized_base_url).await {
            return true;
        }
        tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
    }
    false
}

fn locate_server_script(configured: &str) -> Result<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Ok(path);
        }
        return Err(SeraError::Rejected {
            code: "gateway_script_missing",
            message: format!(
                "Configured [claude_gateway] script_path was not found: {}",
                path.display()
            ),
        });
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("services/claude_gateway/server.py"));
        candidates.push(dir.join("server.py"));
    }
    candidates.push(PathBuf::from("services/claude_gateway/server.py"));
    candidates.push(PathBuf::from("../services/claude_gateway/server.py"));
    candidates.push(PathBuf::from("../../services/claude_gateway/server.py"));

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(SeraError::Rejected {
        code: "gateway_script_missing",
        message: "Could not locate services/claude_gateway/server.py. Set [claude_gateway] script_path in serabot.toml.".into(),
    })
}

fn locate_python(configured: &str, gateway_dir: &Path) -> Result<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if python_works(&path) {
            return Ok(path);
        }
        return Err(SeraError::Rejected {
            code: "missing_python",
            message: format!(
                "Configured [claude_gateway] python_bin could not be run: {}. Install Python 3 and run: {INSTALL_HINT}",
                path.display()
            ),
        });
    }

    let candidates = [
        gateway_dir.join(".venv/bin/python"),
        gateway_dir.join(".venv/Scripts/python.exe"),
        PathBuf::from("python3"),
        PathBuf::from("python"),
    ];
    for candidate in candidates {
        if python_works(&candidate) {
            return Ok(candidate);
        }
    }

    Err(SeraError::Rejected {
        code: "missing_python",
        message: format!(
            "Python 3 was not found. Install Python 3 and run `{INSTALL_HINT}`, or set [claude_gateway] python_bin."
        ),
    })
}

fn python_works(bin: &Path) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn check_python_imports(python: &Path, gateway_dir: &Path) -> Result<()> {
    for (module, code, label) in [
        ("uvicorn", "missing_uvicorn", "uvicorn"),
        (
            "claude_agent_sdk",
            "missing_sdk",
            "claude-agent-sdk (`claude_agent_sdk`)",
        ),
    ] {
        let status = Command::new(python)
            .current_dir(gateway_dir)
            .arg("-c")
            .arg(format!("import {module}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !status.is_ok_and(|s| s.success()) {
            return Err(SeraError::Rejected {
                code,
                message: format!(
                    "The Claude Agent SDK gateway requires {label}. Install dependencies with: {INSTALL_HINT}"
                ),
            });
        }
    }
    Ok(())
}

fn parse_host_port(normalized_base_url: &str) -> Result<(String, u16)> {
    let parsed = url::Url::parse(normalized_base_url)
        .map_err(|_| SeraError::Config("invalid claude_gateway url".into()))?;
    let raw_host = parsed.host_str().unwrap_or("127.0.0.1");
    let host = if raw_host.eq_ignore_ascii_case("localhost") {
        "127.0.0.1".to_owned()
    } else {
        raw_host.trim_matches(|c| c == '[' || c == ']').to_owned()
    };
    let port = parsed.port_or_known_default().unwrap_or(8791);
    Ok((host, port))
}

fn read_bounded_stderr(stderr: Option<std::process::ChildStderr>, token: &str) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut buf = vec![0u8; STDERR_CAP_BYTES];
    let mut total = 0usize;
    while total < buf.len() {
        match stderr.read(&mut buf[total..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => total += n,
        }
    }
    let text = String::from_utf8_lossy(&buf[..total]).trim().to_owned();
    if token.is_empty() {
        text
    } else {
        text.replace(token, "[REDACTED]")
    }
}

fn classify_early_exit(status: String, stderr: &str) -> SeraError {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("no module named") && lower.contains("uvicorn") {
        return SeraError::Rejected {
            code: "missing_uvicorn",
            message: format!(
                "The Claude Agent SDK gateway requires uvicorn. Install dependencies with: {INSTALL_HINT}"
            ),
        };
    }
    if lower.contains("no module named") && lower.contains("claude_agent_sdk") {
        return SeraError::Rejected {
            code: "missing_sdk",
            message: format!(
                "The Claude Agent SDK package is not installed. Install dependencies with: {INSTALL_HINT}"
            ),
        };
    }
    let detail = if stderr.is_empty() {
        String::new()
    } else {
        format!(" Details: {stderr}")
    };
    SeraError::Rejected {
        code: "gateway_start_failed",
        message: format!(
            "The Claude Agent SDK gateway exited early ({status}). Install dependencies with: {INSTALL_HINT}.{detail}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn ensure_running_returns_already_running_when_health_is_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let supervisor = ClaudeGatewaySupervisor::new();
        let cfg = ClaudeGatewayConfig::default();
        let state = supervisor
            .ensure_running(&server.uri(), "tok-test", &cfg)
            .await
            .unwrap();
        assert_eq!(state, GatewayStartState::AlreadyRunning);
    }

    #[tokio::test]
    async fn ensure_running_rejects_when_auto_start_is_disabled() {
        let supervisor = ClaudeGatewaySupervisor::new();
        let cfg = ClaudeGatewayConfig {
            auto_start: false,
            ..Default::default()
        };
        let err = supervisor
            .ensure_running("http://127.0.0.1:59997", "tok-test", &cfg)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            SeraError::Rejected {
                code: "gateway_unreachable",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn ensure_running_rejects_non_loopback_url() {
        let supervisor = ClaudeGatewaySupervisor::new();
        let cfg = ClaudeGatewayConfig::default();
        let err = supervisor
            .ensure_running("https://example.com:8791", "tok-test", &cfg)
            .await
            .unwrap_err();
        assert!(matches!(err, SeraError::Config(_)));
    }

    #[test]
    fn parse_host_port_handles_localhost_and_ipv6() {
        assert_eq!(
            parse_host_port("http://localhost:8791").unwrap(),
            ("127.0.0.1".into(), 8791)
        );
        assert_eq!(
            parse_host_port("http://[::1]:8792").unwrap(),
            ("::1".into(), 8792)
        );
    }

    #[test]
    fn classify_early_exit_identifies_missing_modules() {
        let err = classify_early_exit(
            "exit status: 1".into(),
            "ModuleNotFoundError: No module named 'uvicorn'",
        );
        assert!(matches!(
            err,
            SeraError::Rejected {
                code: "missing_uvicorn",
                ..
            }
        ));

        let err = classify_early_exit(
            "exit status: 1".into(),
            "ModuleNotFoundError: No module named 'claude_agent_sdk'",
        );
        assert!(matches!(
            err,
            SeraError::Rejected {
                code: "missing_sdk",
                ..
            }
        ));
    }
}
