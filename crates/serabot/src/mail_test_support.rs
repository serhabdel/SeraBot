//! A stateful fake IMAP server for tests (compiled only under `cfg(test)`).
//!
//! It speaks enough of IMAP4rev1 for the mail client: `LOGIN`, `LIST`, `SELECT`,
//! `UID SEARCH` (ALL, UNSEEN, FLAGGED, FROM, SUBJECT, plain OR text), `UID FETCH`,
//! `UID STORE`, `UID MOVE`, `UID EXPUNGE`, `APPEND` (with `APPENDUID`) and `LOGOUT`.
//! It copies the habits that broke clients on Dovecot/Hostinger: extra untagged lines
//! after `LOGIN`, a `.` hierarchy delimiter with unquoted names, and literals that are
//! followed directly by `)`.
//!
//! Every command line is recorded, so a test can assert what the client sent.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::mail::{MailCredentials, Security};

#[derive(Clone, Debug)]
pub(crate) struct FakeMessage {
    pub uid: u32,
    pub raw: String,
    /// Flags without the backslash, for example `Seen`.
    pub flags: Vec<String>,
}

impl FakeMessage {
    pub(crate) fn new(uid: u32, subject: &str, from: &str, body: &str) -> Self {
        Self {
            uid,
            raw: format!(
                "From: {from}\r\nTo: me@example.example\r\nSubject: {subject}\r\nMessage-ID: <m{uid}@fake>\r\nDate: Mon, 23 Nov 2026 09:00:00 +0000\r\n\r\n{body}\r\n"
            ),
            flags: Vec::new(),
        }
    }

    pub(crate) fn seen(mut self) -> Self {
        self.flags.push("Seen".to_owned());
        self
    }
}

#[derive(Default)]
struct State {
    folders: Vec<String>,
    boxes: BTreeMap<String, Vec<FakeMessage>>,
    next_uid: u32,
    log: Vec<String>,
}

pub(crate) struct FakeImap {
    pub port: u16,
    state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeImap {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeImap {
    /// Start a server whose `INBOX` holds `messages`. `folders` are the other folders.
    pub(crate) async fn start(folders: &[&str], messages: Vec<FakeMessage>) -> FakeImap {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut state = State {
            next_uid: messages.iter().map(|m| m.uid).max().unwrap_or(0) + 100,
            ..State::default()
        };
        state.folders.push("INBOX".to_owned());
        state
            .folders
            .extend(folders.iter().map(|f| (*f).to_owned()));
        state.boxes.insert("INBOX".to_owned(), messages);
        for folder in folders {
            state.boxes.entry((*folder).to_owned()).or_default();
        }
        let state = Arc::new(Mutex::new(state));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let state = shared.clone();
                tokio::spawn(async move { handle(socket, state).await });
            }
        });
        FakeImap { port, state, task }
    }

    pub(crate) fn credentials(&self) -> MailCredentials {
        MailCredentials {
            smtp_host: "127.0.0.1".into(),
            smtp_port: 2525,
            smtp_security: Security::None,
            username: "me@example.example".into(),
            password: "app-password".into(),
            imap_host: "127.0.0.1".into(),
            imap_port: self.port,
            imap_security: Security::None,
            from: None,
            signature: None,
        }
    }

    /// Every command line the client has sent so far.
    pub(crate) fn commands(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    pub(crate) fn messages(&self, folder: &str) -> Vec<FakeMessage> {
        self.state
            .lock()
            .unwrap()
            .boxes
            .get(folder)
            .cloned()
            .unwrap_or_default()
    }
}

fn unquote(text: &str) -> String {
    text.trim().trim_matches('"').to_owned()
}

