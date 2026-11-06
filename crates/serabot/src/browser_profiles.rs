//! Named per-user browser profiles and per-bot settings.
//!
//! A profile is a Chrome user-data directory `<browser base>/<user_id>/<id>`.
//! Every store method takes the acting user's id and filters on it. Directory
//! helpers validate every path component and prove the target sits inside the
//! user's own directory before anything is created or removed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};

use crate::error::{Result, SeraError};
use crate::store::Store;

pub const DEFAULT_PROFILE: &str = "default";
pub const DEFAULT_PROFILE_NAME: &str = "Default";
pub const MAX_PROFILES_PER_USER: usize = 8;
pub const MAX_PROFILE_NAME_CHARS: usize = 48;
pub const MAX_BOT_DISPLAY_NAME_CHARS: usize = 48;
pub const MAX_BOT_DESCRIPTION_CHARS: usize = 280;

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `^[a-z0-9][a-z0-9-]{0,31}$`
pub fn valid_profile_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn valid_user_component(user_id: &str) -> bool {
    !user_id.is_empty()
        && user_id.len() <= 64
        && user_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `<base>/<user_id>/<profile_id>`; only plain validated ids become path parts.
pub fn profile_dir(base: &Path, user_id: &str, profile_id: &str) -> Result<PathBuf> {
    if !valid_user_component(user_id) {
        return Err(SeraError::Validation(
            "invalid user id for browser profile".into(),
        ));
    }
    if !valid_profile_id(profile_id) {
        return Err(SeraError::Validation("invalid browser profile id".into()));
    }
    Ok(base.join(user_id).join(profile_id))
}

/// Remove one profile's Chrome directory. The path must resolve (after
/// symlinks) to a directory directly inside `<base>/<user_id>`; a symlink at
/// the profile path is refused rather than followed. Returns whether a
/// directory existed.
pub fn remove_profile_dir(base: &Path, user_id: &str, profile_id: &str) -> Result<bool> {
    let dir = profile_dir(base, user_id, profile_id)?;
    let metadata = match std::fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SeraError::Validation(
            "browser profile path is not a plain directory".into(),
        ));
    }
    let base_canonical = std::fs::canonicalize(base)?;
    let user_root = std::fs::canonicalize(base.join(user_id))?;
    let canonical = std::fs::canonicalize(&dir)?;
    if user_root.parent() != Some(base_canonical.as_path())
        || canonical.parent() != Some(user_root.as_path())
    {
        return Err(SeraError::Validation(
            "browser profile path escapes the browser directory".into(),
        ));
    }
    std::fs::remove_dir_all(&canonical)?;
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserProfile {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// Owner-editable bot fields outside the runtime config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BotSettings {
    pub display_name: Option<String>,
    pub description: Option<String>,
    /// `None` = the default profile.
    pub browser_profile: Option<String>,
}

fn clean_name(raw: &str) -> Result<String> {
    let name = raw.trim();
    let count = name.chars().count();
    if count == 0 || count > MAX_PROFILE_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(SeraError::Validation(format!(
            "the profile name must be 1 to {MAX_PROFILE_NAME_CHARS} characters"
        )));
    }
    Ok(name.to_owned())
}

fn profile_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BrowserProfile> {
    let id: String = row.get(0)?;
    Ok(BrowserProfile {
        is_default: id == DEFAULT_PROFILE,
        id,
        name: row.get(1)?,
        created_at: row.get(2)?,
        last_used_at: row.get(3)?,
    })
}

impl Store {
    fn ensure_default_browser_profile(&self, user_id: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT OR IGNORE INTO browser_profiles(user_id, id, name, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![user_id, DEFAULT_PROFILE, DEFAULT_PROFILE_NAME, now()],
        )?;
        Ok(())
    }

