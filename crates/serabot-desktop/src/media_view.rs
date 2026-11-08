//! Pure helpers for the chat's media and work-limit views (0.24): inline video
//! and audio players, the "Generating ..." placeholder for background media
//! jobs, the Continue button under a limit-stopped reply, and the Settings
//! form for the per-turn work limits. No UI code here; the components in the
//! console binary call these and the tests cover them.

use std::collections::BTreeSet;

use crate::ApiError;
use crate::api::{MediaJob, Message, Role, TurnLimitPair, TurnLimitsView};

/// Largest file read back for an inline player. Larger media stays a card
/// with Open and Save, so a long video never fills memory.
pub const MAX_PREVIEW_BYTES: usize = 40 * 1024 * 1024;

/// Most inline previews kept in memory at once (0.24). Past this, the oldest
/// one is dropped and must be read again if its player is opened.
pub const MAX_CACHED_PREVIEWS: usize = 3;

/// The owner message that Continue sends. The bot sees it in history with
/// the turn that stopped.
pub const CONTINUE_TEXT: &str = "Continue from where you stopped.";

/// Which inline player a library entry gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlinePlayer {
    Video,
    Audio,
}

/// The inline player for a media type, or `None` when the file is not
/// video or audio.
pub fn inline_player(mime: &str) -> Option<InlinePlayer> {
    let mime = mime.trim().to_ascii_lowercase();
    if mime.starts_with("video/") {
        Some(InlinePlayer::Video)
    } else if mime.starts_with("audio/") {
        Some(InlinePlayer::Audio)
    } else {
        None
    }
}

/// A data URL for a preview, refused above [`MAX_PREVIEW_BYTES`] and for a
/// type that is not video or audio.
pub fn preview_data_url_for(mime: &str, bytes: &[u8]) -> Result<String, ApiError> {
    if inline_player(mime).is_none() {
        return Err(ApiError::Validation("Only video and audio play inline."));
    }
    if bytes.len() > MAX_PREVIEW_BYTES {
        return Err(ApiError::Validation(
            "This file is too large to play inline. Open it instead.",
        ));
    }
    Ok(crate::avatars::preview_data_url(mime.trim(), bytes))
}

/// The jobs still rendering, oldest first. Each one gets a placeholder card.
/// A job being saved and posted (`finishing`) still shows its card.
pub fn pending_jobs(jobs: &[MediaJob]) -> Vec<&MediaJob> {
    jobs.iter()
        .filter(|job| job.status == "pending" || job.status == "finishing")
        .collect()
}

/// The inline previews read back so far, keyed by artifact hash, and the reads
/// still in flight. Entries are capped at [`MAX_CACHED_PREVIEWS`]; the oldest
/// read is dropped first. A hash has at most one read in flight.
#[derive(Debug, Default)]
pub struct PreviewCache {
    /// Oldest read first.
    entries: Vec<(String, String)>,
    in_flight: BTreeSet<String>,
}

impl PreviewCache {
    /// The data URL cached for a hash.
    pub fn get(&self, hash: &str) -> Option<&String> {
        self.entries
            .iter()
            .find(|(key, _)| key == hash)
            .map(|(_, data_url)| data_url)
    }

    pub fn contains(&self, hash: &str) -> bool {
        self.get(hash).is_some()
    }

    /// Mark a read as started. Returns `false` when the hash is already cached
    /// or already being read, so the caller must not start another read.
    pub fn begin(&mut self, hash: &str) -> bool {
        if self.contains(hash) || self.in_flight.contains(hash) {
            return false;
        }
        self.in_flight.insert(hash.to_owned());
        true
    }

    /// Store a finished read and evict the oldest entries past the cap.
    pub fn insert(&mut self, hash: String, data_url: String) {
        self.in_flight.remove(&hash);
        self.entries.retain(|(key, _)| *key != hash);
        self.entries.push((hash, data_url));
        while self.entries.len() > MAX_CACHED_PREVIEWS {
            self.entries.remove(0);
        }
    }

    /// A read failed. The failure does not name its hash, so every in-flight
    /// mark is cleared and the next Play asks again.
    pub fn fail_in_flight(&mut self) {
        self.in_flight.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }
}

/// The placeholder line for a job: "Generating a video..." while it runs.
pub fn placeholder_label(job: &MediaJob) -> String {
    let kind = match job.kind.as_str() {
        "image" | "video" | "audio" => job.kind.as_str(),
        _ => "file",
    };
    format!("Generating {} {}\u{2026}", article_for(kind), kind)
}

