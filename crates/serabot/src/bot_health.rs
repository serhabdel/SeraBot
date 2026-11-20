//! Bot health (0.70): per bot, over the last 7 or 30 days, how many turns
//! ran, how many finished, how long they took, which tools failed and how
//! often a turn moved to another model.
//!
//! Counts and durations only; no message text. Every query is scoped to the
//! acting member's tasks. A turn is a `tasks` row of kind `turn`; its length
//! is `updated_at - created_at` of a finished task; tool failures are
//! `tool.failed` audit rows and failovers `model.failover` audit rows, both
//! tied to the member's task by `task_id`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::Serialize;

use crate::error::Result;
use crate::store::Store;
use crate::usage_insights::percentile;

/// Needs attention below this success rate (percent) with enough turns.
pub const MIN_SUCCESS_PERCENT: f64 = 80.0;
/// Turns needed before the success rate is judged.
pub const MIN_TURNS_FOR_RATE: i64 = 5;
/// A tool failing this many times needs attention.
pub const TOOL_FAILURES_LIMIT: i64 = 3;
/// A slow p95 (milliseconds) needs attention: 3 minutes.
pub const P95_LIMIT_MS: i64 = 180_000;

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ToolFailure {
    pub tool: String,
    pub failures: i64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct BotHealth {
    pub bot: String,
    pub turns: i64,
    /// Turns that finished: done.
    pub succeeded: i64,
    /// Turns that failed, were stopped or need review.
    pub failed: i64,
    /// Percent of finished turns that succeeded; `None` with no finished turn.
    pub success_percent: Option<f64>,
    pub avg_ms: i64,
    pub p95_ms: i64,
    pub failovers: i64,
    pub top_failing_tools: Vec<ToolFailure>,
    pub needs_attention: bool,
    pub reason: Option<String>,
}

/// One turn as the math sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnRow {
    pub bot: String,
    pub status: String,
    /// Milliseconds from creation to last update, for finished turns.
    pub duration_ms: Option<i64>,
}

fn is_failure(status: &str) -> bool {
    matches!(status, "failed" | "cancelled" | "needs_review")
}

/// The attention rules: success under 80% with at least 5 turns, a tool that
/// failed 3 or more times, or a p95 over 3 minutes. The first match names the
/// reason.
pub fn attention_reason(
    turns: i64,
    success_percent: Option<f64>,
    top_tool: Option<&ToolFailure>,
    p95_ms: i64,
) -> Option<String> {
    if let Some(percent) = success_percent
        && turns >= MIN_TURNS_FOR_RATE
        && percent < MIN_SUCCESS_PERCENT
    {
        return Some(format!("Only {percent:.0}% of {turns} turns finished"));
    }
    if let Some(tool) = top_tool
        && tool.failures >= TOOL_FAILURES_LIMIT
    {
        return Some(format!("{} failed {} times", tool.tool, tool.failures));
    }
    if p95_ms > P95_LIMIT_MS {
        return Some(format!(
            "Slow turns: 1 in 20 takes over {} min",
            p95_ms / 60_000
        ));
    }
    None
}

