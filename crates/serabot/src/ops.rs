//! Alerts and backups (0.68).
//!
//! **Alerts** reach a member where they already talk to SeraBot: their linked
//! Telegram chat, and optionally one allowed WhatsApp chat they picked. A
//! sweep runs every minute and raises:
//!
//! - `failover`: one model failed over `failover_alert_count` times in 15
//!   minutes (to that member);
//! - `signin`: a provider sign-in expired (to that member);
//! - `tunnel`: the PC tunnel went offline while that member had a turn
//!   running (to that member);
//! - `disk` / `memory`: the server is nearly full (owner only);
//! - `backup`: the nightly backup failed (owner only).
//!
//! Each alert is sent at most once per `alert_cooldown_mins` per member and
//! key, audited as `alert.sent`, and never carries message text.
//!
//! **Backups**: once a day at `backup_hour_utc`, a consistent copy of the
//! database (`VACUUM INTO`) is encrypted with the server's age key, test
//! restored (decrypted into a temporary file and checked with
//! `PRAGMA integrity_check`), optionally copied to the owner's OneDrive, and
//! pruned to `backup_keep` files. The plaintext copy never stays on disk.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Datelike, Timelike, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::{Result, SeraError};
use crate::runtime::Notifier;
use crate::store::Store;

/// Conversation id used for alert delivery. It belongs to no conversation, so
/// Telegram routes it to the member's linked chat as a system notification.
pub const ALERT_CONVERSATION: &str = "alerts:system";
const SWEEP_EVERY: Duration = Duration::from_secs(60);
const FAILOVER_WINDOW_MINS: i64 = 15;
const BACKUP_STATUS_KEY: &str = "ops.backup.status";
const BACKUP_PREFIX: &str = "serabot-";
const BACKUP_SUFFIX: &str = ".db.age";

// ---------------------------------------------------------------------------
// Alert settings
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlertSettings {
    pub telegram: bool,
    /// One of the member's WhatsApp chat conversations, or none.
    pub whatsapp_conversation_id: Option<String>,
}

impl Default for AlertSettings {
    fn default() -> Self {
        Self {
            telegram: true,
            whatsapp_conversation_id: None,
        }
    }
}

/// Switches for the scheduled helpers (0.70). Stored beside the alert
/// settings; a member who never saved any gets the defaults: brief on,
/// triage off, memory review on.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct HelperSettings {
    pub morning_brief: bool,
    /// The member's own hour (UTC); `None` follows `[ops] brief_hour_utc`.
    pub brief_hour_utc: Option<u8>,
    pub email_triage: bool,
    pub triage_hour_utc: Option<u8>,
    pub memory_review: bool,
}

impl Default for HelperSettings {
    fn default() -> Self {
        Self {
            morning_brief: true,
            brief_hour_utc: None,
            email_triage: false,
            triage_hour_utc: None,
            memory_review: true,
        }
    }
}

