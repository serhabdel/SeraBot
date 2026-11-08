//! The worker loop, maintenance and task dispatch.

use super::*;

/// Aborts the task it holds when dropped. The lease heartbeat is held this
/// way, so a panic in the turn cannot leave it renewing a lease nobody runs.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Runtime {
    /// The turn a task drives, when it has one: a turn task's own turn, or
    /// the turn a resume task continues.
    fn task_turn_id(&self, task: &Task) -> Option<String> {
        match task.kind.as_str() {
            "turn" => self
                .store
                .get_turn_by_task(&task.user_id, &task.id)
                .ok()
                .flatten()
                .map(|turn| turn.id),
            "resume" => serde_json::from_value::<ResumePayload>(task.payload.clone())
                .ok()
                .map(|payload| payload.turn_id),
            _ => None,
        }
    }

    /// Keep a claimed task's lease alive while its turn runs. A tool call can
    /// take longer than one lease, so the lease is renewed every third of its
    /// length. The heartbeat stops when the task leaves `running` (finished,
    /// parked for approval, or reclaimed under a newer fencing token).
    fn spawn_lease_heartbeat(&self, task: &Task) -> tokio::task::JoinHandle<()> {
        let store = self.store.clone();
        let user = task.user_id.clone();
        let id = task.id.clone();
        let token = task.fencing_token;
        let lease_secs = self.config.runtime.task_lease_secs.max(3);
        tokio::spawn(async move {
            let mut beat = tokio::time::interval(std::time::Duration::from_secs(lease_secs / 3));
            // The first tick completes at once; the claim already set a lease.
            beat.tick().await;
            loop {
                beat.tick().await;
                let until = Utc::now() + chrono::Duration::seconds(lease_secs as i64);
                match store.renew_task_lease(&user, &id, token, until) {
                    Ok(true) => {}
                    Ok(false) => {
                        debug!(task = %id, "lease heartbeat stopped: task is no longer running under this claim");
                        break;
                    }
                    Err(error) => error!(task = %id, %error, "failed to renew task lease"),
                }
            }
        })
    }

    pub async fn run_worker(self) {
        if let Err(error) = self.store.reconcile_dispatching_actions() {
            error!(%error, "failed to reconcile dispatching actions");
        }
        info!(worker = %self.worker_id, "worker started");
        let mut maintenance = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                _ = maintenance.tick() => {
                    self.maintenance();
                }
                permit = self.semaphore.clone().acquire_owned() => {
                    let permit = match permit { Ok(permit) => permit, Err(_) => break };
                    match self.store.claim_task(&self.worker_id, self.config.runtime.task_lease_secs) {
                        Ok(Some(task)) => {
                            let runtime = self.clone();
                            tokio::spawn(async move {
                                let _permit = permit;
                                // Dropped on every exit, including a panic.
                                let _heartbeat = AbortOnDrop(runtime.spawn_lease_heartbeat(&task));
                                let limit = std::time::Duration::from_secs(
                                    runtime.config.runtime.turn_max_secs.max(1),
                                );
                                runtime.run_claimed_task(task, limit).await;
                            });
                        }
                        Ok(None) => {
                            drop(permit);
                            tokio::time::sleep(std::time::Duration::from_millis(
                                self.config.runtime.worker_poll_ms.clamp(50, 200),
                            )).await;
                        }
                        Err(error) => {
                            drop(permit);
                            error!(%error, "failed to claim task");
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                    }
                }
            }
        }
        info!("worker stopped");
    }

    /// Run one claimed task under its wall-clock limit. A task that runs past
    /// it is stopped and failed for good: its work may already have had an
    /// external effect, so it is never replayed.
    pub(super) async fn run_claimed_task(&self, task: Task, limit: std::time::Duration) {
        let user = task.user_id.clone();
        let id = task.id.clone();
        let token = task.fencing_token;
        let conversation = match task.kind.as_str() {
            "turn" => serde_json::from_value::<TurnPayload>(task.payload.clone())
                .ok()
                .map(|payload| payload.conversation_id),
            _ => None,
        };
        match tokio::time::timeout(limit, self.process_task(task)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => error!(task = %id, %error, "task processing failed"),
            Err(_) => {
                self.stop_overdue_task(&user, &id, token, conversation.as_deref(), limit)
                    .await
            }
        }
    }

    /// Fail a task that passed its time limit, audit it and tell the owner in
    /// the conversation. The fencing token guards the write: a task another
    /// worker reclaimed is left alone.
    async fn stop_overdue_task(
        &self,
        user: &str,
        id: &str,
        token: i64,
        conversation: Option<&str>,
        limit: std::time::Duration,
    ) {
        error!(task = %id, limit_secs = limit.as_secs(), "task passed its time limit; stopping it");
        // The turn future was dropped before `process_task` could clear its stop mark.
        if let Ok(Some(turn)) = self.store.get_turn_by_task(user, id) {
            self.clear_stop(&turn.id);
        }
        let reason = "the request passed its time limit and was stopped; it is not retried";
        let details = json!({"limit_secs": limit.as_secs()});
        if let Err(error) = self.store.record_audit(
            user,
            "task.timed_out",
            None,
            Some(id),
            None,
            None,
            None,
            Some(&details),
        ) {
            error!(%error, "failed to audit a timed-out task");
        }
        let stopped = match self.store.fail_task_final(user, id, token, reason) {
            Ok(stopped) => stopped,
            Err(error) => {
                error!(task = %id, %error, "failed to fail a timed-out task");
                false
            }
        };
        if !stopped {
            warn!(task = %id, "timed-out task was no longer held under this claim");
            return;
        }
        if let Some(conversation) = conversation {
            let _ = self
                .notifier
                .send_text(
                    user,
                    conversation,
                    "This request ran past its time limit, so I stopped it. Ask again if you still need it.",
                )
                .await;
        }
        if let Err(error) = self.store.append_event(
            user,
            "task.failed",
            &json!({"task_id": id, "error": reason}),
        ) {
            error!(%error, "failed to record a timed-out task");
        }
    }

    fn maintenance(&self) {
        if let Err(error) = self.store.expire_approvals() {
            error!(%error, "failed to expire approvals");
        }
        if let Err(error) = self.store.recover_expired_tasks() {
            error!(%error, "failed to recover expired tasks");
        }
        let retention = self.config.server.event_retention_days.max(1);
        let before = Utc::now() - chrono::Duration::days(retention);
        if let Err(error) = self.store.prune_events(before) {
            error!(%error, "failed to prune events");
        }
        let artifact_retention = self.config.render.artifact_retention_days;
        if artifact_retention > 0 {
            match crate::maker::ArtifactStore::new(self.config.artifacts_dir()) {
                Ok(artifacts) => {
                    let cutoff = Utc::now() - chrono::Duration::days(artifact_retention as i64);
                    match artifacts.prune_older_than(cutoff) {
                        Ok(removed) if removed > 0 => {
                            info!(removed, "pruned expired artifacts");
                        }
                        Ok(_) => {}
                        Err(error) => error!(%error, "failed to prune artifacts"),
                    }
                }
                Err(error) => error!(%error, "failed to open the artifact store for pruning"),
            }
        }
    }

    pub(super) async fn process_task(&self, task: Task) -> Result<()> {
        // The worker executes every task under its owning user (phase 2).
        let user = task.user_id.clone();
        let outcome = match task.kind.as_str() {
            "turn" => self.run_turn(&task).await,
            "resume" => self.resume_turn(&task).await,
            "brief" => self.run_brief(&task).await,
            "watch" => self.run_watch(&task).await,
            other => Err(SeraError::Validation(format!(
                "unsupported task kind in this build: {other}"
            ))),
        };
        // Every exit of the task (completed, parked for approval, failed or
        // superseded) drops the turn's stop mark, so a stop that arrived late
        // cannot linger, and a mark set before an approval pause cannot end
        // the turn after it resumes.
        if let Some(turn_id) = self.task_turn_id(&task) {
            self.clear_stop(&turn_id);
        }
        match outcome {
            Ok(TurnOutcome::Completed) => {
                if !self
                    .store
                    .complete_task(&user, &task.id, task.fencing_token)?
                {
                    warn!(task = %task.id, "task completion rejected by fencing token");
                }
            }
            Ok(TurnOutcome::Suspended) => {
                self.store.record_audit(
                    &user,
                    "task.suspended_for_approval",
                    None,
                    Some(&task.id),
                    None,
                    None,
                    None,
                    None,
                )?;
            }
            Err(error) => {
                error!(task = %task.id, %error, "task failed");
                self.store.record_audit(
                    &user,
                    "task.failed",
                    None,
                    Some(&task.id),
                    None,
                    None,
                    None,
                    Some(&json!({"error": error.to_string()})),
                )?;
                let permanent = matches!(
                    &error,
                    SeraError::Forbidden(_)
                        | SeraError::Validation(_)
                        | SeraError::Config(_)
                        | SeraError::PolicyViolation
                        | SeraError::NotFound(_)
                        | SeraError::InvalidState(_)
                );
                if permanent {
                    self.store.fail_task_final(
                        &user,
                        &task.id,
                        task.fencing_token,
                        &error.to_string(),
                    )?;
                } else {
                    self.store.fail_task(
                        &user,
                        &task.id,
                        task.fencing_token,
                        &error.to_string(),
                        self.config.runtime.task_lease_secs.max(5),
                    )?;
                }
                let payload: Option<TurnPayload> =
                    serde_json::from_value(task.payload.clone()).ok();
                if let Some(payload) = payload {
                    let _ = self
                        .notifier
                        .send_text(
                            &user,
                            &payload.conversation_id,
                            "This request failed. The details are available in the audit log.",
                        )
                        .await;
                }
                self.store.append_event(
                    &user,
                    "task.failed",
                    &json!({"task_id": task.id, "error": error.to_string()}),
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::AbortOnDrop;
    use std::time::Duration;

    #[tokio::test]
    async fn heartbeat_guard_aborts_the_heartbeat_when_the_task_panics() {
        let heartbeat = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let watch = heartbeat.abort_handle();
        let joined = tokio::spawn(async move {
            let _heartbeat = AbortOnDrop(heartbeat);
            panic!("turn panicked");
        })
        .await;
        assert!(joined.is_err_and(|error| error.is_panic()));
        for _ in 0..100 {
            if watch.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(watch.is_finished(), "the heartbeat must stop with the task");
    }

    #[tokio::test]
    async fn heartbeat_guard_aborts_on_a_normal_exit_too() {
        let heartbeat = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let watch = heartbeat.abort_handle();
        drop(AbortOnDrop(heartbeat));
        for _ in 0..100 {
            if watch.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(watch.is_finished());
    }
}
