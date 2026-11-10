//! Jobs panel reliability rows (0.30.0, B6): uncertain tasks and
//! `outcome_unknown` actions from `GET /v1/jobs`.

use serde_json::Value;

const MAX_ROWS: usize = 32;
const MAX_ID: usize = 64;
const MAX_TOOL: usize = 128;
const MAX_ERROR: usize = 300;
const MAX_CONVERSATION: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobsReliabilitySnapshot {
    pub uncertain_tasks: Vec<ReliabilityTask>,
    pub unknown_actions: Vec<ReliabilityAction>,
    pub traces_path: Option<String>,
    pub supported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReliabilityTask {
    pub id: String,
    pub kind: String,
    pub bot: String,
    pub status: String,
    pub last_error: Option<String>,
    pub conversation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReliabilityAction {
    pub id: String,
    pub tool: String,
    pub conversation_id: Option<String>,
    pub destination: Option<String>,
}

fn clean(value: &Value, key: &str, max: usize) -> Option<String> {
    let text: String = value
        .get(key)?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

pub fn parse_jobs_reliability(body: &Value) -> Option<JobsReliabilitySnapshot> {
    let reliability = body.get("reliability")?;
    let mut snapshot = JobsReliabilitySnapshot {
        supported: true,
        traces_path: clean(reliability, "traces_path", 64),
        ..JobsReliabilitySnapshot::default()
    };
    if let Some(rows) = reliability
        .get("uncertain_tasks")
        .and_then(|v| v.as_array())
    {
        for row in rows.iter().take(MAX_ROWS) {
            let Some(id) = clean(row, "id", MAX_ID) else {
                continue;
            };
            let kind = clean(row, "kind", 64).unwrap_or_else(|| "task".into());
            let bot = clean(row, "bot", 64).unwrap_or_else(|| "main".into());
            let status = clean(row, "status", 32).unwrap_or_else(|| "failed".into());
            snapshot.uncertain_tasks.push(ReliabilityTask {
                id,
                kind,
                bot,
                status,
                last_error: row.get("last_error").and_then(|v| v.as_str()).map(|s| {
                    s.chars()
                        .filter(|c| !c.is_control())
                        .take(MAX_ERROR)
                        .collect()
                }),
                conversation_id: clean(row, "conversation_id", MAX_CONVERSATION),
            });
        }
    }
    if let Some(rows) = reliability
        .get("unknown_actions")
        .and_then(|v| v.as_array())
    {
        for row in rows.iter().take(MAX_ROWS) {
            let Some(id) = clean(row, "id", MAX_ID) else {
                continue;
            };
            let tool = clean(row, "tool", MAX_TOOL).unwrap_or_else(|| "tool".into());
            snapshot.unknown_actions.push(ReliabilityAction {
                id,
                tool,
                conversation_id: clean(row, "conversation_id", MAX_CONVERSATION),
                destination: clean(row, "destination", 256),
            });
        }
    }
    Some(snapshot)
}
