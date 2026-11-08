//! Background media jobs (0.24): a provider render (an image, a video or an
//! audio file) that outlives the tool call that started it.
//!
//! How a job runs:
//!
//! 1. A provider integration submits the work, gets an external id, and calls
//!    [`Store::create_media_job`]. The row is created only after the provider
//!    accepted the work.
//! 2. The background loop ([`run_media_jobs`]) polls every pending job every
//!    [`POLL_INTERVAL`], through the [`MediaPoller`] registered for the job's
//!    provider name ([`register_media_poller`]).
//! 3. A finished job saves its file to the owner's artifact store, records it
//!    in the conversation, and posts "Your <kind> is ready." A failed job, or
//!    one still pending at its deadline, posts a short note instead.
//!
//! Unknown-outcome rule: a job is never submitted again. The loop only polls
//! jobs that already exist, and a job whose provider has no poller waits until
//! its deadline. A submission whose outcome is unknown creates no row, so the
//! caller reports it rather than retrying it.
//!
//! Every store method takes the acting user's id. The loop is the single system
//! reader: it selects due jobs across users, as the task claim does, and then
//! acts on each job under its own owner. Fixed text is posted to chat; a
//! provider's error text is kept in the row only, truncated.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::domain::{Language, Role, new_id};
use crate::error::{Result, SeraError};
use crate::maker::{ArtifactMeta, ArtifactStore};
use crate::store::Store;
use crate::util::truncate_chars;

/// How often the loop polls the pending jobs.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);
/// How long a job may stay pending before it expires.
pub const DEFAULT_DEADLINE_MINUTES: i64 = 20;
/// Most pending jobs one poll pass handles.
const POLL_BATCH: usize = 50;
/// Longest provider error kept on a job row.
pub const MAX_JOB_ERROR_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "image" => Some(Self::Image),
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaJobStatus {
    Pending,
    Done,
    Failed,
    Expired,
}

impl MediaJobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "pending" => Some(Self::Pending),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// One background job, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaJob {
    pub id: String,
    pub user_id: String,
    pub conversation_id: String,
    pub bot: String,
    pub provider: String,
    pub kind: MediaKind,
    pub external_id: String,
    pub status: MediaJobStatus,
    pub artifact_hash: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deadline: DateTime<Utc>,
}

/// A finished file a poller hands back.
#[derive(Debug, Clone)]
pub struct MediaArtifact {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub title: Option<String>,
}

/// What a poll found. `Pending` means still rendering.
#[derive(Debug, Clone)]
pub enum PollResult {
    Pending,
    Done(MediaArtifact),
    Failed(String),
}

/// Looks up a job at its provider. A poller must not submit new work.
#[async_trait]
pub trait MediaPoller: Send + Sync {
    async fn poll(&self, user_id: &str, external_id: &str) -> Result<PollResult>;
}

type PollerMap = HashMap<String, Arc<dyn MediaPoller>>;

static POLLERS: OnceLock<RwLock<PollerMap>> = OnceLock::new();

fn pollers() -> &'static RwLock<PollerMap> {
    POLLERS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register the poller for a provider name. A later registration for the same
/// name replaces the earlier one.
pub fn register_media_poller(provider: &str, poller: Arc<dyn MediaPoller>) -> Result<()> {
    if provider.trim().is_empty() {
        return Err(SeraError::Validation("media provider name is empty".into()));
    }
    let mut map = pollers()
        .write()
        .map_err(|_| SeraError::Internal("media pollers are unavailable".into()))?;
    map.insert(provider.to_owned(), poller);
    Ok(())
}

/// The poller registered for a provider name, if any.
pub fn media_poller(provider: &str) -> Option<Arc<dyn MediaPoller>> {
    pollers().read().ok()?.get(provider).cloned()
}

