//! What Claude Code on this computer needs, and the only commands SeraBot runs
//! for it. Every command is fixed: no argument comes from a model or a page.
//!
//! - Node.js 22 or newer, installed by the person (SeraBot offers `winget`).
//! - The pinned adapter, fetched by `npx` on first start. It bundles Claude
//!   Code and its own sign-in (`--cli auth login --claudeai`).
//!
//! The child gets a short allowlist of environment variables, so API keys in
//! the desktop's environment never reach it: Claude Code uses its own login.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// The ACP adapter for Claude Code.
pub const PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";
/// Pinned adapter version (a release at least two weeks old when chosen).
pub const VERSION: &str = "0.100.0";
/// The adapter needs Node.js 22 or newer.
pub const MIN_NODE_MAJOR: u64 = 22;

/// `@agentclientprotocol/claude-agent-acp@0.100.0`.
pub fn package_spec() -> String {
    format!("{PACKAGE}@{VERSION}")
}

/// Environment variables the agent process receives; everything else is
/// cleared (API keys and tokens included).
const ENV_ALLOW: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "USERPROFILE",
    "HOME",
    "HOMEDRIVE",
    "HOMEPATH",
    "USERNAME",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "COMMONPROGRAMFILES",
    "TEMP",
    "TMP",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "LANG",
    "CLAUDE_CONFIG_DIR",
    "NODE_EXTRA_CA_CERTS",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
];

/// The allowed part of `vars`.
pub fn child_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    vars.into_iter()
        .filter(|(name, _)| {
            let name = name.to_string_lossy().to_ascii_uppercase();
            ENV_ALLOW.contains(&name.as_str())
        })
        .collect()
}

fn base_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    command.envs(child_env(std::env::vars_os()));
    command
}

/// Find a program on `path` (`PATH`). On Windows `.exe` and `.cmd` are tried.
pub fn find_program(name: &str, path: &OsStr) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd")]
    } else {
        vec![name.to_owned()]
    };
    std::env::split_paths(path).find_map(|dir| {
        names
            .iter()
            .map(|candidate| dir.join(candidate))
            .find(|candidate| candidate.is_file())
    })
}

/// `v22.11.0` → `(22, "22.11.0")`.
pub fn parse_node_version(output: &str) -> Option<(u64, String)> {
    let version = output.trim().trim_start_matches('v');
    let major = version.split('.').next()?.parse().ok()?;
    Some((major, version.to_owned()))
}

/// Where Node.js stands on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeCheck {
    Missing,
    TooOld(String),
    /// Node is there but `npx` is not next to it.
    NoNpx(String),
    Ready {
        version: String,
        npx: PathBuf,
    },
}

impl NodeCheck {
    pub fn npx(&self) -> Option<&Path> {
        match self {
            Self::Ready { npx, .. } => Some(npx),
            _ => None,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Missing => "Node.js is not installed.".into(),
            Self::TooOld(version) => {
                format!(
                    "Node.js {version} is too old; Claude Code needs {MIN_NODE_MAJOR} or newer."
                )
            }
            Self::NoNpx(version) => format!("Node.js {version} is installed but npx is missing."),
            Self::Ready { version, .. } => format!("Node.js {version} is ready."),
        }
    }
}

/// Look for Node.js and npx. Runs `node --version` (blocking; call it off the
/// UI thread).
pub fn check_node() -> NodeCheck {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let Some(node) = find_program("node", &path) else {
        return NodeCheck::Missing;
    };
    let mut command = base_command(&node);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    hide_window(&mut command);
    let Ok(output) = command.output() else {
        return NodeCheck::Missing;
    };
    let Some((major, version)) = parse_node_version(&String::from_utf8_lossy(&output.stdout))
    else {
        return NodeCheck::Missing;
    };
    if major < MIN_NODE_MAJOR {
        return NodeCheck::TooOld(version);
    }
    let npx = node
        .parent()
        .and_then(|dir| find_program("npx", dir.as_os_str()))
        .or_else(|| find_program("npx", &path));
    match npx {
        Some(npx) => NodeCheck::Ready { version, npx },
        None => NodeCheck::NoNpx(version),
    }
}