/// Combine turns, tool failures (`(bot, tool)`) and failovers per bot.
pub fn compute(
    turns: &[TurnRow],
    tool_failures: &[(String, String)],
    failovers: &[String],
) -> Vec<BotHealth> {
    let mut by_bot: BTreeMap<String, BotHealth> = BTreeMap::new();
    let mut durations: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for turn in turns {
        let entry = by_bot.entry(turn.bot.clone()).or_default();
        entry.turns += 1;
        if turn.status == "done" {
            entry.succeeded += 1;
        } else if is_failure(&turn.status) {
            entry.failed += 1;
        }
        if turn.status == "done"
            && let Some(ms) = turn.duration_ms
        {
            durations
                .entry(turn.bot.clone())
                .or_default()
                .push(ms.max(0));
        }
    }
    let mut tools: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for (bot, tool) in tool_failures {
        if by_bot.contains_key(bot) {
            *tools
                .entry(bot.clone())
                .or_default()
                .entry(tool.clone())
                .or_default() += 1;
        }
    }
    for bot in failovers {
        if let Some(entry) = by_bot.get_mut(bot) {
            entry.failovers += 1;
        }
    }
    let mut out: Vec<BotHealth> = by_bot
        .into_iter()
        .map(|(bot, mut health)| {
            health.bot = bot.clone();
            let finished = health.succeeded + health.failed;
            health.success_percent =
                (finished > 0).then(|| health.succeeded as f64 * 100.0 / finished as f64);
            let mut values = durations.remove(&bot).unwrap_or_default();
            values.sort_unstable();
            if !values.is_empty() {
                health.avg_ms = values.iter().sum::<i64>() / values.len() as i64;
            }
            health.p95_ms = percentile(&values, 95);
            let mut failing: Vec<ToolFailure> = tools
                .remove(&bot)
                .unwrap_or_default()
                .into_iter()
                .map(|(tool, failures)| ToolFailure { tool, failures })
                .collect();
            failing.sort_by(|a, b| {
                b.failures
                    .cmp(&a.failures)
                    .then_with(|| a.tool.cmp(&b.tool))
            });
            failing.truncate(3);
            health.reason = attention_reason(
                health.turns,
                health.success_percent,
                failing.first(),
                health.p95_ms,
            );
            health.needs_attention = health.reason.is_some();
            health.top_failing_tools = failing;
            health
        })
        .collect();
    // Bots that need attention first, then the busiest.
    out.sort_by(|a, b| {
        b.needs_attention
            .cmp(&a.needs_attention)
            .then_with(|| b.turns.cmp(&a.turns))
            .then_with(|| a.bot.cmp(&b.bot))
    });
    out
}

fn duration_ms(created: &str, updated: &str) -> Option<i64> {
    let created = DateTime::parse_from_rfc3339(created).ok()?;
    let updated = DateTime::parse_from_rfc3339(updated).ok()?;
    Some((updated - created).num_milliseconds())
}