fn stamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_stamp(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaJob> {
    // The table's CHECK constraints keep these columns to known values.
    let kind: String = row.get(5)?;
    let status: String = row.get(7)?;
    Ok(MediaJob {
        id: row.get(0)?,
        user_id: row.get(1)?,
        conversation_id: row.get(2)?,
        bot: row.get(3)?,
        provider: row.get(4)?,
        kind: MediaKind::parse(&kind).unwrap_or(MediaKind::Image),
        external_id: row.get(6)?,
        status: MediaJobStatus::parse(&status).unwrap_or(MediaJobStatus::Failed),
        artifact_hash: row.get(8)?,
        error: row.get(9)?,
        created_at: parse_stamp(&row.get::<_, String>(10)?),
        updated_at: parse_stamp(&row.get::<_, String>(11)?),
        deadline: parse_stamp(&row.get::<_, String>(12)?),
    })
}

const JOB_COLUMNS: &str = "id, user_id, conversation_id, bot, provider, kind, external_id, status,
     artifact_hash, error, created_at, updated_at, deadline";

impl Store {
    /// Record a job the provider accepted. The conversation must belong to the
    /// acting user. The job expires after [`DEFAULT_DEADLINE_MINUTES`].
    pub fn create_media_job(
        &self,
        user_id: &str,
        conversation_id: &str,
        bot: &str,
        provider: &str,
        kind: MediaKind,
        external_id: &str,
    ) -> Result<MediaJob> {
        if provider.trim().is_empty() || bot.trim().is_empty() {
            return Err(SeraError::Validation(
                "media job needs a bot and provider".into(),
            ));
        }
        if external_id.trim().is_empty() || external_id.len() > 256 {
            return Err(SeraError::Validation(
                "media job needs a provider id of 1 to 256 characters".into(),
            ));
        }
        self.get_conversation(user_id, conversation_id)?
            .ok_or_else(|| SeraError::NotFound(format!("conversation {conversation_id}")))?;
        let now = Utc::now();
        let job = MediaJob {
            id: new_id(),
            user_id: user_id.to_owned(),
            conversation_id: conversation_id.to_owned(),
            bot: bot.to_owned(),
            provider: provider.to_owned(),
            kind,
            external_id: external_id.to_owned(),
            status: MediaJobStatus::Pending,
            artifact_hash: None,
            error: None,
            created_at: now,
            updated_at: now,
            deadline: now + chrono::Duration::minutes(DEFAULT_DEADLINE_MINUTES),
        };
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO media_jobs(id, user_id, conversation_id, bot, provider, kind, external_id,
                 status, artifact_hash, error, created_at, updated_at, deadline)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', NULL, NULL, ?8, ?8, ?9)",
            params![
                job.id,
                user_id,
                conversation_id,
                bot,
                provider,
                kind.as_str(),
                external_id,
                stamp(now),
                stamp(job.deadline),
            ],
        )?;
        drop(connection);
        self.append_event(
            user_id,
            "media_job.created",
            &json!({
                "job_id": job.id,
                "conversation_id": conversation_id,
                "kind": kind.as_str(),
                "status": "pending",
            }),
        )?;
        Ok(job)
    }

    /// One job of the acting user, by id. Another user's job is reported as
    /// missing.
    pub fn get_media_job(&self, user_id: &str, id: &str) -> Result<MediaJob> {
        let connection = self.connection()?;
        connection
            .query_row(
                &format!("SELECT {JOB_COLUMNS} FROM media_jobs WHERE id = ?1 AND user_id = ?2"),
                params![id, user_id],
                map_job,
            )
            .optional()?
            .ok_or_else(|| SeraError::NotFound(format!("media job {id}")))
    }

    /// The acting user's jobs in one conversation, oldest first.
    pub fn list_media_jobs(&self, user_id: &str, conversation_id: &str) -> Result<Vec<MediaJob>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM media_jobs
             WHERE user_id = ?1 AND conversation_id = ?2
             ORDER BY created_at ASC, rowid ASC LIMIT 100"
        ))?;
        let rows = statement.query_map(params![user_id, conversation_id], map_job)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Pending jobs of every user, oldest first, for the poll loop. This is the
    /// system reader; the loop acts on each job under its own `user_id`.
    pub fn pending_media_jobs(&self, limit: usize) -> Result<Vec<MediaJob>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM media_jobs
             WHERE status = 'pending'
             ORDER BY created_at ASC, rowid ASC LIMIT ?1"
        ))?;
        let rows = statement.query_map(params![limit.clamp(1, 500) as i64], map_job)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Move a pending job to its final status. Only a pending job moves, and
    /// only once, so a second finish returns `false`.
    pub fn finish_media_job(
        &self,
        user_id: &str,
        id: &str,
        status: MediaJobStatus,
        artifact_hash: Option<&str>,
        error: Option<&str>,
    ) -> Result<bool> {
        if status == MediaJobStatus::Pending {
            return Err(SeraError::Validation(
                "a media job cannot return to pending".into(),
            ));
        }
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE media_jobs SET status = ?1, artifact_hash = ?2, error = ?3, updated_at = ?4
             WHERE id = ?5 AND user_id = ?6 AND status = 'pending'",
            params![
                status.as_str(),
                artifact_hash,
                error.map(|text| truncate_chars(text, MAX_JOB_ERROR_CHARS)),
                stamp(Utc::now()),
                id,
                user_id,
            ],
        )? == 1;
        drop(connection);
        if changed {
            let job = self.get_media_job(user_id, id)?;
            self.append_event(
                user_id,
                "media_job.updated",
                &json!({
                    "job_id": job.id,
                    "conversation_id": job.conversation_id,
                    "kind": job.kind.as_str(),
                    "status": status.as_str(),
                }),
            )?;
        }
        Ok(changed)
    }
}

