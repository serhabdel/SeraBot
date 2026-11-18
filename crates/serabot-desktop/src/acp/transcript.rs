//! The Claude Code chat as the page shows it: streamed chunks folded into
//! messages, tool calls updated in place, the latest plan, and notices.

use super::protocol::{PlanEntry, Update};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Total characters kept across the transcript; the oldest items go first.
const MAX_CHARS: usize = 2_000_000;
const TRANSCRIPTS_DIR: &str = "acp-transcripts";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Item {
    User(String),
    Agent(String),
    Thought(String),
    Tool {
        id: String,
        title: String,
        kind: String,
        status: String,
    },
    Plan(Vec<PlanEntry>),
    Notice {
        text: String,
        error: bool,
    },
}

impl Item {
    fn chars(&self) -> usize {
        match self {
            Self::User(text) | Self::Agent(text) | Self::Thought(text) => text.len(),
            Self::Tool { title, .. } => title.len() + 16,
            Self::Plan(entries) => entries.iter().map(|entry| entry.content.len()).sum(),
            Self::Notice { text, .. } => text.len(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transcript {
    pub items: Vec<Item>,
}

fn transcript_path(dir: &Path, bot_id: &str) -> Option<PathBuf> {
    if !crate::valid_speaker(bot_id) {
        return None;
    }
    Some(dir.join(TRANSCRIPTS_DIR).join(format!("{bot_id}.json")))
}

impl Transcript {
    pub fn load_for_bot(dir: &Path, bot_id: &str) -> Self {
        let Some(path) = transcript_path(dir, bot_id) else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str::<Self>(&text)
            .map(|mut loaded| {
                loaded.trim();
                loaded
            })
            .unwrap_or_default()
    }

    pub fn save_for_bot(&self, dir: &Path, bot_id: &str) -> Result<(), String> {
        let Some(path) = transcript_path(dir, bot_id) else {
            return Err("Invalid bot id for transcript.".to_owned());
        };
        let parent = path
            .parent()
            .ok_or_else(|| "Invalid transcript directory.".to_owned())?;
        std::fs::create_dir_all(parent)
            .map_err(|_| "Could not create transcript folder.".to_owned())?;
        let text =
            serde_json::to_string(self).map_err(|_| "Could not encode transcript.".to_owned())?;
        let temp = parent.join(format!(".{bot_id}.json.tmp"));
        std::fs::write(&temp, text).map_err(|_| "Could not write transcript.".to_owned())?;
        std::fs::rename(&temp, &path).map_err(|_| {
            let _ = std::fs::remove_file(&temp);
            "Could not save transcript.".to_owned()
        })
    }
    pub fn push_user(&mut self, text: &str) {
        self.items.push(Item::User(text.to_owned()));
        self.trim();
    }

    pub fn notice(&mut self, text: impl Into<String>, error: bool) {
        self.items.push(Item::Notice {
            text: text.into(),
            error,
        });
        self.trim();
    }

    pub fn apply(&mut self, update: Update) {
        match update {
            Update::AgentText(text) => self.append(text, Item::Agent),
            Update::Thought(text) => self.append(text, Item::Thought),
            // Our own prompt is already shown; a replayed one is not repeated.
            Update::UserText(_) | Update::Other => {}
            Update::ToolCall {
                id,
                title,
                kind,
                status,
            } => {
                if let Some(Item::Tool {
                    title: old_title,
                    kind: old_kind,
                    status: old_status,
                    ..
                }) = self.tool_mut(&id)
                {
                    *old_title = title;
                    *old_kind = kind;
                    *old_status = status;
                } else {
                    self.items.push(Item::Tool {
                        id,
                        title,
                        kind,
                        status,
                    });
                }
            }
            Update::ToolUpdate { id, title, status } => {
                if let Some(Item::Tool {
                    title: old_title,
                    status: old_status,
                    ..
                }) = self.tool_mut(&id)
                {
                    if let Some(title) = title {
                        *old_title = title;
                    }
                    if let Some(status) = status {
                        *old_status = status;
                    }
                }
            }
            Update::Plan(entries) => {
                // One plan per turn: the newest replaces an earlier one that
                // has no user message after it.
                let last_user = self
                    .items
                    .iter()
                    .rposition(|item| matches!(item, Item::User(_)));
                let plan = self
                    .items
                    .iter()
                    .rposition(|item| matches!(item, Item::Plan(_)));
                match (plan, last_user) {
                    (Some(at), Some(user)) if at > user => self.items[at] = Item::Plan(entries),
                    (Some(at), None) => self.items[at] = Item::Plan(entries),
                    _ => self.items.push(Item::Plan(entries)),
                }
            }
        }
        self.trim();
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut Item> {
        self.items
            .iter_mut()
            .rev()
            .find(|item| matches!(item, Item::Tool { id: tool, .. } if tool == id))
    }

    fn append(&mut self, text: String, make: fn(String) -> Item) {
        if text.is_empty() {
            return;
        }
        match (self.items.last_mut(), make(String::new())) {
            (Some(Item::Agent(existing)), Item::Agent(_))
            | (Some(Item::Thought(existing)), Item::Thought(_)) => existing.push_str(&text),
            _ => self.items.push(make(text)),
        }
    }

    fn trim(&mut self) {
        let mut total: usize = self.items.iter().map(Item::chars).sum();
        while total > MAX_CHARS && self.items.len() > 1 {
            total -= self.items.remove(0).chars();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_fold_into_one_message_and_tools_update_in_place() {
        let mut transcript = Transcript::default();
        transcript.push_user("hello");
        transcript.apply(Update::Thought("think ".into()));
        transcript.apply(Update::Thought("more".into()));
        transcript.apply(Update::AgentText("Hel".into()));
        transcript.apply(Update::AgentText("lo".into()));
        transcript.apply(Update::ToolCall {
            id: "t".into(),
            title: "Read a".into(),
            kind: "read".into(),
            status: "pending".into(),
        });
        transcript.apply(Update::ToolUpdate {
            id: "t".into(),
            title: None,
            status: Some("completed".into()),
        });
        transcript.apply(Update::AgentText("Done".into()));
        assert_eq!(
            transcript.items,
            vec![
                Item::User("hello".into()),
                Item::Thought("think more".into()),
                Item::Agent("Hello".into()),
                Item::Tool {
                    id: "t".into(),
                    title: "Read a".into(),
                    kind: "read".into(),
                    status: "completed".into()
                },
                Item::Agent("Done".into()),
            ]
        );
    }

    #[test]
    fn a_turn_keeps_one_plan() {
        let mut transcript = Transcript::default();
        transcript.push_user("go");
        let entry = |status: &str| PlanEntry {
            content: "Step".into(),
            status: status.into(),
        };
        transcript.apply(Update::Plan(vec![entry("pending")]));
        transcript.apply(Update::Plan(vec![entry("completed")]));
        assert_eq!(transcript.items.len(), 2);
        transcript.push_user("again");
        transcript.apply(Update::Plan(vec![entry("pending")]));
        assert_eq!(transcript.items.len(), 4);
    }

    #[test]
    fn per_bot_transcript_round_trips_and_rejects_bad_bot_ids() {
        let dir = std::env::temp_dir().join(format!(
            "serabot-acp-transcript-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let mut transcript = Transcript::default();
        transcript.push_user("Explain Cargo.toml");
        transcript.apply(Update::AgentText("It defines the workspace.".into()));
        transcript.save_for_bot(&dir, "analyst").unwrap();

        let loaded = Transcript::load_for_bot(&dir, "analyst");
        assert_eq!(loaded, transcript);
        assert!(Transcript::load_for_bot(&dir, "../escape").items.is_empty());
        assert!(transcript.save_for_bot(&dir, "../escape").is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
