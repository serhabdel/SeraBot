//! WhatsApp as a linked device of a member's own (spare) number.
//!
//! Design: `docs/specs/whatsapp-linked-device.md`. This module holds the parts
//! that do not need the WhatsApp client: who may reach the bot (the per-member
//! allowlist), how a WhatsApp chat maps to a SeraBot conversation, and the
//! account rows. The client itself lives in [`client`] behind the
//! `whatsapp-link` feature.
//!
//! Every row is UID-scoped. A chat that is not on the member's allowlist is
//! dropped before anything is stored or sent to a model.

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::error::{Result, SeraError};
use crate::store::Store;

#[cfg(feature = "whatsapp-link")]
pub mod client;

/// Conversation origin for chats that arrive over the linked device.
pub const ORIGIN_WHATSAPP_LINK: &str = "whatsapp_link";
/// The tool a bot uses to send to an allowed chat on its own initiative.
pub const SEND_TOOL: &str = "whatsapp_link_send";
/// Sends a file into the WhatsApp chat this conversation came from (0.63).
/// A reply the member asked for, like the text reply: not approval-gated.
pub const REPLY_FILE_TOOL: &str = "whatsapp_link_reply_file";
const MAX_ALLOWLIST: usize = 50;
const PERSONAL_SERVER: &str = "s.whatsapp.net";

/// A personal chat id (`<digits>@s.whatsapp.net`) from a phone number in any
/// common spelling (`+33 6 12 34 56 78`, `0033612345678`) or from a JID.
/// Groups and other servers are refused: the linked device only serves
/// one-to-one chats.
pub fn normalize_jid(value: &str) -> Result<String> {
    let value = value.trim();
    let (number, server) = match value.split_once('@') {
        Some((user, server)) => (user.split(':').next().unwrap_or(user), server),
        None => (value, PERSONAL_SERVER),
    };
    if server != PERSONAL_SERVER {
        return Err(SeraError::Validation(
            "only personal chats can be allowed, not groups or channels".into(),
        ));
    }
    let mut digits: String = number.chars().filter(char::is_ascii_digit).collect();
    if number.trim_start().starts_with("00") {
        digits = digits.trim_start_matches("00").to_string();
    }
    if !(7..=15).contains(&digits.len()) {
        return Err(SeraError::Validation(
            "a phone number has 7 to 15 digits, with its country code".into(),
        ));
    }
    Ok(format!("{digits}@{PERSONAL_SERVER}"))
}

/// The SeraBot conversation that carries one member's WhatsApp chat. Stable,
/// opaque (no phone number in conversation ids or logs) and per member.
pub fn conversation_id_for(user_id: &str, jid: &str) -> String {
    let digest = crate::util::sha256_hex(format!("{user_id}\n{jid}"));
    format!("wa-{}", &digest[..20])
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LinkAccount {
    pub status: String,
    pub phone: Option<String>,
    pub allowlist: Vec<String>,
    pub updated_at: String,
}

/// What to do with one incoming WhatsApp message, before any download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundDecision {
    /// Process it in this conversation.
    Accept { conversation_id: String },
    /// Drop it silently: our own echo, a group, or a chat not on the list.
    Drop,
}

/// The allowlist gate. Messages the linked number sent itself (including the
/// bot's own replies) are never processed, which also prevents reply loops.
pub fn decide_inbound(
    user_id: &str,
    allowlist: &[String],
    chat: &str,
    is_from_me: bool,
    is_group: bool,
) -> InboundDecision {
    if is_from_me || is_group {
        return InboundDecision::Drop;
    }
    let Ok(jid) = normalize_jid(chat) else {
        return InboundDecision::Drop;
    };
    if !allowlist.iter().any(|allowed| allowed == &jid) {
        return InboundDecision::Drop;
    }
    InboundDecision::Accept {
        conversation_id: conversation_id_for(user_id, &jid),
    }
}

impl Store {
    pub fn whatsapp_link_account(&self, user_id: &str) -> Result<Option<LinkAccount>> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT status, phone, allowlist_json, updated_at
                 FROM whatsapp_link_accounts WHERE user_id = ?1",
                params![user_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        Ok(
            row.map(|(status, phone, allowlist, updated_at)| LinkAccount {
                status,
                phone,
                allowlist: serde_json::from_str(&allowlist).unwrap_or_default(),
                updated_at,
            }),
        )
    }