/// Post a line from the job's bot into the job's conversation, under the job's
/// owner, and tell open clients that the conversation changed.
fn post_job_message(store: &Store, job: &MediaJob, text: &str) -> Result<()> {
    store.append_message_named(
        &job.user_id,
        &job.conversation_id,
        None,
        Role::Assistant,
        text,
        Some(&job.bot),
    )?;
    store.append_event(
        &job.user_id,
        "message",
        &json!({"conversation_id": job.conversation_id, "text": text}),
    )?;
    Ok(())
}

fn expire_if_due(store: &Store, job: &MediaJob) -> Result<bool> {
    if Utc::now() < job.deadline {
        return Ok(false);
    }
    let reason = "no result before the deadline";
    if store.finish_media_job(
        &job.user_id,
        &job.id,
        MediaJobStatus::Expired,
        None,
        Some(reason),
    )? {
        post_job_message(
            store,
            job,
            &format!(
                "Your {} did not finish in time, so I stopped waiting for it.",
                job.kind.as_str()
            ),
        )?;
    }
    Ok(true)
}

fn finish_failed(store: &Store, job: &MediaJob, error: &str) -> Result<()> {
    if store.finish_media_job(
        &job.user_id,
        &job.id,
        MediaJobStatus::Failed,
        None,
        Some(error),
    )? {
        post_job_message(
            store,
            job,
            &format!("Your {} could not be made.", job.kind.as_str()),
        )?;
    }
    Ok(())
}

fn finish_done(
    store: &Store,
    artifacts: &ArtifactStore,
    job: &MediaJob,
    artifact: MediaArtifact,
) -> Result<()> {
    let filename = if artifact.filename.trim().is_empty() {
        format!("{}-{}", job.kind.as_str(), job.id)
    } else {
        artifact.filename.clone()
    };
    let title = artifact.title.clone().unwrap_or_else(|| filename.clone());
    let stored = match artifacts.save(
        &artifact.bytes,
        &filename,
        ArtifactMeta {
            title: title.clone(),
            filename: filename.clone(),
            language: Language::En,
            source_artifact: None,
            created_at: Utc::now(),
        },
    ) {
        Ok(stored) => stored,
        Err(error) => {
            warn!(job = %job.id, %error, "could not store a finished media file");
            return finish_failed(store, job, "the finished file could not be stored");
        }
    };
    if !store.finish_media_job(
        &job.user_id,
        &job.id,
        MediaJobStatus::Done,
        Some(&stored.hash),
        None,
    )? {
        return Ok(());
    }
    store.record_conversation_artifact(
        &job.user_id,
        &job.conversation_id,
        None,
        &format!("media:{}", job.provider),
        &stored.hash,
        &stored.filename,
        Some(&title),
    )?;
    post_job_message(store, job, &format!("Your {} is ready.", job.kind.as_str()))
}

/// One pass over the pending jobs. Returns how many finished. A job that
/// fails to poll stays pending and is tried again on the next pass.
pub async fn poll_media_jobs_once(store: &Store, artifacts_dir: &Path) -> Result<usize> {
    let jobs = store.pending_media_jobs(POLL_BATCH)?;
    if jobs.is_empty() {
        return Ok(0);
    }
    let artifacts = ArtifactStore::new(artifacts_dir)?;
    let mut finished = 0;
    for job in jobs {
        let Some(poller) = media_poller(&job.provider) else {
            // No poller for this provider yet: the job waits for one, until its deadline.
            expire_if_due(store, &job)?;
            continue;
        };
        match poller.poll(&job.user_id, &job.external_id).await {
            Ok(PollResult::Pending) => {
                expire_if_due(store, &job)?;
            }
            Ok(PollResult::Done(artifact)) => {
                finish_done(store, &artifacts, &job, artifact)?;
                finished += 1;
            }
            Ok(PollResult::Failed(error)) => {
                finish_failed(store, &job, &error)?;
                finished += 1;
            }
            Err(error) => {
                warn!(job = %job.id, provider = %job.provider, %error, "media job poll failed; will retry");
                expire_if_due(store, &job)?;
            }
        }
    }
    Ok(finished)
}