fn flag_list(flags: &[String]) -> String {
    flags
        .iter()
        .map(|f| format!("\\{f}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The text between the first pair of quotes after `key` (e.g. `FROM "anna"`).
fn quoted_after(criteria: &str, key: &str) -> Option<String> {
    let at = criteria.find(&format!("{key} \""))? + key.len() + 2;
    let end = criteria[at..].find('"')?;
    Some(criteria[at..at + end].to_ascii_lowercase())
}

fn matches_search(message: &FakeMessage, criteria: &str) -> bool {
    let has_flag = |name: &str| message.flags.iter().any(|f| f.eq_ignore_ascii_case(name));
    if criteria.contains("UNSEEN") && has_flag("Seen") {
        return false;
    }
    if criteria.contains("UNFLAGGED") && has_flag("Flagged") {
        return false;
    }
    if criteria.contains("FLAGGED") && !criteria.contains("UNFLAGGED") && !has_flag("Flagged") {
        return false;
    }
    let raw = message.raw.to_ascii_lowercase();
    let header = |name: &str| -> String {
        raw.lines()
            .find(|l| l.starts_with(&format!("{name}:")))
            .unwrap_or("")
            .to_owned()
    };
    if criteria.starts_with("OR OR SUBJECT") {
        return quoted_after(criteria, "SUBJECT").is_none_or(|needle| raw.contains(&needle));
    }
    if let Some(needle) = quoted_after(criteria, "FROM")
        && !header("from").contains(&needle)
    {
        return false;
    }
    if let Some(needle) = quoted_after(criteria, "TO")
        && !header("to").contains(&needle)
    {
        return false;
    }
    if let Some(needle) = quoted_after(criteria, "SUBJECT")
        && !header("subject").contains(&needle)
    {
        return false;
    }
    true
}

async fn handle(socket: tokio::net::TcpStream, state: Arc<Mutex<State>>) {
    let (read_half, mut out) = socket.into_split();
    let mut reader = BufReader::new(read_half);
    let _ = out
        .write_all(b"* OK [CAPABILITY IMAP4rev1 UIDPLUS MOVE AUTH=LOGIN] Dovecot ready.\r\n")
        .await;
    let mut selected = "INBOX".to_owned();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let command = line.trim_end_matches(['\r', '\n']).to_owned();
        state.lock().unwrap().log.push(command.clone());
        let mut parts = command.splitn(3, ' ');
        let tag = parts.next().unwrap_or("a0").to_owned();
        let verb = parts.next().unwrap_or("").to_ascii_uppercase();
        let rest = parts.next().unwrap_or("").to_owned();
        let reply: String = match verb.as_str() {
            "LOGIN" => format!(
                "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n* OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*)] Flags permitted.\r\n{tag} OK logged in\r\n"
            ),
            "LIST" => {
                let st = state.lock().unwrap();
                let mut text = String::new();
                for folder in &st.folders {
                    text.push_str(&format!("* LIST (\\HasNoChildren) \".\" {folder}\r\n"));
                }
                text.push_str(&format!("{tag} OK done\r\n"));
                text
            }
            "SELECT" | "EXAMINE" => {
                selected = unquote(&rest);
                let st = state.lock().unwrap();
                match st.boxes.get(&selected) {
                    Some(messages) => format!(
                        "* {} EXISTS\r\n{tag} OK [READ-WRITE] selected\r\n",
                        messages.len()
                    ),
                    None => format!("{tag} NO no such folder\r\n"),
                }
            }
            "UID" => {
                let (sub, args) = rest.split_once(' ').unwrap_or((rest.as_str(), ""));
                let mut st = state.lock().unwrap();
                match sub.to_ascii_uppercase().as_str() {
                    "SEARCH" => {
                        let uids: Vec<String> = st
                            .boxes
                            .get(&selected)
                            .map(|v| {
                                v.iter()
                                    .filter(|m| matches_search(m, args))
                                    .map(|m| m.uid.to_string())
                                    .collect()
                            })
                            .unwrap_or_default();
                        format!("* SEARCH {}\r\n{tag} OK done\r\n", uids.join(" "))
                    }
                    "FETCH" => {
                        let uid: u32 = args
                            .split_whitespace()
                            .next()
                            .and_then(|w| w.parse().ok())
                            .unwrap_or(0);
                        match st
                            .boxes
                            .get(&selected)
                            .and_then(|v| v.iter().find(|m| m.uid == uid))
                        {
                            Some(m) => format!(
                                "* {uid} FETCH (UID {uid} FLAGS ({}) BODY[] {{{}}}\r\n{})\r\n{tag} OK done\r\n",
                                flag_list(&m.flags),
                                m.raw.len(),
                                m.raw
                            ),
                            None => format!("{tag} OK done\r\n"),
                        }
                    }
                    "STORE" => {
                        let mut it = args.splitn(3, ' ');
                        let uid: u32 = it.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                        let op = it.next().unwrap_or("").to_owned();
                        let flags: Vec<String> = it
                            .next()
                            .unwrap_or("")
                            .trim_matches(['(', ')'])
                            .split_whitespace()
                            .map(|f| f.trim_start_matches('\\').to_owned())
                            .collect();
                        if let Some(m) = st
                            .boxes
                            .get_mut(&selected)
                            .and_then(|v| v.iter_mut().find(|m| m.uid == uid))
                        {
                            if op.starts_with('+') {
                                for f in flags {
                                    if !m.flags.contains(&f) {
                                        m.flags.push(f);
                                    }
                                }
                            } else if op.starts_with('-') {
                                m.flags.retain(|f| !flags.contains(f));
                            }
                        }
                        format!("{tag} OK stored\r\n")
                    }
                    "MOVE" => {
                        let mut it = args.splitn(2, ' ');
                        let uid: u32 = it.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                        let dest = unquote(it.next().unwrap_or(""));
                        let taken = st.boxes.get_mut(&selected).and_then(|v| {
                            v.iter().position(|m| m.uid == uid).map(|at| v.remove(at))
                        });
                        match (taken, st.boxes.contains_key(&dest)) {
                            (Some(mut m), true) => {
                                st.next_uid += 1;
                                m.uid = st.next_uid;
                                st.boxes.get_mut(&dest).unwrap().push(m);
                                format!("* 1 EXPUNGE\r\n{tag} OK moved\r\n")
                            }
                            _ => format!("{tag} NO cannot move\r\n"),
                        }
                    }
                    "EXPUNGE" => {
                        let uid: u32 = args.trim().parse().unwrap_or(0);
                        if let Some(v) = st.boxes.get_mut(&selected) {
                            v.retain(|m| !(m.uid == uid && m.flags.iter().any(|f| f == "Deleted")));
                        }
                        format!("* 1 EXPUNGE\r\n{tag} OK expunged\r\n")
                    }
                    _ => format!("{tag} BAD unknown UID command\r\n"),
                }
            }
            "APPEND" => {
                // APPEND "folder" (\Draft \Seen) {N}
                let folder = rest
                    .split('"')
                    .nth(1)
                    .map_or_else(|| "INBOX".to_owned(), str::to_owned);
                let size: usize = rest
                    .rsplit('{')
                    .next()
                    .map(|s| s.trim_end_matches('}'))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let _ = out.write_all(b"+ go ahead\r\n").await;
                let mut bytes = vec![0u8; size];
                if reader.read_exact(&mut bytes).await.is_err() {
                    break;
                }
                let mut tail = String::new();
                let _ = reader.read_line(&mut tail).await;
                let mut st = state.lock().unwrap();
                if st.boxes.contains_key(&folder) {
                    st.next_uid += 1;
                    let uid = st.next_uid;
                    st.boxes.get_mut(&folder).unwrap().push(FakeMessage {
                        uid,
                        raw: String::from_utf8_lossy(&bytes).into_owned(),
                        flags: vec!["Draft".to_owned(), "Seen".to_owned()],
                    });
                    format!("{tag} OK [APPENDUID 1 {uid}] Append completed.\r\n")
                } else {
                    format!("{tag} NO [TRYCREATE] no such folder\r\n")
                }
            }
            "LOGOUT" => {
                let _ = out
                    .write_all(format!("* BYE\r\n{tag} OK bye\r\n").as_bytes())
                    .await;
                break;
            }
            _ => format!("{tag} OK\r\n"),
        };
        if out.write_all(reply.as_bytes()).await.is_err() {
            break;
        }
    }
}