/// "an" before a vowel sound ("an image", "an audio"), "a" otherwise ("a video").
fn article_for(kind: &str) -> &'static str {
    match kind {
        "image" | "audio" => "an",
        _ => "a",
    }
}

/// Whether the reply gets a Continue button: an assistant reply that a tool
/// limit ended, and only while no later assistant reply exists. Any later
/// reply, limited or not, means the conversation has moved on.
pub fn offers_continue(messages: &[Message], index: usize) -> bool {
    let Some(message) = messages.get(index) else {
        return false;
    };
    if message.role != Role::Assistant || !message.limit_reached {
        return false;
    }
    !messages[index + 1..]
        .iter()
        .any(|later| later.role == Role::Assistant)
}

/// Parse the Settings form. Both fields must be whole numbers within
/// `1..=ceiling`; the error names the field that is wrong.
pub fn parse_turn_limits(
    calls: &str,
    rounds: &str,
    ceiling: TurnLimitPair,
) -> Result<TurnLimitPair, String> {
    let calls = parse_within(
        "Tool calls per turn",
        calls,
        ceiling.max_tool_calls_per_turn,
    )?;
    let rounds = parse_within("Tool rounds per turn", rounds, ceiling.max_tool_rounds)?;
    Ok(TurnLimitPair {
        max_tool_calls_per_turn: calls,
        max_tool_rounds: rounds,
    })
}

fn parse_within(label: &str, text: &str, ceiling: usize) -> Result<usize, String> {
    let value: usize = text
        .trim()
        .parse()
        .map_err(|_| format!("{label} must be a whole number."))?;
    if value == 0 || value > ceiling {
        return Err(format!("{label} must be between 1 and {ceiling}."));
    }
    Ok(value)
}