/// The media job loop. Polls pending jobs every [`POLL_INTERVAL`] until
/// shutdown.
pub async fn run_media_jobs(store: Store, artifacts_dir: PathBuf, shutdown: CancellationToken) {
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tick.tick() => {
                if let Err(failure) = poll_media_jobs_once(&store, &artifacts_dir).await {
                    error!(%failure, "media job poll pass failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Replays a script of poll results and counts the polls it receives.
    struct ScriptedPoller {
        script: Mutex<VecDeque<Result<PollResult>>>,
        polls: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl MediaPoller for ScriptedPoller {
        async fn poll(&self, _user_id: &str, _external_id: &str) -> Result<PollResult> {
            *self.polls.lock().unwrap() += 1;
            self.script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(PollResult::Pending))
        }
    }

    fn provider_name(tag: &str) -> String {
        format!("fake-{tag}-{}", new_id())
    }

    fn scripted(provider: &str, script: Vec<Result<PollResult>>) -> Arc<Mutex<usize>> {
        let polls = Arc::new(Mutex::new(0));
        register_media_poller(
            provider,
            Arc::new(ScriptedPoller {
                script: Mutex::new(script.into()),
                polls: polls.clone(),
            }),
        )
        .unwrap();
        polls
    }

    fn artifact_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn setup() -> (Store, String) {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        store
            .create_conversation(&admin, "dir:main", "direct", None)
            .unwrap();
        (store, admin)
    }

    fn video_bytes() -> MediaArtifact {
        MediaArtifact {
            bytes: b"synthetic video bytes".to_vec(),
            filename: "clip.mp4".into(),
            title: Some("Synthetic clip".into()),
        }
    }

    #[test]
    fn created_job_is_pending_and_scoped_to_its_owner() {
        let (store, admin) = setup();
        let job = store
            .create_media_job(
                &admin,
                "dir:main",
                "maker",
                "fake",
                MediaKind::Video,
                "ext-1",
            )
            .unwrap();
        assert_eq!(job.status, MediaJobStatus::Pending);
        assert_eq!(
            store.get_media_job(&admin, &job.id).unwrap().external_id,
            "ext-1"
        );
        assert_eq!(store.list_media_jobs(&admin, "dir:main").unwrap().len(), 1);
        let stranger = "00000000-0000-7000-8000-0000000000ff";
        assert!(store.get_media_job(stranger, &job.id).is_err());
        assert!(
            store
                .list_media_jobs(stranger, "dir:main")
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .create_media_job(
                    &admin,
                    "dir:missing",
                    "maker",
                    "fake",
                    MediaKind::Audio,
                    "x"
                )
                .is_err()
        );
    }

    #[test]
    fn a_job_finishes_once() {
        let (store, admin) = setup();
        let job = store
            .create_media_job(
                &admin,
                "dir:main",
                "maker",
                "fake",
                MediaKind::Image,
                "ext-2",
            )
            .unwrap();
        assert!(
            store
                .finish_media_job(&admin, &job.id, MediaJobStatus::Failed, None, Some("boom"))
                .unwrap()
        );
        assert!(
            !store
                .finish_media_job(&admin, &job.id, MediaJobStatus::Done, Some("h"), None)
                .unwrap()
        );
        let stored = store.get_media_job(&admin, &job.id).unwrap();
        assert_eq!(stored.status, MediaJobStatus::Failed);
        assert_eq!(stored.error.as_deref(), Some("boom"));
        assert!(store.pending_media_jobs(10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn pending_then_done_saves_the_file_and_posts_once() {
        let (store, admin) = setup();
        let provider = provider_name("done");
        let polls = scripted(
            &provider,
            vec![Ok(PollResult::Pending), Ok(PollResult::Done(video_bytes()))],
        );
        let job = store
            .create_media_job(
                &admin,
                "dir:main",
                "maker",
                &provider,
                MediaKind::Video,
                "ext-3",
            )
            .unwrap();
        let dir = artifact_dir();

        assert_eq!(poll_media_jobs_once(&store, dir.path()).await.unwrap(), 0);
        assert_eq!(
            store.get_media_job(&admin, &job.id).unwrap().status,
            MediaJobStatus::Pending
        );
        assert_eq!(poll_media_jobs_once(&store, dir.path()).await.unwrap(), 1);
        // Once finished, the job is not polled again.
        assert_eq!(poll_media_jobs_once(&store, dir.path()).await.unwrap(), 0);
        assert_eq!(*polls.lock().unwrap(), 2);

        let stored = store.get_media_job(&admin, &job.id).unwrap();
        assert_eq!(stored.status, MediaJobStatus::Done);
        let hash = stored.artifact_hash.expect("artifact hash is recorded");
        let artifacts = store
            .list_conversation_artifacts(&admin, "dir:main")
            .unwrap();
        assert!(artifacts.iter().any(|entry| entry.hash == hash));
        let messages = store.list_visible_messages(&admin, "dir:main", 20).unwrap();
        let ready: Vec<_> = messages
            .iter()
            .filter(|message| message.content == "Your video is ready.")
            .collect();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].name.as_deref(), Some("maker"));
    }

    #[tokio::test]
    async fn failed_poll_result_posts_a_short_note() {
        let (store, admin) = setup();
        let provider = provider_name("failed");
        scripted(
            &provider,
            vec![Ok(PollResult::Failed("provider said no".into()))],
        );
        let job = store
            .create_media_job(
                &admin,
                "dir:main",
                "maker",
                &provider,
                MediaKind::Audio,
                "ext-4",
            )
            .unwrap();
        poll_media_jobs_once(&store, artifact_dir().path())
            .await
            .unwrap();
        let stored = store.get_media_job(&admin, &job.id).unwrap();
        assert_eq!(stored.status, MediaJobStatus::Failed);
        assert_eq!(stored.error.as_deref(), Some("provider said no"));
        let messages = store.list_visible_messages(&admin, "dir:main", 20).unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.content == "Your audio could not be made.")
        );
        assert!(
            store
                .list_conversation_artifacts(&admin, "dir:main")
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_job_past_its_deadline_expires_without_new_work() {
        let (store, admin) = setup();
        let provider = provider_name("late");
        let polls = scripted(&provider, vec![Ok(PollResult::Pending)]);
        let job = store
            .create_media_job(
                &admin,
                "dir:main",
                "maker",
                &provider,
                MediaKind::Video,
                "ext-5",
            )
            .unwrap();
        {
            let connection = store.connection().unwrap();
            connection
                .execute(
                    "UPDATE media_jobs SET deadline = ?1 WHERE id = ?2",
                    params![stamp(Utc::now() - chrono::Duration::seconds(1)), job.id],
                )
                .unwrap();
        }
        poll_media_jobs_once(&store, artifact_dir().path())
            .await
            .unwrap();
        assert_eq!(*polls.lock().unwrap(), 1);
        assert_eq!(
            store.get_media_job(&admin, &job.id).unwrap().status,
            MediaJobStatus::Expired
        );
        let messages = store.list_visible_messages(&admin, "dir:main", 20).unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.content.contains("did not finish in time"))
        );
    }

    #[tokio::test]
    async fn a_poll_error_keeps_the_job_pending_and_is_retried() {
        let (store, admin) = setup();
        let provider = provider_name("flaky");
        let polls = scripted(
            &provider,
            vec![
                Err(SeraError::External("timeout".into())),
                Ok(PollResult::Failed("gone".into())),
            ],
        );
        let job = store
            .create_media_job(
                &admin,
                "dir:main",
                "maker",
                &provider,
                MediaKind::Image,
                "ext-6",
            )
            .unwrap();
        let dir = artifact_dir();
        poll_media_jobs_once(&store, dir.path()).await.unwrap();
        assert_eq!(
            store.get_media_job(&admin, &job.id).unwrap().status,
            MediaJobStatus::Pending
        );
        poll_media_jobs_once(&store, dir.path()).await.unwrap();
        assert_eq!(
            store.get_media_job(&admin, &job.id).unwrap().status,
            MediaJobStatus::Failed
        );
        assert_eq!(*polls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn a_job_without_a_poller_waits_and_is_never_resubmitted() {
        let (store, admin) = setup();
        let provider = provider_name("unregistered");
        let job = store
            .create_media_job(
                &admin,
                "dir:main",
                "maker",
                &provider,
                MediaKind::Image,
                "ext-7",
            )
            .unwrap();
        assert_eq!(
            poll_media_jobs_once(&store, artifact_dir().path())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            store.get_media_job(&admin, &job.id).unwrap().status,
            MediaJobStatus::Pending
        );
    }

    #[test]
    fn registry_replaces_and_rejects_empty_names() {
        let name = provider_name("registry");
        assert!(media_poller(&name).is_none());
        scripted(&name, vec![]);
        assert!(media_poller(&name).is_some());
        assert!(
            register_media_poller(
                "  ",
                Arc::new(ScriptedPoller {
                    script: Mutex::new(VecDeque::new()),
                    polls: Arc::new(Mutex::new(0)),
                })
            )
            .is_err()
        );
    }
}