impl Store {
    /// Health of the member's bots since `since`. Only the member's own tasks
    /// and the audit rows of those tasks are read.
    pub fn bot_health(&self, user_id: &str, since: DateTime<Utc>) -> Result<Vec<BotHealth>> {
        let connection = self.connection()?;
        let since = since.to_rfc3339();
        let mut turns = Vec::new();
        {
            let mut statement = connection.prepare(
                "SELECT bot, status, created_at, updated_at FROM tasks
                 WHERE user_id = ?1 AND kind = 'turn' AND created_at >= ?2
                 ORDER BY created_at DESC LIMIT 50000",
            )?;
            let rows = statement.query_map(params![user_id, since], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (bot, status, created, updated) = row?;
                turns.push(TurnRow {
                    duration_ms: duration_ms(&created, &updated),
                    bot,
                    status,
                });
            }
        }
        let audit = |event: &str| -> Result<Vec<(String, String)>> {
            let mut statement = connection.prepare(
                "SELECT t.bot, COALESCE(a.tool, '') FROM audit_log a
                 JOIN tasks t ON t.id = a.task_id
                 WHERE t.user_id = ?1 AND t.kind = 'turn' AND a.event = ?2 AND a.at >= ?3
                 LIMIT 100000",
            )?;
            let rows = statement.query_map(params![user_id, event, since], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        };
        let tool_failures: Vec<(String, String)> = audit("tool.failed")?
            .into_iter()
            .filter(|(_, tool)| !tool.is_empty())
            .collect();
        let failovers: Vec<String> = audit("model.failover")?
            .into_iter()
            .map(|(bot, _)| bot)
            .collect();
        Ok(compute(&turns, &tool_failures, &failovers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(bot: &str, status: &str, ms: i64) -> TurnRow {
        TurnRow {
            bot: bot.into(),
            status: status.into(),
            duration_ms: Some(ms),
        }
    }

    #[test]
    fn success_rate_and_durations() {
        let mut turns = Vec::new();
        for ms in [1000, 2000, 3000, 4000] {
            turns.push(turn("main", "done", ms));
        }
        turns.push(turn("main", "failed", 500));
        turns.push(turn("main", "running", 0));
        let out = compute(&turns, &[], &[]);
        let main = &out[0];
        assert_eq!(main.turns, 6);
        assert_eq!(main.succeeded, 4);
        assert_eq!(main.failed, 1);
        assert_eq!(main.success_percent, Some(80.0));
        assert_eq!(main.avg_ms, 2500);
        assert_eq!(main.p95_ms, 4000);
        assert!(!main.needs_attention);
    }

    #[test]
    fn low_success_needs_attention_only_with_five_turns() {
        let few: Vec<_> = (0..4).map(|_| turn("a", "failed", 1)).collect();
        assert!(!compute(&few, &[], &[])[0].needs_attention);
        let mut many: Vec<_> = (0..3).map(|_| turn("a", "done", 1)).collect();
        many.extend((0..2).map(|_| turn("a", "cancelled", 1)));
        let health = &compute(&many, &[], &[])[0];
        assert!(health.needs_attention);
        assert!(health.reason.as_deref().unwrap().contains("60%"));
    }

    #[test]
    fn repeated_tool_failure_needs_attention() {
        let turns = vec![turn("a", "done", 10)];
        let failures: Vec<_> = (0..3)
            .map(|_| ("a".to_string(), "fetch_url".to_string()))
            .collect();
        let health = &compute(&turns, &failures, &[])[0];
        assert!(health.needs_attention);
        assert_eq!(health.top_failing_tools[0].tool, "fetch_url");
        assert_eq!(health.top_failing_tools[0].failures, 3);
        assert!(health.reason.as_deref().unwrap().contains("fetch_url"));
        let two = &failures[..2];
        assert!(!compute(&turns, two, &[])[0].needs_attention);
    }

    #[test]
    fn slow_p95_needs_attention() {
        let turns = vec![turn("a", "done", P95_LIMIT_MS + 1)];
        let health = &compute(&turns, &[], &[])[0];
        assert!(health.needs_attention);
        assert!(health.reason.as_deref().unwrap().contains("Slow"));
        let ok = vec![turn("a", "done", P95_LIMIT_MS)];
        assert!(!compute(&ok, &[], &[])[0].needs_attention);
    }

    #[test]
    fn failovers_and_unknown_bots() {
        let turns = vec![turn("a", "done", 1)];
        let out = compute(
            &turns,
            &[("ghost".into(), "x".into())],
            &["a".into(), "a".into(), "ghost".into()],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].failovers, 2);
        assert!(out[0].top_failing_tools.is_empty());
    }

    #[test]
    fn attention_bots_sort_first() {
        let mut turns = vec![turn("calm", "done", 1), turn("calm", "done", 1)];
        turns.extend((0..5).map(|_| turn("sick", "failed", 1)));
        let out = compute(&turns, &[], &[]);
        assert_eq!(out[0].bot, "sick");
    }

    fn seed(store: &Store, user: &str, bot: &str, status: &str) -> String {
        let task = store
            .enqueue_task(user, "turn", bot, &serde_json::json!({}), 0, Utc::now())
            .unwrap();
        let connection = store.connection().unwrap();
        connection
            .execute(
                "UPDATE tasks SET status = ?1 WHERE id = ?2",
                params![status, task.id],
            )
            .unwrap();
        task.id
    }

    #[test]
    fn store_query_is_scoped_to_the_member() {
        let store = Store::open_in_memory(8).unwrap();
        let alice = store
            .create_user("microsoft:alice-health", "a@example.example", "A")
            .unwrap()
            .id;
        let bob = store
            .create_user("microsoft:bob-health", "b@example.example", "B")
            .unwrap()
            .id;
        let done = seed(&store, &alice, "main", "done");
        let failed = seed(&store, &alice, "main", "failed");
        seed(&store, &bob, "main", "failed");
        seed(&store, &bob, "bobbot", "failed");
        for task in [&failed, &failed, &failed] {
            store
                .record_audit(
                    &alice,
                    "tool.failed",
                    Some("fetch_url"),
                    Some(task),
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
        }
        store
            .record_audit(
                &alice,
                "model.failover",
                None,
                Some(&done),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let since = Utc::now() - chrono::Duration::days(7);
        let mine = store.bot_health(&alice, since).unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].turns, 2);
        assert_eq!(mine[0].failed, 1);
        assert_eq!(mine[0].failovers, 1);
        assert_eq!(mine[0].top_failing_tools[0].failures, 3);
        assert!(mine[0].needs_attention);
        let theirs = store.bot_health(&bob, since).unwrap();
        assert_eq!(theirs.len(), 2);
        assert!(theirs.iter().all(|b| b.top_failing_tools.is_empty()));
        // The window excludes older tasks.
        let future = Utc::now() + chrono::Duration::days(1);
        assert!(store.bot_health(&alice, future).unwrap().is_empty());
    }
}
