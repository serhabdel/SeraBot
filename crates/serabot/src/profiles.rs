//! Store access for bot avatars, group profiles and the onboarding interview.
//!
//! Every method takes the acting user's id and filters on it; there is no read
//! or write path that is not scoped to one user. Group rows additionally check
//! that the conversation belongs to that user before anything is stored.

use std::collections::HashMap;

use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};

use crate::error::{Result, SeraError};
use crate::store::Store;

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A stored, already normalised avatar image.
#[derive(Debug, Clone)]
pub struct StoredAvatar {
    pub mime: String,
    pub data: Vec<u8>,
    pub updated_at: String,
}

/// The optional description and avatar of one group (room) conversation.
#[derive(Debug, Clone)]
pub struct ConversationProfile {
    pub description: String,
    pub has_avatar: bool,
    pub updated_at: String,
}

/// Raw onboarding row; `onboarding.rs` owns the meaning of the fields.
#[derive(Debug, Clone)]
pub struct OnboardingRow {
    pub state: String,
    pub transcript_json: String,
    pub proposal_json: Option<String>,
    pub rounds: i64,
    /// Room the bot joins once the owner confirms (picked in the create sheet).
    pub group_id: Option<String>,
}

impl Store {
    pub fn set_bot_avatar(&self, user_id: &str, bot: &str, mime: &str, data: &[u8]) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO bot_avatars(user_id, bot, mime, data, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id, bot) DO UPDATE SET
                 mime=excluded.mime, data=excluded.data, updated_at=excluded.updated_at",
            params![user_id, bot, mime, data, now()],
        )?;
        Ok(())
    }

    pub fn get_bot_avatar(&self, user_id: &str, bot: &str) -> Result<Option<StoredAvatar>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT mime, data, updated_at FROM bot_avatars WHERE user_id = ?1 AND bot = ?2",
                params![user_id, bot],
                |row| {
                    Ok(StoredAvatar {
                        mime: row.get(0)?,
                        data: row.get(1)?,
                        updated_at: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(SeraError::from)
    }

    pub fn delete_bot_avatar(&self, user_id: &str, bot: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "DELETE FROM bot_avatars WHERE user_id = ?1 AND bot = ?2",
            params![user_id, bot],
        )?;
        Ok(changed > 0)
    }

    /// `bot -> updated_at` for every avatar the user has set; the desktop uses
    /// the stamp as a cache key.
    pub fn bot_avatar_versions(&self, user_id: &str) -> Result<HashMap<String, String>> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT bot, updated_at FROM bot_avatars WHERE user_id = ?1")?;
        let rows = statement.query_map(params![user_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = HashMap::new();
        for row in rows {
            let (bot, stamp): (String, String) = row?;
            out.insert(bot, stamp);
        }
        Ok(out)
    }

    /// `conversation id -> updated_at` for every group the user gave an image.
    pub fn conversation_avatar_versions(&self, user_id: &str) -> Result<HashMap<String, String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT conversation_id, updated_at FROM conversation_profiles
             WHERE user_id = ?1 AND avatar_data IS NOT NULL",
        )?;
        let rows = statement.query_map(params![user_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = HashMap::new();
        for row in rows {
            let (conversation, stamp): (String, String) = row?;
            out.insert(conversation, stamp);
        }
        Ok(out)
    }

    /// Remove everything profile-shaped that hangs off one bot.
    pub fn delete_bot_profile_rows(&self, user_id: &str, bot: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM bot_avatars WHERE user_id = ?1 AND bot = ?2",
            params![user_id, bot],
        )?;
        connection.execute(
            "DELETE FROM bot_onboarding WHERE user_id = ?1 AND bot = ?2",
            params![user_id, bot],
        )?;
        Ok(())
    }

    fn require_conversation(&self, user_id: &str, conversation_id: &str) -> Result<()> {
        if self.get_conversation(user_id, conversation_id)?.is_none() {
            return Err(SeraError::NotFound(format!(
                "conversation {conversation_id}"
            )));
        }
        Ok(())
    }

    pub fn get_conversation_profile(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<Option<ConversationProfile>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT description, avatar_data IS NOT NULL, updated_at
                 FROM conversation_profiles WHERE user_id = ?1 AND conversation_id = ?2",
                params![user_id, conversation_id],
                |row| {
                    Ok(ConversationProfile {
                        description: row.get(0)?,
                        has_avatar: row.get(1)?,
                        updated_at: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(SeraError::from)
    }

    pub fn set_conversation_description(
        &self,
        user_id: &str,
        conversation_id: &str,
        description: &str,
    ) -> Result<()> {
        self.require_conversation(user_id, conversation_id)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO conversation_profiles(user_id, conversation_id, description, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, conversation_id) DO UPDATE SET
                 description=excluded.description, updated_at=excluded.updated_at",
            params![user_id, conversation_id, description, now()],
        )?;
        Ok(())
    }

    pub fn set_conversation_avatar(
        &self,
        user_id: &str,
        conversation_id: &str,
        mime: &str,
        data: &[u8],
    ) -> Result<()> {
        self.require_conversation(user_id, conversation_id)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO conversation_profiles(user_id, conversation_id, avatar_mime, avatar_data, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id, conversation_id) DO UPDATE SET
                 avatar_mime=excluded.avatar_mime, avatar_data=excluded.avatar_data,
                 updated_at=excluded.updated_at",
            params![user_id, conversation_id, mime, data, now()],
        )?;
        Ok(())
    }

    pub fn get_conversation_avatar(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<Option<StoredAvatar>> {
        let connection = self.connection()?;
        let row: Option<(Option<String>, Option<Vec<u8>>, String)> = connection
            .query_row(
                "SELECT avatar_mime, avatar_data, updated_at FROM conversation_profiles
                 WHERE user_id = ?1 AND conversation_id = ?2",
                params![user_id, conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        Ok(match row {
            Some((Some(mime), Some(data), updated_at)) => Some(StoredAvatar {
                mime,
                data,
                updated_at,
            }),
            _ => None,
        })
    }

    pub fn delete_conversation_avatar(&self, user_id: &str, conversation_id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE conversation_profiles SET avatar_mime = NULL, avatar_data = NULL, updated_at = ?3
             WHERE user_id = ?1 AND conversation_id = ?2 AND avatar_data IS NOT NULL",
            params![user_id, conversation_id, now()],
        )?;
        Ok(changed > 0)
    }

    pub fn get_onboarding(&self, user_id: &str, bot: &str) -> Result<Option<OnboardingRow>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT state, transcript_json, proposal_json, rounds, group_id FROM bot_onboarding
                 WHERE user_id = ?1 AND bot = ?2",
                params![user_id, bot],
                |row| {
                    Ok(OnboardingRow {
                        state: row.get(0)?,
                        transcript_json: row.get(1)?,
                        proposal_json: row.get(2)?,
                        rounds: row.get(3)?,
                        group_id: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(SeraError::from)
    }

    pub fn put_onboarding(&self, user_id: &str, bot: &str, row: &OnboardingRow) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO bot_onboarding(user_id, bot, state, transcript_json, proposal_json, rounds, group_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(user_id, bot) DO UPDATE SET
                 state=excluded.state, transcript_json=excluded.transcript_json,
                 proposal_json=excluded.proposal_json, rounds=excluded.rounds, group_id=excluded.group_id,
                 updated_at=excluded.updated_at",
            params![
                user_id,
                bot,
                row.state,
                row.transcript_json,
                row.proposal_json,
                row.rounds,
                row.group_id,
                now()
            ],
        )?;
        Ok(())
    }
}
