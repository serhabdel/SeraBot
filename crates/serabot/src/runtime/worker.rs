//! The worker loop, maintenance and task dispatch.

use super::*;

impl Runtime {
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
                                if let Err(error) = runtime.process_task(task).await {
                                    error!(%error, "task processing failed");
                                }
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