/// The one-line status under the Settings card: what applies now, and whether
/// it is the default or the owner's own value.
pub fn limits_summary(view: &TurnLimitsView) -> String {
    let source = if view.overridden {
        "Your setting"
    } else {
        "Default"
    };
    format!(
        "{source}: {} tool calls and {} tool rounds per turn. Ceiling: {} calls and {} rounds.",
        view.max_tool_calls_per_turn,
        view.max_tool_rounds,
        view.ceiling.max_tool_calls_per_turn,
        view.ceiling.max_tool_rounds,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(kind: &str, status: &str) -> MediaJob {
        MediaJob {
            id: "job-1".into(),
            bot: "maker".into(),
            kind: kind.into(),
            status: status.into(),
            artifact_hash: None,
            error: None,
            created_at: "2026-11-08T10:00:00.000Z".into(),
        }
    }

    fn message(role: Role, text: &str, limit_reached: bool) -> Message {
        Message {
            id: text.into(),
            role,
            content: text.into(),
            created_at: "2026-11-08T10:00:00.000Z".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached,
        }
    }

    #[test]
    fn placeholder_names_each_kind_with_its_article() {
        assert_eq!(
            placeholder_label(&job("image", "pending")),
            "Generating an image\u{2026}"
        );
        assert_eq!(
            placeholder_label(&job("audio", "pending")),
            "Generating an audio\u{2026}"
        );
        assert_eq!(
            placeholder_label(&job("video", "pending")),
            "Generating a video\u{2026}"
        );
    }

    #[test]
    fn video_and_audio_get_players_and_other_types_do_not() {
        assert_eq!(inline_player("video/mp4"), Some(InlinePlayer::Video));
        assert_eq!(inline_player(" Audio/MPEG "), Some(InlinePlayer::Audio));
        assert_eq!(inline_player("image/png"), None);
        assert_eq!(inline_player("application/pdf"), None);
        assert!(preview_data_url_for("image/png", b"x").is_err());
        assert!(
            preview_data_url_for("video/mp4", b"abc")
                .unwrap()
                .starts_with("data:video/mp4;base64,")
        );
    }

    #[test]
    fn oversize_previews_are_refused() {
        let big = vec![0u8; MAX_PREVIEW_BYTES + 1];
        assert!(preview_data_url_for("video/mp4", &big).is_err());
    }

    #[test]
    fn only_pending_jobs_get_placeholders() {
        let jobs = vec![
            job("video", "pending"),
            job("audio", "done"),
            job("image", "failed"),
            job("image", "pending"),
            job("video", "finishing"),
        ];
        let pending = pending_jobs(&jobs);
        assert_eq!(pending.len(), 3);
        assert_eq!(placeholder_label(pending[0]), "Generating a video\u{2026}");
        assert_eq!(placeholder_label(pending[1]), "Generating an image\u{2026}");
        assert_eq!(
            placeholder_label(&job("odd", "pending")),
            "Generating a file\u{2026}"
        );
    }

    #[test]
    fn continue_is_offered_only_while_no_later_reply_exists() {
        let transcript = vec![
            message(Role::User, "read the root", false),
            message(Role::Assistant, "first stop", true),
            message(Role::User, CONTINUE_TEXT, false),
            message(Role::Assistant, "second stop", true),
            message(Role::Assistant, "normal reply", false),
        ];
        assert!(!offers_continue(&transcript, 0));
        assert!(!offers_continue(&transcript, 1));
        // A later assistant reply, even a normal one, ends the offer.
        assert!(!offers_continue(&transcript, 3));
        assert!(!offers_continue(&transcript, 4));
        assert!(!offers_continue(&transcript, 99));
        assert_eq!(CONTINUE_TEXT, "Continue from where you stopped.");
    }

    #[test]
    fn continue_is_offered_on_the_last_limited_reply() {
        let transcript = vec![
            message(Role::User, "read the root", false),
            message(Role::Assistant, "stopped", true),
            message(Role::User, "thanks", false),
        ];
        assert!(offers_continue(&transcript, 1));
    }

    #[test]
    fn preview_cache_keeps_three_and_drops_the_oldest() {
        let mut cache = PreviewCache::default();
        for (hash, url) in [("a", "data:a"), ("b", "data:b"), ("c", "data:c")] {
            assert!(cache.begin(hash));
            cache.insert(hash.to_owned(), url.to_owned());
        }
        assert_eq!(cache.len(), MAX_CACHED_PREVIEWS);
        assert!(cache.begin("d"));
        cache.insert("d".into(), "data:d".into());
        assert_eq!(cache.len(), MAX_CACHED_PREVIEWS);
        assert!(!cache.contains("a"), "the oldest read is dropped");
        assert_eq!(cache.get("d").map(String::as_str), Some("data:d"));
        assert!(cache.contains("b") && cache.contains("c"));
    }

    #[test]
    fn preview_reads_are_deduplicated_while_in_flight() {
        let mut cache = PreviewCache::default();
        assert!(cache.begin("video"));
        assert!(
            !cache.begin("video"),
            "a second Play waits for the first read"
        );
        assert_eq!(cache.in_flight_count(), 1);
        cache.insert("video".into(), "data:video".into());
        assert_eq!(cache.in_flight_count(), 0);
        assert!(!cache.begin("video"), "a cached file is not read again");
    }

    #[test]
    fn a_failed_read_clears_in_flight_marks_so_play_can_retry() {
        let mut cache = PreviewCache::default();
        assert!(cache.begin("audio"));
        cache.fail_in_flight();
        assert!(cache.is_empty());
        assert!(cache.begin("audio"));
    }

    #[test]
    fn limit_form_accepts_values_within_the_ceiling() {
        let ceiling = TurnLimitPair {
            max_tool_calls_per_turn: 500,
            max_tool_rounds: 200,
        };
        assert_eq!(
            parse_turn_limits(" 150 ", "60", ceiling),
            Ok(TurnLimitPair {
                max_tool_calls_per_turn: 150,
                max_tool_rounds: 60,
            })
        );
        assert!(parse_turn_limits("0", "60", ceiling).is_err());
        assert!(parse_turn_limits("501", "60", ceiling).is_err());
        let rounds_error = parse_turn_limits("10", "201", ceiling).unwrap_err();
        assert!(rounds_error.contains("Tool rounds per turn"));
        assert!(parse_turn_limits("ten", "10", ceiling).is_err());
    }

    #[test]
    fn summary_says_where_the_value_comes_from() {
        let view = TurnLimitsView {
            max_tool_calls_per_turn: 150,
            max_tool_rounds: 60,
            default: TurnLimitPair {
                max_tool_calls_per_turn: 150,
                max_tool_rounds: 60,
            },
            ceiling: TurnLimitPair {
                max_tool_calls_per_turn: 500,
                max_tool_rounds: 200,
            },
            overridden: false,
        };
        assert!(limits_summary(&view).starts_with("Default: 150 tool calls"));
        let own = TurnLimitsView {
            overridden: true,
            ..view
        };
        assert!(limits_summary(&own).starts_with("Your setting: 150 tool calls"));
    }
}
