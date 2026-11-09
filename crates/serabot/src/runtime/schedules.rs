//! Scheduled routines: briefs and watch runs.

use super::*;

impl Runtime {
    pub(super) async fn run_brief(&self, task: &Task) -> Result<TurnOutcome> {
        let payload: ScheduledPayload = serde_json::from_value(task.payload.clone())
            .map_err(|error| SeraError::Validation(format!("invalid brief payload: {error}")))?;
        // The task row's owner is authoritative; the payload's copy is
        // metadata recorded for observability.
        let user = task.user_id.clone();
        let tz: chrono_tz::Tz = self
            .config
            .schedule
            .timezone
            .parse()
            .map_err(|_| SeraError::Config("invalid schedule timezone".into()))?;
        let report = crate::brief::build_brief_report(&self.store, &user, Utc::now(), tz, 24)?;
        let text = if self.gateway.scan_egress(&user, &report.text).is_err() {
            self.store.record_audit(
                &user,
                "brief.blocked_by_policy",
                None,
                Some(&task.id),
                None,
                None,
                Some(self.gateway.policy().version()),
                None,
            )?;
            "The morning brief was withheld because it violated the content policy.".to_string()
        } else {
            report.text
        };
        // Opt-in silence: an empty brief (no deadlines, no new/changed items,
        // no source warnings) sends no notification but is still audited.
        if self.config.watcher.silent_when_empty && report.empty {
            let details = json!({
                "task_id": task.id,
                "conversation_id": payload.conversation_id,
            });
            self.store.record_audit(
                &user,
                "brief.silent_run",
                None,
                Some(&task.id),
                None,
                None,
                Some(self.gateway.policy().version()),
                Some(&details),
            )?;
            self.store
                .append_event(&user, "brief.silent_run", &details)?;
            return Ok(TurnOutcome::Completed);
        }
        self.notifier
            .send_text(&user, &payload.conversation_id, &text)
            .await?;
        self.store.append_event(
            &user,
            "brief.delivered",
            &json!({"task_id": task.id, "conversation_id": payload.conversation_id}),
        )?;
        Ok(TurnOutcome::Completed)
    }

    pub(super) async fn run_watch(&self, task: &Task) -> Result<TurnOutcome> {
        let payload: ScheduledPayload = serde_json::from_value(task.payload.clone())
            .map_err(|error| SeraError::Validation(format!("invalid watch payload: {error}")))?;
        let user = task.user_id.clone();
        let sources =
            crate::sources::effective_sources_for_user(&self.config, &self.store, &user)?;
        if sources.iter().all(|source| !source.enabled) {
            self.notifier
                .send_text(
                    &user,
                    &payload.conversation_id,
                    "No monitoring sources are enabled.",
                )
                .await?;
            return Ok(TurnOutcome::Completed);
        }
        let fetcher = crate::fetch::SafeFetcher::new(
            self.config.watcher.user_agent.clone(),
            self.config.watcher.timeout_secs,
            self.config.watcher.max_response_bytes,
        )?
        .allow_hosts(crate::sources::watcher_allow_hosts(&self.config)?);
        let reports =
            crate::watcher::watch_all(&self.store, &user, &fetcher, &sources, &self.config).await?;
        let mut failures = 0;
        for report in &reports {
            if !report.ok {
                failures += 1;
            }
        }
        // Opt-in silence: zero new/changed items and zero failures across all
        // sources skips the notification; source_runs and the audit ledger are
        // still recorded (watch_all already wrote source_runs).
        if watch_run_is_silent(self.config.watcher.silent_when_empty, &reports) {
            let details = json!({
                "task_id": task.id,
                "conversation_id": payload.conversation_id,
                "sources": reports.len(),
                "failed": failures,
            });
            self.store.record_audit(
                &user,
                "watch.silent_run",
                None,
                Some(&task.id),
                None,
                None,
                Some(self.gateway.policy().version()),
                Some(&details),
            )?;
            self.store
                .append_event(&user, "watch.silent_run", &details)?;
            return Ok(TurnOutcome::Completed);
        }
        let mut text = String::from("Source watch\n");
        for report in &reports {
            text.push_str(&format!(
                "- {}: new {}, changed {}, failed {}, seen {}\n",
                report.source, report.new, report.changed, report.failed, report.items_seen
            ));
            for warning in report.warnings.iter().take(3) {
                text.push_str(&format!("  warning: {}\n", truncate_chars(warning, 300)));
            }
        }
        if self.gateway.scan_egress(&user, &text).is_err() {
            text = "Source watch completed, but the report was withheld by the content policy."
                .to_string();
        }
        self.notifier
            .send_text(&user, &payload.conversation_id, &text)
            .await?;
        self.store.append_event(
            &user,
            "watch.completed",
            &json!({
                "task_id": task.id,
                "sources": reports.len(),
                "failed": failures,
            }),
        )?;
        Ok(TurnOutcome::Completed)
    }
}