    /// The user's profiles, `default` first (created on first use).
    pub fn list_browser_profiles(&self, user_id: &str) -> Result<Vec<BrowserProfile>> {
        self.ensure_default_browser_profile(user_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, name, created_at, last_used_at FROM browser_profiles
             WHERE user_id = ?1
             ORDER BY (id = 'default') DESC, created_at, id",
        )?;
        let rows = statement.query_map(params![user_id], profile_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_browser_profile(&self, user_id: &str, id: &str) -> Result<Option<BrowserProfile>> {
        if id == DEFAULT_PROFILE {
            self.ensure_default_browser_profile(user_id)?;
        }
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, name, created_at, last_used_at FROM browser_profiles
                 WHERE user_id = ?1 AND id = ?2",
                params![user_id, id],
                profile_from_row,
            )
            .optional()
            .map_err(SeraError::from)
    }

    /// Create a profile; the id is generated from the name. At most
    /// [`MAX_PROFILES_PER_USER`] per user, `default` included.
    pub fn create_browser_profile(&self, user_id: &str, name: &str) -> Result<BrowserProfile> {
        let name = clean_name(name)?;
        let existing = self.list_browser_profiles(user_id)?;
        if existing.len() >= MAX_PROFILES_PER_USER {
            return Err(SeraError::InvalidState(format!(
                "at most {MAX_PROFILES_PER_USER} browser profiles"
            )));
        }
        let base = slugify(&name);
        let id = unique_id(&base, |candidate| {
            existing.iter().any(|profile| profile.id == candidate)
        })?;
        let created_at = now();
        let connection = self.connection()?;
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO browser_profiles(user_id, id, name, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![user_id, id, name, created_at],
        )?;
        if inserted != 1 {
            return Err(SeraError::InvalidState(
                "that profile already exists; try again".into(),
            ));
        }
        Ok(BrowserProfile {
            id,
            name,
            is_default: false,
            created_at,
            last_used_at: None,
        })
    }

    pub fn rename_browser_profile(
        &self,
        user_id: &str,
        id: &str,
        name: &str,
    ) -> Result<BrowserProfile> {
        let name = clean_name(name)?;
        self.get_browser_profile(user_id, id)?
            .ok_or_else(|| SeraError::NotFound(format!("browser profile {id}")))?;
        let connection = self.connection()?;
        connection.execute(
            "UPDATE browser_profiles SET name = ?3 WHERE user_id = ?1 AND id = ?2",
            params![user_id, id, name],
        )?;
        drop(connection);
        self.get_browser_profile(user_id, id)?
            .ok_or_else(|| SeraError::NotFound(format!("browser profile {id}")))
    }

    /// Delete a profile row and point its bots back at the default profile.
    /// `default` is refused. Returns the bots that were reassigned.
    pub fn delete_browser_profile(&self, user_id: &str, id: &str) -> Result<Vec<String>> {
        if id == DEFAULT_PROFILE {
            return Err(SeraError::InvalidState(
                "the default browser profile cannot be deleted".into(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let removed = transaction.execute(
            "DELETE FROM browser_profiles WHERE user_id = ?1 AND id = ?2",
            params![user_id, id],
        )?;
        if removed != 1 {
            return Err(SeraError::NotFound(format!("browser profile {id}")));
        }
        let mut reassigned = Vec::new();
        {
            let mut statement = transaction.prepare(
                "SELECT bot FROM bot_settings WHERE user_id = ?1 AND browser_profile = ?2
                 ORDER BY bot",
            )?;
            for bot in statement.query_map(params![user_id, id], |row| row.get::<_, String>(0))? {
                reassigned.push(bot?);
            }
        }
        transaction.execute(
            "UPDATE bot_settings SET browser_profile = NULL, updated_at = ?3
             WHERE user_id = ?1 AND browser_profile = ?2",
            params![user_id, id, now()],
        )?;
        transaction.commit()?;
        Ok(reassigned)
    }

    pub fn touch_browser_profile(&self, user_id: &str, id: &str) -> Result<()> {
        if id == DEFAULT_PROFILE {
            self.ensure_default_browser_profile(user_id)?;
        }
        let connection = self.connection()?;
        connection.execute(
            "UPDATE browser_profiles SET last_used_at = ?3 WHERE user_id = ?1 AND id = ?2",
            params![user_id, id, now()],
        )?;
        Ok(())
    }

    pub fn bot_settings(&self, user_id: &str, bot: &str) -> Result<BotSettings> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT display_name, description, browser_profile FROM bot_settings
                 WHERE user_id = ?1 AND bot = ?2",
                params![user_id, bot],
                |row| {
                    Ok(BotSettings {
                        display_name: row.get(0)?,
                        description: row.get(1)?,
                        browser_profile: row.get(2)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn put_bot_settings(&self, user_id: &str, bot: &str, settings: &BotSettings) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO bot_settings(user_id, bot, display_name, description, browser_profile, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(user_id, bot) DO UPDATE SET
                 display_name = excluded.display_name,
                 description = excluded.description,
                 browser_profile = excluded.browser_profile,
                 updated_at = excluded.updated_at",
            params![
                user_id,
                bot,
                settings.display_name,
                settings.description,
                settings.browser_profile,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn delete_bot_settings(&self, user_id: &str, bot: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM bot_settings WHERE user_id = ?1 AND bot = ?2",
            params![user_id, bot],
        )?;
        Ok(())
    }

    /// The profile id a bot's browser tools use: its assignment when that
    /// profile still exists, otherwise `default`.
    pub fn bot_browser_profile(&self, user_id: &str, bot: &str) -> Result<String> {
        let connection = self.connection()?;
        let assigned: Option<String> = connection
            .query_row(
                "SELECT s.browser_profile FROM bot_settings s
                 JOIN browser_profiles p
                   ON p.user_id = s.user_id AND p.id = s.browser_profile
                 WHERE s.user_id = ?1 AND s.bot = ?2",
                params![user_id, bot],
                |row| row.get(0),
            )
            .optional()?;
        Ok(assigned.unwrap_or_else(|| DEFAULT_PROFILE.to_owned()))
    }

    /// Profile id -> bot ids using it, over the user's `bot_ids` (their fleet).
    /// Bots without an assignment count toward `default`.
    pub fn browser_profile_bots(
        &self,
        user_id: &str,
        bot_ids: &[String],
    ) -> Result<HashMap<String, Vec<String>>> {
        let mut assigned: HashMap<String, String> = HashMap::new();
        {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT s.bot, s.browser_profile FROM bot_settings s
                 JOIN browser_profiles p
                   ON p.user_id = s.user_id AND p.id = s.browser_profile
                 WHERE s.user_id = ?1",
            )?;
            for row in statement.query_map(params![user_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })? {
                let (bot, profile) = row?;
                assigned.insert(bot, profile);
            }
        }
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        let mut sorted = bot_ids.to_vec();
        sorted.sort();
        sorted.dedup();
        for bot in sorted {
            let profile = assigned
                .remove(&bot)
                .unwrap_or_else(|| DEFAULT_PROFILE.to_owned());
            map.entry(profile).or_default().push(bot);
        }
        Ok(map)
    }
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        slug = "profile".into();
    }
    slug.truncate(32);
    slug.trim_end_matches('-').to_owned()
}

fn unique_id(base: &str, taken: impl Fn(&str) -> bool) -> Result<String> {
    if !taken(base) {
        return Ok(base.to_owned());
    }
    for n in 2..100 {
        let suffix = format!("-{n}");
        let mut stem = base.to_owned();
        stem.truncate(32usize.saturating_sub(suffix.len()));
        let candidate = format!("{}{suffix}", stem.trim_end_matches('-'));
        if !taken(&candidate) {
            return Ok(candidate);
        }
    }
    Err(SeraError::InvalidState(
        "too many profiles share that name; pick another".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_users() -> (Store, String, String) {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let other = store
            .create_user("microsoft:bp-other", "other@example.example", "Other")
            .unwrap()
            .id;
        (store, admin, other)
    }

    #[test]
    fn profile_ids_follow_the_contract_regex() {
        for ok in ["default", "a", "0abc", "work-2", &"a".repeat(32)] {
            assert!(valid_profile_id(ok), "{ok}");
        }
        for bad in ["", "-a", "A", "a_b", "a b", "..", "a/b", &"a".repeat(33)] {
            assert!(!valid_profile_id(bad), "{bad}");
        }
    }

    #[test]
    fn default_is_implicit_and_listed_first() {
        let (store, admin, _) = store_with_users();
        let list = store.list_browser_profiles(&admin).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].is_default && list[0].id == "default");
        let work = store.create_browser_profile(&admin, "Work Login!").unwrap();
        assert_eq!(work.id, "work-login");
        let list = store.list_browser_profiles(&admin).unwrap();
        assert_eq!(list[0].id, "default");
        assert_eq!(list[1].id, "work-login");
        // A name that slugs to an existing id gets a suffix.
        assert_eq!(
            store.create_browser_profile(&admin, "Default").unwrap().id,
            "default-2"
        );
    }

    #[test]
    fn max_eight_profiles_default_included() {
        let (store, admin, _) = store_with_users();
        for n in 0..7 {
            store
                .create_browser_profile(&admin, &format!("p{n}"))
                .unwrap();
        }
        assert_eq!(store.list_browser_profiles(&admin).unwrap().len(), 8);
        let err = store
            .create_browser_profile(&admin, "one more")
            .unwrap_err();
        assert!(matches!(err, SeraError::InvalidState(_)), "{err}");
    }

    #[test]
    fn names_are_validated_and_rename_works() {
        let (store, admin, _) = store_with_users();
        assert!(store.create_browser_profile(&admin, "  ").is_err());
        assert!(
            store
                .create_browser_profile(&admin, &"x".repeat(49))
                .is_err()
        );
        let p = store.create_browser_profile(&admin, "Shop").unwrap();
        let renamed = store
            .rename_browser_profile(&admin, &p.id, "Shopping")
            .unwrap();
        assert_eq!(
            (renamed.id.as_str(), renamed.name.as_str()),
            ("shop", "Shopping")
        );
        assert!(store.rename_browser_profile(&admin, "nope", "x").is_err());
    }

    #[test]
    fn delete_reassigns_bots_and_refuses_default() {
        let (store, admin, _) = store_with_users();
        let p = store.create_browser_profile(&admin, "Shop").unwrap();
        store
            .put_bot_settings(
                &admin,
                "researcher",
                &BotSettings {
                    browser_profile: Some(p.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            store.bot_browser_profile(&admin, "researcher").unwrap(),
            "shop"
        );
        let err = store.delete_browser_profile(&admin, "default").unwrap_err();
        assert!(matches!(err, SeraError::InvalidState(_)), "{err}");
        let moved = store.delete_browser_profile(&admin, "shop").unwrap();
        assert_eq!(moved, vec!["researcher"]);
        assert_eq!(
            store.bot_browser_profile(&admin, "researcher").unwrap(),
            "default"
        );
        assert_eq!(
            store
                .bot_settings(&admin, "researcher")
                .unwrap()
                .browser_profile,
            None
        );
        assert!(store.delete_browser_profile(&admin, "shop").is_err());
    }

    #[test]
    fn profiles_and_settings_are_per_user() {
        let (store, admin, other) = store_with_users();
        store.create_browser_profile(&admin, "Shop").unwrap();
        assert_eq!(store.list_browser_profiles(&other).unwrap().len(), 1);
        assert!(store.get_browser_profile(&other, "shop").unwrap().is_none());
        assert!(store.delete_browser_profile(&other, "shop").is_err());
        assert!(store.rename_browser_profile(&other, "shop", "x").is_err());
        store
            .put_bot_settings(
                &admin,
                "b",
                &BotSettings {
                    display_name: Some("Bee".into()),
                    browser_profile: Some("shop".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            store.bot_settings(&other, "b").unwrap(),
            BotSettings::default()
        );
        assert_eq!(store.bot_browser_profile(&other, "b").unwrap(), "default");
        // An assignment to a profile the user lacks falls back to default.
        store
            .put_bot_settings(
                &other,
                "b",
                &BotSettings {
                    browser_profile: Some("shop".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(store.bot_browser_profile(&other, "b").unwrap(), "default");
    }

    #[test]
    fn profile_dir_rejects_traversal_and_bad_ids() {
        let base = Path::new("/b");
        assert_eq!(
            profile_dir(base, "u-1", "work").unwrap(),
            Path::new("/b/u-1/work")
        );
        for bad in ["", "..", ".", "a/b", "a\\b", "A", "-x", "x.y", "../x"] {
            assert!(profile_dir(base, "u-1", bad).is_err(), "profile {bad}");
        }
        for bad in ["", "..", "a/b", "a b", "../x"] {
            assert!(profile_dir(base, bad, "work").is_err(), "user {bad}");
        }
    }

    #[test]
    fn remove_profile_dir_only_touches_the_users_profile() {
        let base = std::env::temp_dir().join(format!("serabot-bp-{}", uuid::Uuid::new_v4()));
        let mine = base.join("alice").join("work");
        let other_profile = base.join("alice").join("default");
        let other_user = base.join("bob").join("work");
        for dir in [&mine, &other_profile, &other_user] {
            std::fs::create_dir_all(dir.join("Default")).unwrap();
            std::fs::write(dir.join("Default").join("Cookies"), b"x").unwrap();
        }
        assert!(remove_profile_dir(&base, "alice", "work").unwrap());
        assert!(!mine.exists());
        assert!(other_profile.exists() && other_user.exists());
        // Idempotent; traversal ids are refused and delete nothing.
        assert!(!remove_profile_dir(&base, "alice", "work").unwrap());
        for (user, profile) in [("alice", ".."), ("..", "work"), ("alice", "../bob")] {
            assert!(remove_profile_dir(&base, user, profile).is_err());
        }
        assert!(other_profile.exists() && other_user.exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn remove_profile_dir_refuses_a_symlinked_profile() {
        let base = std::env::temp_dir().join(format!("serabot-bp-{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("serabot-bp-out-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(base.join("alice")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, base.join("alice").join("evil")).unwrap();
        assert!(remove_profile_dir(&base, "alice", "evil").is_err());
        assert!(outside.join("keep").exists());
        let _ = std::fs::remove_dir_all(base);
        let _ = std::fs::remove_dir_all(outside);
    }
}