    /// Record the link state. `phone` is kept when `None` is passed.
    pub fn set_whatsapp_link_status(
        &self,
        user_id: &str,
        status: &str,
        phone: Option<&str>,
    ) -> Result<()> {
        if !matches!(status, "pairing" | "linked" | "signed_out") {
            return Err(SeraError::Validation("unknown link status".into()));
        }
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO whatsapp_link_accounts(user_id, status, phone, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO UPDATE SET status = excluded.status,
                 phone = COALESCE(excluded.phone, whatsapp_link_accounts.phone),
                 updated_at = excluded.updated_at",
            params![user_id, status, phone, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Replace the member's allowed chats. Every entry is normalised; the
    /// list is short on purpose.
    pub fn set_whatsapp_link_allowlist(
        &self,
        user_id: &str,
        entries: &[String],
    ) -> Result<Vec<String>> {
        if entries.len() > MAX_ALLOWLIST {
            return Err(SeraError::Validation(format!(
                "at most {MAX_ALLOWLIST} allowed chats"
            )));
        }
        let mut list = Vec::new();
        for entry in entries {
            let jid = normalize_jid(entry)?;
            if !list.contains(&jid) {
                list.push(jid);
            }
        }
        if self.whatsapp_link_account(user_id)?.is_none() {
            self.set_whatsapp_link_status(user_id, "signed_out", None)?;
        }
        let connection = self.connection()?;
        connection.execute(
            "UPDATE whatsapp_link_accounts SET allowlist_json = ?2, updated_at = ?3
             WHERE user_id = ?1",
            params![
                user_id,
                serde_json::to_string(&list)?,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(list)
    }

    /// Remember which chat a conversation carries, and mark its origin so
    /// replies go back over WhatsApp.
    pub fn bind_whatsapp_link_chat(
        &self,
        user_id: &str,
        conversation_id: &str,
        jid: &str,
    ) -> Result<()> {
        {
            let connection = self.connection()?;
            connection.execute(
                "INSERT INTO whatsapp_link_chats(user_id, conversation_id, jid, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(user_id, conversation_id) DO NOTHING",
                params![user_id, conversation_id, jid, Utc::now().to_rfc3339()],
            )?;
        }
        self.set_conversation_origin(user_id, conversation_id, ORIGIN_WHATSAPP_LINK)
    }

    /// The chat a member's conversation carries, if it is a WhatsApp chat.
    pub fn whatsapp_link_chat(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<Option<String>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT jid FROM whatsapp_link_chats WHERE user_id = ?1 AND conversation_id = ?2",
                params![user_id, conversation_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Members whose number is linked, for starting their clients at boot.
    pub fn whatsapp_link_linked_users(&self) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT user_id FROM whatsapp_link_accounts WHERE status = 'linked'")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// Forget the link: the account row and the chat map. Conversations and
    /// their messages stay, like any other chat history.
    pub fn delete_whatsapp_link(&self, user_id: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM whatsapp_link_chats WHERE user_id = ?1",
            params![user_id],
        )?;
        connection.execute(
            "DELETE FROM whatsapp_link_accounts WHERE user_id = ?1",
            params![user_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_normalise_to_personal_jids() {
        let jid = "33612345678@s.whatsapp.net";
        assert_eq!(normalize_jid("+33 6 12 34 56 78").unwrap(), jid);
        assert_eq!(normalize_jid("0033612345678").unwrap(), jid);
        assert_eq!(normalize_jid("33612345678:12@s.whatsapp.net").unwrap(), jid);
        assert!(normalize_jid("12036302@g.us").is_err());
        assert!(normalize_jid("123").is_err());
    }

    #[test]
    fn only_allowed_personal_chats_from_others_get_through() {
        let allow = vec!["33612345678@s.whatsapp.net".to_string()];
        let chat = "33612345678@s.whatsapp.net";
        assert!(matches!(
            decide_inbound("u1", &allow, chat, false, false),
            InboundDecision::Accept { .. }
        ));
        assert_eq!(
            decide_inbound("u1", &allow, chat, true, false),
            InboundDecision::Drop
        );
        assert_eq!(
            decide_inbound("u1", &allow, chat, false, true),
            InboundDecision::Drop
        );
        assert_eq!(
            decide_inbound("u1", &allow, "33700000000@s.whatsapp.net", false, false),
            InboundDecision::Drop
        );
        assert_eq!(
            decide_inbound("u1", &[], chat, false, false),
            InboundDecision::Drop
        );
    }

    #[test]
    fn conversation_ids_are_opaque_and_per_member() {
        let jid = "33612345678@s.whatsapp.net";
        let a = conversation_id_for("u1", jid);
        assert!(a.starts_with("wa-") && !a.contains("3361"));
        assert_ne!(a, conversation_id_for("u2", jid));
        crate::orchestration::validate_conversation_id(&a).unwrap();
    }

    #[test]
    fn rows_are_scoped_to_their_member() {
        let store = Store::open_in_memory(8).unwrap();
        let alice = store.admin_user_id().unwrap();
        let bob = store
            .create_user("microsoft:wa-bob", "bob@example.example", "Bob")
            .unwrap()
            .id;
        let list = store
            .set_whatsapp_link_allowlist(&alice, &["+33 6 12 34 56 78".into()])
            .unwrap();
        assert_eq!(list, vec!["33612345678@s.whatsapp.net".to_string()]);
        store
            .set_whatsapp_link_status(&alice, "linked", Some("33700000000"))
            .unwrap();
        let conversation = conversation_id_for(&alice, &list[0]);
        store
            .bind_whatsapp_link_chat(&alice, &conversation, &list[0])
            .unwrap();

        assert_eq!(
            store
                .whatsapp_link_chat(&alice, &conversation)
                .unwrap()
                .as_deref(),
            Some(list[0].as_str())
        );
        assert!(
            store
                .whatsapp_link_chat(&bob, &conversation)
                .unwrap()
                .is_none()
        );
        assert!(store.whatsapp_link_account(&bob).unwrap().is_none());
        let account = store.whatsapp_link_account(&alice).unwrap().unwrap();
        assert_eq!(account.status, "linked");
        assert_eq!(account.allowlist, list);

        store.delete_whatsapp_link(&alice).unwrap();
        assert!(store.whatsapp_link_account(&alice).unwrap().is_none());
        assert!(
            store
                .whatsapp_link_chat(&alice, &conversation)
                .unwrap()
                .is_none()
        );
    }
}