/// A partial change to [`HelperSettings`]; absent fields stay as they are.
/// An hour of `Some(None)` clears the member's own hour.
#[derive(Clone, Debug, Default)]
pub struct HelperPatch {
    pub morning_brief: Option<bool>,
    pub brief_hour_utc: Option<Option<u8>>,
    pub email_triage: Option<bool>,
    pub triage_hour_utc: Option<Option<u8>>,
    pub memory_review: Option<bool>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SentAlert {
    pub at: String,
    pub kind: String,
    pub text: String,
}

impl Store {
    pub fn alert_settings(&self, user_id: &str) -> Result<AlertSettings> {
        let connection = self.connection()?;
        let row: Option<(i64, Option<String>)> = connection
            .query_row(
                "SELECT telegram, whatsapp_conversation_id FROM alert_settings WHERE user_id = ?1",
                params![user_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row
            .map(|(telegram, whatsapp)| AlertSettings {
                telegram: telegram != 0,
                whatsapp_conversation_id: whatsapp,
            })
            .unwrap_or_default())
    }

    /// Save a member's alert settings. A WhatsApp choice must be one of the
    /// member's own WhatsApp chat conversations.
    pub fn set_alert_settings(&self, user_id: &str, settings: &AlertSettings) -> Result<()> {
        let connection = self.connection()?;
        if let Some(conversation) = &settings.whatsapp_conversation_id {
            let owned: Option<i64> = connection
                .query_row(
                    "SELECT 1 FROM whatsapp_link_chats WHERE user_id = ?1 AND conversation_id = ?2",
                    params![user_id, conversation],
                    |row| row.get(0),
                )
                .optional()?;
            if owned.is_none() {
                return Err(SeraError::Validation(
                    "pick one of your own WhatsApp chats for alerts".into(),
                ));
            }
        }
        connection.execute(
            "INSERT INTO alert_settings(user_id, telegram, whatsapp_conversation_id, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO UPDATE SET telegram = excluded.telegram,
                whatsapp_conversation_id = excluded.whatsapp_conversation_id,
                updated_at = excluded.updated_at",
            params![
                user_id,
                i64::from(settings.telegram),
                settings.whatsapp_conversation_id,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// The member's scheduled-helper switches.
    pub fn helper_settings(&self, user_id: &str) -> Result<HelperSettings> {
        let connection = self.connection()?;
        #[allow(clippy::type_complexity)]
        let row: Option<(i64, Option<i64>, i64, Option<i64>, i64)> = connection
            .query_row(
                "SELECT morning_brief, brief_hour_utc, email_triage, triage_hour_utc, memory_review
                 FROM alert_settings WHERE user_id = ?1",
                params![user_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let hour = |value: Option<i64>| value.and_then(|hour| u8::try_from(hour).ok());
        Ok(row
            .map(
                |(brief, brief_hour, triage, triage_hour, review)| HelperSettings {
                    morning_brief: brief != 0,
                    brief_hour_utc: hour(brief_hour),
                    email_triage: triage != 0,
                    triage_hour_utc: hour(triage_hour),
                    memory_review: review != 0,
                },
            )
            .unwrap_or_default())
    }

    pub fn set_helper_settings(&self, user_id: &str, patch: &HelperPatch) -> Result<()> {
        for hour in [patch.brief_hour_utc, patch.triage_hour_utc]
            .into_iter()
            .flatten()
            .flatten()
        {
            if hour > 23 {
                return Err(SeraError::Validation("hour must be 0 to 23".into()));
            }
        }
        let mut next = self.helper_settings(user_id)?;
        if let Some(value) = patch.morning_brief {
            next.morning_brief = value;
        }
        if let Some(value) = patch.brief_hour_utc {
            next.brief_hour_utc = value;
        }
        if let Some(value) = patch.email_triage {
            next.email_triage = value;
        }
        if let Some(value) = patch.triage_hour_utc {
            next.triage_hour_utc = value;
        }
        if let Some(value) = patch.memory_review {
            next.memory_review = value;
        }
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO alert_settings(user_id, telegram, whatsapp_conversation_id, updated_at,
                 morning_brief, brief_hour_utc, email_triage, triage_hour_utc, memory_review)
             VALUES (?1, 1, NULL, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(user_id) DO UPDATE SET morning_brief = excluded.morning_brief,
                brief_hour_utc = excluded.brief_hour_utc, email_triage = excluded.email_triage,
                triage_hour_utc = excluded.triage_hour_utc, memory_review = excluded.memory_review,
                updated_at = excluded.updated_at",
            params![
                user_id,
                Utc::now().to_rfc3339(),
                i64::from(next.morning_brief),
                next.brief_hour_utc.map(i64::from),
                i64::from(next.email_triage),
                next.triage_hour_utc.map(i64::from),
                i64::from(next.memory_review),
            ],
        )?;
        Ok(())
    }

    /// The member's WhatsApp chats that can receive alerts: (conversation, jid).
    pub fn alert_whatsapp_choices(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT conversation_id, jid FROM whatsapp_link_chats WHERE user_id = ?1
             ORDER BY created_at DESC LIMIT 50",
        )?;
        let rows = statement.query_map(params![user_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Whether files may go to the member's own chats without an approval
    /// card when they ask for them (default on).
    pub fn auto_send_own(&self, user_id: &str) -> Result<bool> {
        Ok(self.kv_get(&format!("auto_send_own:{user_id}"))?.as_deref() != Some("0"))
    }

    pub fn set_auto_send_own(&self, user_id: &str, on: bool) -> Result<()> {
        self.kv_set(
            &format!("auto_send_own:{user_id}"),
            if on { "1" } else { "0" },
        )
    }

    /// The member's most recent alerts.
    pub fn recent_alerts(&self, user_id: &str, limit: u32) -> Result<Vec<SentAlert>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT at, COALESCE(json_extract(details_json, '$.kind'), ''),
                    COALESCE(json_extract(details_json, '$.text'), '')
             FROM audit_log WHERE actor = ?1 AND event = 'alert.sent'
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![user_id, limit.min(100)], |row| {
            Ok(SentAlert {
                at: row.get(0)?,
                kind: row.get(1)?,
                text: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

// ---------------------------------------------------------------------------
// Alert delivery
// ---------------------------------------------------------------------------

/// One alert to one member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub user_id: String,
    pub kind: &'static str,
    /// Dedup key within the kind (a model, a provider, a host…).
    pub key: String,
    pub text: String,
}

pub struct Alerts {
    store: Store,
    notifier: Arc<dyn Notifier>,
    cooldown: chrono::Duration,
}

impl Alerts {
    pub fn new(store: Store, notifier: Arc<dyn Notifier>, cooldown_mins: u32) -> Self {
        Self {
            store,
            notifier,
            cooldown: chrono::Duration::minutes(i64::from(cooldown_mins.max(1))),
        }
    }

    /// Send an alert unless the same one went out within the cooldown.
    /// Returns whether it was sent.
    pub async fn raise(&self, alert: &Alert) -> Result<bool> {
        let cooldown_key = format!("ops.alert.{}.{}.{}", alert.user_id, alert.kind, alert.key);
        let now = Utc::now();
        if let Some(last) = self.store.kv_get(&cooldown_key)?
            && let Ok(last) = DateTime::parse_from_rfc3339(&last)
            && now - last.with_timezone(&Utc) < self.cooldown
        {
            return Ok(false);
        }
        self.store.kv_set(&cooldown_key, &now.to_rfc3339())?;
        self.deliver(alert, &format!("⚠️ SeraBot: {}", alert.text))
            .await?;
        Ok(true)
    }

    /// Deliver `text` on the member's alert channels and audit it.
    pub async fn deliver(&self, alert: &Alert, text: &str) -> Result<()> {
        let text = text.to_string();
        let settings = self.store.alert_settings(&alert.user_id)?;
        let mut channels = Vec::new();
        if settings.telegram {
            match self
                .notifier
                .send_text(&alert.user_id, ALERT_CONVERSATION, &text)
                .await
            {
                Ok(()) => channels.push("telegram"),
                Err(error) => tracing::warn!(%error, kind = alert.kind, "alert not delivered"),
            }
        }
        if let Some(conversation) = &settings.whatsapp_conversation_id {
            match self
                .notifier
                .send_text(&alert.user_id, conversation, &text)
                .await
            {
                Ok(()) => channels.push("whatsapp"),
                Err(error) => tracing::warn!(%error, kind = alert.kind, "alert not delivered"),
            }
        }
        self.store.record_audit(
            &alert.user_id,
            "alert.sent",
            None,
            None,
            None,
            None,
            None,
            Some(&json!({
                "kind": alert.kind,
                "key": alert.key,
                "text": alert.text,
                "channels": channels,
            })),
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Alert sweep
// ---------------------------------------------------------------------------

/// Failover counts per (member, provider, model) over the last 15 minutes.
pub fn failover_streaks(
    store: &Store,
    now: DateTime<Utc>,
    threshold: u32,
) -> Result<Vec<(String, String, String, i64)>> {
    let connection = store.connection()?;
    let since = (now - chrono::Duration::minutes(FAILOVER_WINDOW_MINS)).to_rfc3339();
    let mut statement = connection.prepare(
        "SELECT actor,
                COALESCE(json_extract(details_json, '$.from.provider'), ''),
                COALESCE(json_extract(details_json, '$.from.model'), ''),
                COUNT(*)
         FROM audit_log WHERE event = 'model.failover' AND at >= ?1
         GROUP BY 1, 2, 3 HAVING COUNT(*) >= ?2",
    )?;
    let rows = statement.query_map(params![since, i64::from(threshold.max(1))], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Sign-ins that expired since audit row `after`: (row id, member, provider).
pub fn expired_signins(store: &Store, after: i64) -> Result<Vec<(i64, String, String)>> {
    let connection = store.connection()?;
    let mut statement = connection.prepare(
        "SELECT id, actor, COALESCE(json_extract(details_json, '$.provider'), '')
         FROM audit_log WHERE event = 'provider.account_needs_reauth' AND id > ?1
         ORDER BY id LIMIT 200",
    )?;
    let rows = statement.query_map(params![after], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn running_turns(store: &Store, user_id: &str) -> Result<i64> {
    let connection = store.connection()?;
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM tasks WHERE user_id = ?1 AND status = 'running'",
        params![user_id],
        |row| row.get(0),
    )?)
}

/// Whole-number percent of `used` in `total`.
pub fn percent(used: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    ((used as f64 * 100.0 / total as f64).round() as u64).min(100) as u8
}

pub struct OpsWatch {
    config: Arc<Config>,
    store: Store,
    alerts: Alerts,
    monitor: Arc<crate::sysstatus::StatusMonitor>,
    onedrive: Option<Arc<crate::onedrive::OneDriveRegistry>>,
    /// Members whose relay was online at the previous sweep.
    relay_online: Mutex<HashMap<String, bool>>,
    /// Held while a backup runs, so a manual run and the nightly one never
    /// overlap.
    backup_lock: tokio::sync::Mutex<()>,
    /// Set after start-up (0.70): the runtime that takes the triage turn and
    /// scans outgoing text, and the members' own connectors for the brief.
    runtime: std::sync::OnceLock<crate::runtime::Runtime>,
    personal: std::sync::OnceLock<Arc<crate::personal::PersonalConnectors>>,
    /// Fetcher for the members' job watches (0.70); `None` if it cannot be built.
    job_fetcher: Option<crate::fetch::SafeFetcher>,
    /// True while a job sweep runs in the background.
    jobs_busy: std::sync::atomic::AtomicBool,
}

impl OpsWatch {
    pub fn new(
        config: Arc<Config>,
        store: Store,
        notifier: Arc<dyn Notifier>,
        onedrive: Option<Arc<crate::onedrive::OneDriveRegistry>>,
    ) -> Arc<Self> {
        let cooldown = config.ops.alert_cooldown_mins;
        Arc::new(Self {
            alerts: Alerts::new(store.clone(), notifier, cooldown),
            job_fetcher: crate::job_watch::job_fetcher(&config).ok(),
            jobs_busy: std::sync::atomic::AtomicBool::new(false),
            config,
            store,
            monitor: Arc::new(crate::sysstatus::StatusMonitor::new()),
            onedrive,
            relay_online: Mutex::new(HashMap::new()),
            backup_lock: tokio::sync::Mutex::new(()),
            runtime: std::sync::OnceLock::new(),
            personal: std::sync::OnceLock::new(),
        })
    }

    /// Run the minute sweep and the nightly backup until shutdown.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        let mut ticker = tokio::time::interval(SWEEP_EVERY);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ticker.tick() => {}
            }
            if self.config.ops.alerts_enabled
                && let Err(error) = self.sweep(Utc::now()).await
            {
                tracing::warn!(%error, "alert sweep failed");
            }
            if self.config.ops.backup_enabled && self.backup_due(Utc::now()) {
                let _ = self.run_backup("schedule").await;
            }
            self.start_job_sweep(Utc::now());
            if let Some(week) = self.digest_due(Utc::now()) {
                let _ = self.store.kv_set("ops.digest.week", &week);
                if let Err(error) = self.send_digests(Utc::now()).await {
                    tracing::warn!(%error, "weekly digest failed");
                }
            }
            self.run_helpers(Utc::now()).await;
        }
    }

    /// Give the watch the runtime and the members' connectors (0.70).
    pub fn attach(
        &self,
        runtime: crate::runtime::Runtime,
        personal: Arc<crate::personal::PersonalConnectors>,
    ) {
        let _ = self.runtime.set(runtime);
        let _ = self.personal.set(personal);
    }

    /// Morning brief, email triage and memory review for every member whose
    /// hour has come. Each is claimed once per day (or week) before it runs,
    /// so a restart or a slow send never repeats it.
    pub async fn run_helpers(&self, now: DateTime<Utc>) {
        if let Err(error) = self.send_due_briefs(now).await {
            tracing::warn!(%error, "morning briefs failed");
        }
        if let Err(error) = self.submit_due_triage(now) {
            tracing::warn!(%error, "email triage failed");
        }
        if let Err(error) = self.send_due_memory_reviews(now).await {
            tracing::warn!(%error, "memory review failed");
        }
    }

    async fn send_due_briefs(&self, now: DateTime<Utc>) -> Result<usize> {
        let ops = &self.config.ops;
        if !ops.brief_enabled {
            return Ok(0);
        }
        let mut sent = 0;
        for user in self.store.list_users()? {
            let settings = self.store.helper_settings(&user.id)?;
            if !settings.morning_brief
                || !hour_due(settings.brief_hour_utc, ops.brief_hour_utc, now)
                || !claim_daily(&self.store, "brief", &user.id, now)?
            {
                continue;
            }
            if let Err(error) = self.send_brief_to(&user.id, now).await {
                tracing::warn!(%error, "morning brief not sent");
            } else {
                sent += 1;
            }
        }
        Ok(sent)
    }

    /// Build and deliver one member's brief now. Returns the text.
    pub async fn send_brief_to(&self, user_id: &str, now: DateTime<Utc>) -> Result<String> {
        let text = member_brief(
            &self.store,
            &self.config,
            self.personal.get().map(|p| p.as_ref()),
            self.runtime.get(),
            user_id,
            now,
        )
        .await?;
        let alert = Alert {
            user_id: user_id.to_string(),
            kind: "brief",
            key: now.format("%Y-%m-%d").to_string(),
            text: text.clone(),
        };
        self.alerts.deliver(&alert, &text).await?;
        Ok(text)
    }

    fn submit_due_triage(&self, now: DateTime<Utc>) -> Result<usize> {
        let Some(runtime) = self.runtime.get() else {
            return Ok(0);
        };
        let personal = self.personal.get();
        triage_sweep(
            &self.store,
            &self.config,
            now,
            &|user| {
                personal.is_some_and(|personal| {
                    use crate::connectors::ConnectorKind;
                    [ConnectorKind::OutlookMail, ConnectorKind::Gmail]
                        .into_iter()
                        .any(|kind| personal.connected(user, kind).unwrap_or(false))
                })
            },
            &mut |user| {
                runtime
                    .submit_text(user, TRIAGE_CONVERSATION, TRIAGE_PROMPT, Some(TRIAGE_BOT))
                    .map(|_| ())
            },
        )
    }

    async fn send_due_memory_reviews(&self, now: DateTime<Utc>) -> Result<usize> {
        let ops = &self.config.ops;
        if !ops.memory_review_enabled
            || now.weekday().number_from_monday()
                != u32::from(ops.memory_review_weekday.clamp(1, 7))
            || now.hour() != u32::from(ops.memory_review_hour_utc.min(23))
        {
            return Ok(0);
        }
        let week = now.format("%G-W%V").to_string();
        let mut sent = 0;
        for user in self.store.list_users()? {
            if !self.store.helper_settings(&user.id)?.memory_review {
                continue;
            }
            let key = format!("ops.memreview.{}", user.id);
            if self.store.kv_get(&key)?.as_deref() == Some(week.as_str()) {
                continue;
            }
            self.store.kv_set(&key, &week)?;
            let found = crate::memory_review::candidates(
                &self.store,
                &user.id,
                now,
                crate::memory_review::MAX_CANDIDATES,
            )?;
            if found.is_empty() {
                continue;
            }
            let text = crate::memory_review::review_text(&found);
            let alert = Alert {
                user_id: user.id.clone(),
                kind: "memory_review",
                key: week.clone(),
                text: text.clone(),
            };
            self.alerts.deliver(&alert, &text).await?;
            sent += 1;
        }
        Ok(sent)
    }

    /// 0.70: start the job watches that are due, in the background (a watch
    /// pauses a few seconds between sources, so it never blocks this loop).
    fn start_job_sweep(self: &Arc<Self>, now: DateTime<Utc>) {
        use std::sync::atomic::Ordering;
        if self.job_fetcher.is_none() {
            return;
        }
        let due = match crate::job_watch::due_members(&self.store, now) {
            Ok(due) if !due.is_empty() => due,
            _ => return,
        };
        if self.jobs_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            for user_id in due {
                if let Err(error) = this.watch_jobs_for(&user_id).await {
                    tracing::warn!(%error, "job watch failed");
                }
            }
            this.jobs_busy.store(false, Ordering::SeqCst);
        });
    }

    /// Run one member's scheduled job watch and send what is new.
    pub async fn watch_jobs_for(&self, user_id: &str) -> Result<usize> {
        let Some(fetcher) = &self.job_fetcher else {
            return Ok(0);
        };
        let report = match crate::job_watch::run_watch(
            &self.store,
            fetcher,
            &self.config.watcher.user_agent,
            user_id,
            crate::job_watch::RunMode::Scheduled,
            None,
            crate::job_watch::SOURCE_DELAY,
        )
        .await
        {
            Ok(report) => report,
            Err(error) => {
                // Do not retry every minute.
                let _ = self.store.set_job_watch_state(
                    user_id,
                    "scheduled",
                    &json!({"error": error.to_string()}),
                );
                return Err(error);
            }
        };
        if report.new_matches.is_empty() {
            return Ok(0);
        }
        let text = crate::job_watch::format_matches(&report.new_matches);
        let alert = Alert {
            user_id: user_id.to_string(),
            kind: "jobs",
            key: report.checked_at.clone(),
            text: text.clone(),
        };
        self.alerts.deliver(&alert, &text).await?;
        Ok(report.new_matches.len())
    }

    /// One alert sweep.
    pub async fn sweep(&self, now: DateTime<Utc>) -> Result<()> {
        let ops = &self.config.ops;
        for (user, provider, model, count) in
            failover_streaks(&self.store, now, ops.failover_alert_count)?
        {
            let name = if model.is_empty() { &provider } else { &model };
            self.alerts
                .raise(&Alert {
                    user_id: user,
                    kind: "failover",
                    key: format!("{provider}/{model}"),
                    text: format!(
                        "{name} failed {count} times in the last {FAILOVER_WINDOW_MINS} minutes; \
                         your bots moved to the next model. Check the provider's status or your \
                         account under Settings → Providers."
                    ),
                })
                .await?;
        }
        let cursor_key = "ops.alert.signin_cursor";
        let cursor = match self.store.kv_get(cursor_key)? {
            Some(value) => value.parse().unwrap_or(0),
            // First run: start from now, never alert on old history.
            None => self.latest_audit_id()?,
        };
        let mut last = cursor;
        for (id, user, provider) in expired_signins(&self.store, cursor)? {
            last = last.max(id);
            let label = crate::providers::canonical_kind(&provider).unwrap_or(provider);
            self.alerts
                .raise(&Alert {
                    user_id: user,
                    kind: "signin",
                    key: label.clone(),
                    text: format!(
                        "your {label} sign-in expired. Sign in again under Settings → Providers \
                         so your bots can use it."
                    ),
                })
                .await?;
        }
        self.store.kv_set(cursor_key, &last.to_string())?;
        self.sweep_tunnels().await?;
        self.sweep_host().await?;
        self.sweep_budgets(now).await?;
        Ok(())
    }

    /// 0.69: warn a member at 80% and at 100% of their monthly budget, once
    /// per month each.
    async fn sweep_budgets(&self, now: DateTime<Utc>) -> Result<()> {
        let month = now.format("%Y-%m").to_string();
        for user in self.store.list_users()? {
            let Some(budget) = self.store.monthly_budget(&user.id)? else {
                continue;
            };
            let report = self
                .store
                .cost_report(Some(&user.id), crate::costs::month_start(now))?;
            let Some(level) = crate::costs::budget_level(report.usd, budget) else {
                continue;
            };
            let key = format!("ops.budget.{}.{month}.{level}", user.id);
            if self.store.kv_get(&key)?.is_some() {
                continue;
            }
            self.store.kv_set(&key, &now.to_rfc3339())?;
            let text = if level >= 100 {
                format!(
                    "you have used your monthly budget: about {} of {} this month (estimate). \
                     Bots keep working; change the budget or the models under Usage.",
                    crate::costs::usd(report.usd),
                    crate::costs::usd(budget)
                )
            } else {
                format!(
                    "you are at {}% of your monthly budget: about {} of {} (estimate).",
                    level,
                    crate::costs::usd(report.usd),
                    crate::costs::usd(budget)
                )
            };
            let alert = Alert {
                user_id: user.id.clone(),
                kind: "budget",
                key: format!("{month}-{level}"),
                text: text.clone(),
            };
            self.alerts
                .deliver(&alert, &format!("⚠️ SeraBot: {text}"))
                .await?;
        }
        Ok(())
    }

    fn digest_due(&self, now: DateTime<Utc>) -> Option<String> {
        let ops = &self.config.ops;
        if !ops.digest_enabled
            || now.weekday().number_from_monday() != u32::from(ops.digest_weekday.clamp(1, 7))
            || now.hour() != u32::from(ops.digest_hour_utc.min(23))
        {
            return None;
        }
        let week = now.format("%G-W%V").to_string();
        let sent = self.store.kv_get("ops.digest.week").ok().flatten();
        (sent.as_deref() != Some(week.as_str())).then_some(week)
    }

    /// Send one digest text to one member now (the console's preview).
    pub async fn send_digest_to(&self, user_id: &str, text: &str) -> Result<()> {
        let alert = Alert {
            user_id: user_id.to_string(),
            kind: "digest",
            key: "preview".into(),
            text: text.to_string(),
        };
        self.alerts.deliver(&alert, text).await
    }

    /// Send the weekly digest to every member who keeps it on.
    pub async fn send_digests(&self, now: DateTime<Utc>) -> Result<usize> {
        let admin = self.store.admin_user_id()?;
        let mut sent = 0;
        for user in self.store.list_users()? {
            if !self.store.weekly_digest(&user.id)? {
                continue;
            }
            let text = weekly_digest_text(&self.store, &user.id, user.id == admin, now)?;
            let alert = Alert {
                user_id: user.id.clone(),
                kind: "digest",
                key: now.format("%G-W%V").to_string(),
                text: text.clone(),
            };
            self.alerts.deliver(&alert, &text).await?;
            sent += 1;
        }
        Ok(sent)
    }

    fn latest_audit_id(&self) -> Result<i64> {
        let connection = self.store.connection()?;
        Ok(
            connection.query_row("SELECT COALESCE(MAX(id), 0) FROM audit_log", [], |row| {
                row.get(0)
            })?,
        )
    }

    async fn sweep_tunnels(&self) -> Result<()> {
        let Some(hub) = crate::relay_net::hub() else {
            return Ok(());
        };
        let users = self.store.list_users()?;
        let mut dropped = Vec::new();
        {
            let mut seen = self.relay_online.lock().unwrap_or_else(|p| p.into_inner());
            for user in &users {
                let online = hub.online(&user.id);
                let was = seen.insert(user.id.clone(), online).unwrap_or(false);
                if was && !online {
                    dropped.push(user.id.clone());
                }
            }
        }
        for user in dropped {
            let enabled = self
                .store
                .relay_settings(&user)
                .is_ok_and(|settings| settings.enabled);
            if !enabled || running_turns(&self.store, &user)? == 0 {
                continue;
            }
            self.alerts
                .raise(&Alert {
                    user_id: user,
                    kind: "tunnel",
                    key: "desktop".into(),
                    text: "the PC tunnel went offline while a bot was working. The turn moves \
                           to the server's network or the next model; open the desktop app to \
                           relay again."
                        .into(),
                })
                .await?;
        }
        Ok(())
    }

    async fn sweep_host(&self) -> Result<()> {
        let monitor = self.monitor.clone();
        let data_dir = self
            .config
            .database_path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let snapshot = tokio::task::spawn_blocking(move || monitor.host_snapshot(&data_dir))
            .await
            .map_err(|error| SeraError::Internal(format!("host sample failed: {error}")))?;
        let admin = self.store.admin_user_id()?;
        let ops = &self.config.ops;
        if let Some(disk) = &snapshot.disk {
            let used = percent(disk.used_bytes, disk.total_bytes);
            if used >= ops.disk_alert_percent {
                self.alerts
                    .raise(&Alert {
                        user_id: admin.clone(),
                        kind: "disk",
                        key: "data".into(),
                        text: format!(
                            "the server's data disk is {used}% full ({} GB free). Free space \
                             or old backups soon; SeraBot stops saving when it is full.",
                            disk.free_bytes / 1_000_000_000
                        ),
                    })
                    .await?;
            }
        }
        let memory = percent(
            snapshot.host_memory_used_bytes,
            snapshot.host_memory_total_bytes,
        );
        if memory >= ops.memory_alert_percent {
            self.alerts
                .raise(&Alert {
                    user_id: admin,
                    kind: "memory",
                    key: "host".into(),
                    text: format!(
                        "the server's memory is {memory}% used (SeraBot itself uses {} MB).",
                        snapshot.process_rss_bytes / 1_000_000
                    ),
                })
                .await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Backups
    // -----------------------------------------------------------------------

    fn backup_dir(&self) -> PathBuf {
        self.config.path(&self.config.ops.backup_dir)
    }

    fn backup_due(&self, now: DateTime<Utc>) -> bool {
        if now.hour() != u32::from(self.config.ops.backup_hour_utc.min(23)) {
            return false;
        }
        let today = now.format("%Y-%m-%d").to_string();
        backup_status(&self.store)
            .ok()
            .flatten()
            .is_none_or(|status| !status.at.starts_with(&today))
    }

    /// Whether a backup is running right now.
    pub fn backup_in_progress(&self) -> bool {
        self.backup_lock.try_lock().is_err()
    }

    /// Back up, test restore, copy to OneDrive, prune. Records the outcome in
    /// `ops.backup.status` and the audit log, and alerts the owner on failure.
    pub async fn run_backup(&self, reason: &str) -> Result<BackupStatus> {
        let Ok(_guard) = self.backup_lock.try_lock() else {
            return Err(SeraError::InvalidState(
                "a backup is already running".into(),
            ));
        };
        let admin = self.store.admin_user_id()?;
        let result = self.backup_once(&admin).await;
        let status = match result {
            Ok(status) => status,
            Err(error) => BackupStatus {
                at: Utc::now().to_rfc3339(),
                ok: false,
                error: Some(error.to_string()),
                ..BackupStatus::default()
            },
        };
        self.store
            .kv_set(BACKUP_STATUS_KEY, &serde_json::to_string(&status)?)?;
        self.store.record_audit(
            &admin,
            if status.ok {
                "backup.completed"
            } else {
                "backup.failed"
            },
            None,
            None,
            None,
            None,
            None,
            Some(&json!({
                "reason": reason,
                "file": status.file,
                "bytes": status.bytes,
                "restore_ok": status.restore_ok,
                "onedrive": status.onedrive,
                "error": status.error,
            })),
        )?;
        if !status.ok {
            let _ = self
                .alerts
                .raise(&Alert {
                    user_id: admin,
                    kind: "backup",
                    key: "nightly".into(),
                    text: format!(
                        "the database backup failed: {}",
                        status.error.as_deref().unwrap_or("unknown error")
                    ),
                })
                .await;
        }
        Ok(status)
    }

    async fn backup_once(&self, admin: &str) -> Result<BackupStatus> {
        let dir = self.backup_dir();
        let store = self.store.clone();
        let identity = self.config.identity_path();
        let keep = self.config.ops.backup_keep.max(1) as usize;
        let mut status = tokio::task::spawn_blocking(move || {
            write_backup(&store, &dir, &identity, keep, Utc::now())
        })
        .await
        .map_err(|error| SeraError::Internal(format!("backup task failed: {error}")))??;
        status.onedrive = self.copy_to_onedrive(admin, &status).await;
        Ok(status)
    }

    async fn copy_to_onedrive(&self, admin: &str, status: &BackupStatus) -> String {
        if !self.config.ops.backup_onedrive {
            return "off".into();
        }
        let Some(registry) = &self.onedrive else {
            return "OneDrive is not set up on this server".into();
        };
        if !registry.connected(admin).unwrap_or(false) {
            return "OneDrive is not connected for the owner".into();
        }
        let path = self.backup_dir().join(&status.file);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) => return format!("failed: could not read the backup ({error})"),
        };
        let client = match registry.client_for(admin) {
            Ok(client) => client,
            Err(error) => return format!("failed: {error}"),
        };
        let folder = self.config.ops.backup_onedrive_folder.clone();
        match client
            .lock()
            .await
            .upload(
                &status.file,
                bytes,
                "application/octet-stream",
                Some(folder.as_str()),
            )
            .await
        {
            Ok(_) => "uploaded".into(),
            Err(error) => format!("failed: {error}"),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupStatus {
    pub at: String,
    pub ok: bool,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub bytes: u64,
    /// The encrypted copy decrypted and passed `PRAGMA integrity_check`.
    #[serde(default)]
    pub restore_ok: bool,
    #[serde(default)]
    pub restore_detail: String,
    /// "uploaded", "off", or why it was not copied.
    #[serde(default)]
    pub onedrive: String,
    #[serde(default)]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Scheduled helpers (0.70)
// ---------------------------------------------------------------------------

/// The inbox bot's primary chat; the triage turn lands here.
pub const TRIAGE_CONVERSATION: &str = "dir:inbox";
pub const TRIAGE_BOT: &str = "inbox";
/// The scheduled turn. The normal gateway applies: drafts are shown, and
/// sending anything stays approval-gated.
pub const TRIAGE_PROMPT: &str = "Daily inbox triage: look at my mail from the last 24 hours. \
Group it as Important, Needs reply, FYI and Noise, one line each (sender, subject, the ask). \
For the Needs reply group, draft a short reply for each, shown in full for my approval. \
Do not send, archive or delete anything.";

/// Whether this hour is the member's (or the default) hour.
pub fn hour_due(own: Option<u8>, default: u8, now: DateTime<Utc>) -> bool {
    now.hour() == u32::from(own.unwrap_or(default).min(23))
}

/// Claim today's run of `kind` for a member: true the first time per UTC day.
pub fn claim_daily(store: &Store, kind: &str, user_id: &str, now: DateTime<Utc>) -> Result<bool> {
    let key = format!("ops.{kind}.day.{user_id}");
    let day = now.format("%Y-%m-%d").to_string();
    if store.kv_get(&key)?.as_deref() == Some(day.as_str()) {
        return Ok(false);
    }
    store.kv_set(&key, &day)?;
    Ok(true)
}

/// Submit the triage turn once per day for each member who turned it on, has
/// a readable mailbox and whose hour it is. `submit` is the only effect.
pub fn triage_sweep(
    store: &Store,
    config: &Config,
    now: DateTime<Utc>,
    mailbox: &dyn Fn(&str) -> bool,
    submit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<usize> {
    let ops = &config.ops;
    if !ops.triage_enabled {
        return Ok(0);
    }
    let mut submitted = 0;
    for user in store.list_users()? {
        let settings = store.helper_settings(&user.id)?;
        if !settings.email_triage
            || !hour_due(settings.triage_hour_utc, ops.triage_hour_utc, now)
            || !mailbox(&user.id)
            || !claim_daily(store, "triage", &user.id, now)?
        {
            continue;
        }
        match submit(&user.id) {
            Ok(()) => {
                submitted += 1;
                store.record_audit(
                    &user.id,
                    "triage.scheduled",
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )?;
            }
            Err(error) => tracing::warn!(%error, "triage turn not queued"),
        }
    }
    Ok(submitted)
}

/// One member's morning brief text: their rows, plus calendar and mail from
/// their own connectors when reachable. If the content policy blocks the
/// text, the connector-fed lines are dropped and the rows alone are checked.
pub async fn member_brief(
    store: &Store,
    config: &Config,
    personal: Option<&crate::personal::PersonalConnectors>,
    runtime: Option<&crate::runtime::Runtime>,
    user_id: &str,
    now: DateTime<Utc>,
) -> Result<String> {
    let tz: chrono_tz::Tz = config
        .schedule
        .timezone
        .parse()
        .map_err(|_| SeraError::Config("invalid schedule timezone".into()))?;
    let feeds = match personal {
        Some(personal) => crate::brief::collect_feeds(personal, user_id, now, tz).await,
        None => crate::brief::BriefFeeds::default(),
    };
    let text = crate::brief::morning_brief_text(store, user_id, now, tz, &feeds)?;
    if let Some(runtime) = runtime
        && runtime.scan_owner_output(user_id, &text).is_err()
    {
        let bare = crate::brief::morning_brief_text(store, user_id, now, tz, &Default::default())?;
        if runtime.scan_owner_output(user_id, &bare).is_ok() {
            return Ok(bare);
        }
        return Ok("The morning brief was withheld because it violated the content policy.".into());
    }
    Ok(text)
}

/// The weekly digest for one member: their last 7 days in a few lines.
/// Counts and sizes only, never message text.
pub fn weekly_digest_text(
    store: &Store,
    user_id: &str,
    owner: bool,
    now: DateTime<Utc>,
) -> Result<String> {
    let since = now - chrono::Duration::days(7);
    let usage = store.usage_rollup_for(Some(user_id), since, None, None)?;
    let cost = store.cost_report(Some(user_id), since)?;
    // One connection for the counts, released before the next store calls.
    let (failovers, signins, failed) = {
        let connection = store.connection()?;
        let count =
            |sql: &str| -> Result<i64> {
                Ok(connection
                    .query_row(sql, params![user_id, since.to_rfc3339()], |row| row.get(0))?)
            };
        (
            count(
                "SELECT COUNT(*) FROM audit_log WHERE actor = ?1 AND event = 'model.failover' AND at >= ?2",
            )?,
            count(
                "SELECT COUNT(*) FROM audit_log WHERE actor = ?1 AND event = 'provider.account_needs_reauth' AND at >= ?2",
            )?,
            count(
                "SELECT COUNT(*) FROM tasks WHERE user_id = ?1 AND status = 'failed' AND created_at >= ?2",
            )?,
        )
    };
    let mut lines = vec![
        "📊 SeraBot — your week".to_string(),
        format!(
            "• {} replies, {} tokens",
            usage.totals.calls, usage.totals.total_tokens
        ),
    ];
    let mut cost_line = format!("• Estimated cost: {}", crate::costs::usd(cost.usd));
    if cost.unpriced_tokens > 0 {
        cost_line.push_str(&format!(
            " (+{} tokens without a price)",
            cost.unpriced_tokens
        ));
    }
    if let Some(budget) = store.monthly_budget(user_id)? {
        let month = store.cost_report(Some(user_id), crate::costs::month_start(now))?;
        cost_line.push_str(&format!(
            "; this month {} of {}",
            crate::costs::usd(month.usd),
            crate::costs::usd(budget)
        ));
    }
    lines.push(cost_line);
    if let Some(top) = usage.per_model.first() {
        lines.push(format!("• Most used model: {}", top.model));
    }
    lines.push(format!(
        "• {failed} failed tasks, {failovers} model failovers, {signins} expired sign-ins"
    ));
    if owner {
        match backup_status(store)? {
            Some(status) if status.ok => lines.push(format!(
                "• Last backup {}: test restore passed",
                status.at.get(..10).unwrap_or(&status.at)
            )),
            Some(status) => lines.push(format!(
                "• Last backup failed: {}",
                status.error.unwrap_or_default()
            )),
            None => lines.push("• No backup yet".into()),
        }
    }
    Ok(lines.join("\n"))
}

pub fn backup_status(store: &Store) -> Result<Option<BackupStatus>> {
    Ok(store
        .kv_get(BACKUP_STATUS_KEY)?
        .and_then(|raw| serde_json::from_str(&raw).ok()))
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BackupFile {
    pub file: String,
    pub bytes: u64,
}

/// Encrypted backups in `dir`, newest first.
pub fn list_backups(dir: &Path) -> Vec<BackupFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<BackupFile> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            (name.starts_with(BACKUP_PREFIX) && name.ends_with(BACKUP_SUFFIX)).then(|| BackupFile {
                bytes: entry.metadata().map(|meta| meta.len()).unwrap_or(0),
                file: name,
            })
        })
        .collect();
    files.sort_by(|a, b| b.file.cmp(&a.file));
    files
}

fn load_identity(path: &Path) -> Result<age::x25519::Identity> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| SeraError::Secrets(format!("backup key unavailable: {error}")))?;
    age::x25519::Identity::from_str(raw.trim())
        .map_err(|error| SeraError::Secrets(format!("invalid age identity: {error}")))
}

#[cfg(unix)]
fn owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn owner_only(_path: &Path) -> Result<()> {
    Ok(())
}

/// Removes a plaintext temporary file however the backup ends.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Blocking: copy, encrypt, test restore and prune one backup.
pub fn write_backup(
    store: &Store,
    dir: &Path,
    identity_path: &Path,
    keep: usize,
    now: DateTime<Utc>,
) -> Result<BackupStatus> {
    std::fs::create_dir_all(dir)?;
    let identity = load_identity(identity_path)?;
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let plain = TempFile(dir.join(format!(".{BACKUP_PREFIX}{stamp}.tmp.db")));
    store.backup_to(&plain.0)?;
    owner_only(&plain.0)?;

    let file = format!("{BACKUP_PREFIX}{stamp}{BACKUP_SUFFIX}");
    let target = dir.join(&file);
    let partial = TempFile(dir.join(format!(".{file}.partial")));
    {
        let recipient = identity.to_public();
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
                .map_err(|error| SeraError::Secrets(error.to_string()))?;
        let mut input = std::fs::File::open(&plain.0)?;
        let output = std::fs::File::create(&partial.0)?;
        owner_only(&partial.0)?;
        let mut writer = encryptor
            .wrap_output(output)
            .map_err(|error| SeraError::Secrets(error.to_string()))?;
        std::io::copy(&mut input, &mut writer)?;
        writer.finish()?.flush()?;
    }
    std::fs::rename(&partial.0, &target)?;
    drop(plain);
    let bytes = std::fs::metadata(&target)?.len();
    let (restore_ok, restore_detail) = match test_restore(&target, &identity, dir) {
        Ok(detail) => (true, detail),
        Err(error) => (false, error.to_string()),
    };
    for old in list_backups(dir).into_iter().skip(keep) {
        let _ = std::fs::remove_file(dir.join(old.file));
    }
    Ok(BackupStatus {
        at: now.to_rfc3339(),
        ok: restore_ok,
        error: (!restore_ok).then(|| format!("test restore failed: {restore_detail}")),
        file,
        bytes,
        restore_ok,
        restore_detail,
        onedrive: String::new(),
    })
}

/// Decrypt a backup into a temporary file and check it opens cleanly.
fn test_restore(backup: &Path, identity: &age::x25519::Identity, dir: &Path) -> Result<String> {
    let input = std::fs::File::open(backup)?;
    let decryptor = age::Decryptor::new(std::io::BufReader::new(input))
        .map_err(|error| SeraError::Secrets(format!("cannot open backup: {error}")))?;
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|error| SeraError::Secrets(format!("cannot decrypt backup: {error}")))?;
    let restored = TempFile(dir.join(".restore-test.tmp.db"));
    {
        let mut output = std::fs::File::create(&restored.0)?;
        owner_only(&restored.0)?;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read])?;
        }
        output.flush()?;
    }
    let connection = rusqlite::Connection::open_with_flags(
        &restored.0,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let check: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(SeraError::Internal(format!("integrity check: {check}")));
    }
    let users: i64 = connection.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    Ok(format!("integrity ok, {users} members, schema {version}"))
}

/// The JSON the console shows for backups.
pub fn backup_view(config: &Config, store: &Store) -> Result<Value> {
    let dir = config.path(&config.ops.backup_dir);
    Ok(json!({
        "enabled": config.ops.backup_enabled,
        "hour_utc": config.ops.backup_hour_utc,
        "keep": config.ops.backup_keep,
        "onedrive": config.ops.backup_onedrive,
        "last": backup_status(store)?,
        "files": list_backups(&dir),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    #[derive(Default)]
    struct Capture(Mutex<Vec<(String, String, String)>>);

    #[async_trait]
    impl Notifier for Capture {
        async fn send_text(&self, user: &str, conversation: &str, text: &str) -> Result<()> {
            self.0.lock().unwrap().push((
                user.to_string(),
                conversation.to_string(),
                text.to_string(),
            ));
            Ok(())
        }

        async fn approval_requested(
            &self,
            _user: &str,
            _conversation: &str,
            _notice: &crate::runtime::ApprovalNotice,
        ) -> Result<()> {
            Ok(())
        }
    }

    fn failover(store: &Store, user: &str) {
        store
            .record_audit(
                user,
                "model.failover",
                None,
                None,
                None,
                None,
                None,
                Some(&json!({"from": {"provider": "p", "model": "m"}, "to": {"provider": "p", "model": "n"}})),
            )
            .unwrap();
    }

    #[test]
    fn auto_reply_mode_speaks_only_after_a_voice_note() {
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        store.set_reply_mode(&admin, "chat-a", "auto").unwrap();
        assert_eq!(store.reply_mode(&admin, "chat-a").unwrap(), "auto");
        assert!(!store.speak_reply(&admin, "chat-a").unwrap());
        store.note_inbound_voice(&admin, "chat-a", true).unwrap();
        assert!(store.speak_reply(&admin, "chat-a").unwrap());
        store.note_inbound_voice(&admin, "chat-a", false).unwrap();
        assert!(!store.speak_reply(&admin, "chat-a").unwrap());
        store.set_reply_mode(&admin, "chat-a", "voice").unwrap();
        assert!(store.speak_reply(&admin, "chat-a").unwrap());
        assert!(store.set_reply_mode(&admin, "chat-a", "shout").is_err());
    }

    #[test]
    fn the_digest_counts_without_content() {
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        store
            .record_audit(
                &admin,
                "model.used",
                None,
                None,
                None,
                None,
                None,
                Some(&json!({"provider": "p", "model": "m",
                    "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}})),
            )
            .unwrap();
        failover(&store, &admin);
        let text = weekly_digest_text(&store, &admin, true, Utc::now()).unwrap();
        assert!(text.contains("1 replies, 150 tokens"), "{text}");
        assert!(text.contains("1 model failovers"), "{text}");
        assert!(text.contains("without a price"), "{text}");
        assert!(text.contains("No backup yet"), "{text}");
    }

    #[tokio::test]
    async fn budget_alerts_fire_once_per_level_per_month() {
        let dir = tempfile::tempdir().unwrap();
        let config = Arc::new(Config {
            base_dir: dir.path().to_path_buf(),
            ..Config::default()
        });
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        store
            .set_model_prices(&[crate::costs::ModelPrice {
                provider: "p".into(),
                model: "*".into(),
                input_per_m: 10.0,
                output_per_m: 10.0,
                cached_per_m: None,
            }])
            .unwrap();
        store.set_monthly_budget(&admin, Some(1.0)).unwrap();
        store
            .record_audit(
                &admin,
                "model.used",
                None,
                None,
                None,
                None,
                None,
                Some(&json!({"provider": "p", "model": "m",
                    "usage": {"prompt_tokens": 90_000, "completion_tokens": 0, "total_tokens": 90_000}})),
            )
            .unwrap();
        let capture = Arc::new(Capture::default());
        let watch = OpsWatch::new(config, store.clone(), capture.clone(), None);
        watch.sweep_budgets(Utc::now()).await.unwrap();
        watch.sweep_budgets(Utc::now()).await.unwrap();
        let sent = capture.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].2.contains("80%"));
    }

    fn at(hour: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 11, 20, hour, 5, 0).unwrap()
    }

    #[test]
    fn helper_defaults_and_updates() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let defaults = store.helper_settings(&admin).unwrap();
        assert!(defaults.morning_brief && defaults.memory_review && !defaults.email_triage);
        store
            .set_helper_settings(
                &admin,
                &HelperPatch {
                    email_triage: Some(true),
                    brief_hour_utc: Some(Some(9)),
                    ..Default::default()
                },
            )
            .unwrap();
        let saved = store.helper_settings(&admin).unwrap();
        assert!(saved.email_triage && saved.morning_brief);
        assert_eq!(saved.brief_hour_utc, Some(9));
        // The alert channel settings are untouched by the helper switches.
        assert!(store.alert_settings(&admin).unwrap().telegram);
        store
            .set_helper_settings(
                &admin,
                &HelperPatch {
                    brief_hour_utc: Some(None),
                    morning_brief: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();
        let cleared = store.helper_settings(&admin).unwrap();
        assert!(!cleared.morning_brief);
        assert_eq!(cleared.brief_hour_utc, None);
        assert!(
            store
                .set_helper_settings(
                    &admin,
                    &HelperPatch {
                        triage_hour_utc: Some(Some(24)),
                        ..Default::default()
                    }
                )
                .is_err()
        );
        // Saving channels afterwards keeps the helper switches.
        store
            .set_alert_settings(
                &admin,
                &AlertSettings {
                    telegram: false,
                    whatsapp_conversation_id: None,
                },
            )
            .unwrap();
        assert!(!store.helper_settings(&admin).unwrap().morning_brief);
    }

    #[test]
    fn triage_is_submitted_once_per_day_only_when_on_and_a_mailbox_exists() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let bob = store
            .create_user("microsoft:bob", "bob@example.example", "Bob")
            .unwrap()
            .id;
        let config = Config::default();
        let mut submitted: Vec<String> = Vec::new();
        let run = |store: &Store, now: DateTime<Utc>, mailbox: bool, out: &mut Vec<String>| {
            triage_sweep(store, &config, now, &|_| mailbox, &mut |user| {
                out.push(user.to_string());
                Ok(())
            })
            .unwrap()
        };
        // Off by default: nothing.
        assert_eq!(run(&store, at(7), true, &mut submitted), 0);
        store
            .set_helper_settings(
                &admin,
                &HelperPatch {
                    email_triage: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        // Wrong hour, then no mailbox: nothing, and no day is used up.
        assert_eq!(run(&store, at(3), true, &mut submitted), 0);
        assert_eq!(run(&store, at(7), false, &mut submitted), 0);
        // Right hour with a mailbox: once, however often the minute sweep runs.
        assert_eq!(run(&store, at(7), true, &mut submitted), 1);
        assert_eq!(run(&store, at(7), true, &mut submitted), 0);
        assert_eq!(submitted, vec![admin.clone()]);
        // Next day it runs again; a member who stays off never does.
        let tomorrow = at(7) + chrono::Duration::days(1);
        assert_eq!(run(&store, tomorrow, true, &mut submitted), 1);
        assert!(!submitted.contains(&bob));
        // Switching off stops it.
        store
            .set_helper_settings(
                &admin,
                &HelperPatch {
                    email_triage: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();
        let later = tomorrow + chrono::Duration::days(1);
        assert_eq!(run(&store, later, true, &mut submitted), 0);
    }

    #[test]
    fn the_members_own_hour_overrides_the_default() {
        assert!(hour_due(None, 6, at(6)));
        assert!(!hour_due(None, 6, at(7)));
        assert!(hour_due(Some(7), 6, at(7)));
        assert!(!hour_due(Some(7), 6, at(6)));
    }

    #[tokio::test]
    async fn briefs_go_to_members_with_the_switch_on_once_a_day() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let bob = store
            .create_user("microsoft:bob", "bob@example.example", "Bob")
            .unwrap()
            .id;
        store
            .set_helper_settings(
                &bob,
                &HelperPatch {
                    morning_brief: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();
        let capture = Arc::new(Capture::default());
        let watch = OpsWatch::new(
            Arc::new(Config::default()),
            store.clone(),
            capture.clone(),
            None,
        );
        assert_eq!(watch.send_due_briefs(at(5)).await.unwrap(), 0);
        assert_eq!(watch.send_due_briefs(at(6)).await.unwrap(), 1);
        assert_eq!(watch.send_due_briefs(at(6)).await.unwrap(), 0);
        let sent = capture.0.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, admin);
        assert!(sent[0].2.contains("morning brief"));
    }

    #[tokio::test]
    async fn the_weekly_memory_review_lists_only_the_members_own_old_memories() {
        use chrono::TimeZone;
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let bob = store
            .create_user("microsoft:bob", "bob@example.example", "Bob")
            .unwrap()
            .id;
        let mine =
            crate::memory::add(&store, &admin, "fact", "alpha beta gamma delta mine", "t").unwrap();
        crate::memory::add(&store, &bob, "fact", "bobs private note about travel", "t").unwrap();
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE memories SET updated_at = '2020-01-01T00:00:00Z'",
                [],
            )
            .unwrap();
        let capture = Arc::new(Capture::default());
        let watch = OpsWatch::new(
            Arc::new(Config::default()),
            store.clone(),
            capture.clone(),
            None,
        );
        // Sunday 2026-11-22 17:00 UTC is the default slot.
        let sunday = Utc.with_ymd_and_hms(2026, 11, 22, 17, 1, 0).unwrap();
        assert_eq!(watch.send_due_memory_reviews(sunday).await.unwrap(), 2);
        assert_eq!(watch.send_due_memory_reviews(sunday).await.unwrap(), 0);
        // Wrong day: nothing.
        assert_eq!(watch.send_due_memory_reviews(at(17)).await.unwrap(), 0);
        let sent = capture.0.lock().unwrap();
        let to_admin = sent.iter().find(|row| row.0 == admin).unwrap();
        assert!(to_admin.2.contains("alpha beta gamma delta mine"));
        assert!(!to_admin.2.contains("bobs private"));
        assert!(!to_admin.2.contains(&mine.id));
    }

    #[test]
    fn percent_rounds_and_caps() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(45, 50), 90);
        assert_eq!(percent(9, 4), 100);
    }

    #[tokio::test]
    async fn failover_streaks_alert_once_per_cooldown() {
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        for _ in 0..3 {
            failover(&store, &admin);
        }
        let streaks = failover_streaks(&store, Utc::now(), 3).unwrap();
        assert_eq!(streaks.len(), 1);
        assert_eq!(streaks[0].3, 3);
        assert!(failover_streaks(&store, Utc::now(), 4).unwrap().is_empty());

        let capture = Arc::new(Capture::default());
        let alerts = Alerts::new(store.clone(), capture.clone(), 60);
        let alert = Alert {
            user_id: admin.clone(),
            kind: "failover",
            key: "p/m".into(),
            text: "m failed".into(),
        };
        assert!(alerts.raise(&alert).await.unwrap());
        assert!(!alerts.raise(&alert).await.unwrap());
        let sent = capture.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, ALERT_CONVERSATION);
        assert!(sent[0].2.contains("m failed"));
        let recent = store.recent_alerts(&admin, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].kind, "failover");
    }

    #[tokio::test]
    async fn telegram_off_and_no_whatsapp_sends_nothing() {
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        store
            .set_alert_settings(
                &admin,
                &AlertSettings {
                    telegram: false,
                    whatsapp_conversation_id: None,
                },
            )
            .unwrap();
        let capture = Arc::new(Capture::default());
        let alerts = Alerts::new(store.clone(), capture.clone(), 60);
        alerts
            .raise(&Alert {
                user_id: admin.clone(),
                kind: "disk",
                key: "data".into(),
                text: "full".into(),
            })
            .await
            .unwrap();
        assert!(capture.0.lock().unwrap().is_empty());
        // Still audited, so the console lists it.
        assert_eq!(store.recent_alerts(&admin, 10).unwrap().len(), 1);
    }

    #[test]
    fn whatsapp_alert_chat_must_be_the_members_own() {
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        let error = store
            .set_alert_settings(
                &admin,
                &AlertSettings {
                    telegram: true,
                    whatsapp_conversation_id: Some("someone-elses".into()),
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("WhatsApp"));
        assert_eq!(
            store.alert_settings(&admin).unwrap(),
            AlertSettings::default()
        );
    }

    #[test]
    fn expired_signins_follow_the_cursor() {
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        let id = store
            .record_audit(
                &admin,
                "provider.account_needs_reauth",
                None,
                None,
                None,
                None,
                None,
                Some(&json!({"provider": "anthropic", "account": "a"})),
            )
            .unwrap();
        let found = expired_signins(&store, id - 1).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].2, "anthropic");
        assert!(expired_signins(&store, id).unwrap().is_empty());
    }

    #[test]
    fn backups_are_encrypted_restorable_and_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("live.db");
        let store = Store::open(&db, 8, 5000).unwrap();
        let identity_path = dir.path().join("age-identity.txt");
        crate::secrets::ensure_identity(&identity_path).unwrap();
        let backups = dir.path().join("backups");
        let start = Utc::now();
        let mut last = None;
        for n in 0..3 {
            let status = write_backup(
                &store,
                &backups,
                &identity_path,
                2,
                start + chrono::Duration::seconds(n),
            )
            .unwrap();
            assert!(status.ok, "{status:?}");
            assert!(status.restore_ok);
            assert!(status.restore_detail.contains("integrity ok"));
            last = Some(status);
        }
        let files = list_backups(&backups);
        assert_eq!(files.len(), 2, "pruned to keep");
        assert_eq!(files[0].file, last.unwrap().file);
        // No plaintext copy is left behind, and the file is not SQLite.
        let leftovers: Vec<_> = std::fs::read_dir(&backups)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let raw = std::fs::read(backups.join(&files[0].file)).unwrap();
        assert!(!raw.starts_with(b"SQLite format 3"));
    }

    #[test]
    fn a_backup_without_the_key_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("live.db"), 8, 5000).unwrap();
        let error = write_backup(
            &store,
            &dir.path().join("backups"),
            &dir.path().join("missing-key.txt"),
            3,
            Utc::now(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("backup key"));
    }
}