/// The agent: `npx -y <pinned adapter>`, working in `cwd`, on piped stdio.
pub fn adapter_command(npx: &Path, cwd: &Path) -> Command {
    let mut command = base_command(npx);
    command
        .arg("-y")
        .arg(package_spec())
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_window(&mut command);
    command
}

/// Claude Code's own sign-in, in a console window of its own:
/// `npx -y <adapter> --cli auth login --claudeai`.
pub fn sign_in_command(npx: &Path) -> Command {
    let mut command = base_command(npx);
    command
        .arg("-y")
        .arg(package_spec())
        .args(["--cli", "auth", "login", "--claudeai"]);
    new_console(&mut command);
    command
}

/// Install Node.js LTS with `winget` in a console window (Windows only).
pub fn install_node_command() -> Option<Command> {
    if !cfg!(windows) {
        return None;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let winget = find_program("winget", &path)?;
    let mut command = base_command(&winget);
    command.args([
        "install",
        "-e",
        "--id",
        "OpenJS.NodeJS.LTS",
        "--accept-package-agreements",
        "--accept-source-agreements",
    ]);
    new_console(&mut command);
    Some(command)
}

/// Stop the agent and everything it started (`npx` runs Node, which runs
/// Claude Code).
pub fn stop_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let system = std::env::var_os("SYSTEMROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let mut taskkill = Command::new(system.join("System32").join("taskkill.exe"));
        taskkill
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_window(&mut taskkill);
        let _ = taskkill.status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}

#[cfg(windows)]
fn new_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    command.creation_flags(CREATE_NEW_CONSOLE);
}

#[cfg(not(windows))]
fn new_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_tokens_never_reach_the_agent() {
        let vars = [
            ("PATH", "/bin"),
            ("Path", "/usr/bin"),
            ("ANTHROPIC_API_KEY", "synthetic-key"),
            ("GITHUB_TOKEN", "synthetic-token"),
            ("CLAUDE_CONFIG_DIR", "/cfg"),
            ("HTTPS_PROXY", "http://proxy.example.invalid"),
        ]
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));
        let kept: Vec<String> = child_env(vars)
            .into_iter()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            kept,
            vec!["PATH", "Path", "CLAUDE_CONFIG_DIR", "HTTPS_PROXY"]
        );
    }

    #[test]
    fn node_versions_parse_and_gate_on_the_major() {
        assert_eq!(
            parse_node_version("v22.11.0\n"),
            Some((22, "22.11.0".into()))
        );
        assert_eq!(parse_node_version("v18.2.1"), Some((18, "18.2.1".into())));
        assert_eq!(parse_node_version("garbage"), None);
        assert!(NodeCheck::TooOld("18.2.1".into()).summary().contains("22"));
    }

    #[test]
    fn programs_are_found_on_the_given_path_only() {
        let dir = std::env::temp_dir().join(format!(
            "serabot-acp-path-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = if cfg!(windows) {
            "fakeprog.cmd"
        } else {
            "fakeprog"
        };
        std::fs::write(dir.join(file), "").unwrap();
        let path = std::env::join_paths([dir.clone()]).unwrap();
        assert_eq!(find_program("fakeprog", &path), Some(dir.join(file)));
        assert_eq!(find_program("missingprog", &path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_commands_are_fixed_and_pinned() {
        let npx = Path::new("/opt/node/bin/npx");
        let command = adapter_command(npx, Path::new("/work"));
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["-y".to_string(), package_spec()]);
        assert_eq!(command.get_current_dir(), Some(Path::new("/work")));
        assert!(package_spec().ends_with(&format!("@{VERSION}")));
        let sign_in: Vec<String> = sign_in_command(npx)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&sign_in[2..], ["--cli", "auth", "login", "--claudeai"]);
        // No variable outside the allowlist is passed on.
        assert!(command.get_envs().all(|(name, _)| {
            ENV_ALLOW.contains(&name.to_string_lossy().to_ascii_uppercase().as_str())
        }));
    }
}
