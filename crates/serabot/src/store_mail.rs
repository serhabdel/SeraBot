//! Mail workspace persistence added in 0.74.14: snooze, contacts, saved searches
//! and the send-retry bookkeeping for `scheduled_mail`.
//!
//! Like the rest of the store, every method that touches a user's rows takes the
//! acting `user_id` first and filters on it. The two maintenance helpers that run
//! across users (`claim_due_snoozes`, `recover_stale_scheduled_mail`) are called
//! only by the background loop and are documented as such.

use rusqlite::params;
use serde::Serialize;

use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::mail_ux;
use crate::store::Store;

/// Saved searches kept per user.
pub const MAX_SAVED_SEARCHES: usize = 25;
/// Contacts kept per user; the least used are pruned.
pub const MAX_CONTACTS: i64 = 2000;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SnoozeRow {
    pub id: String,
    #[serde(skip)]
    pub user_id: String,
    pub account_label: String,
    pub folder: String,
    pub uid: u32,
    pub subject: String,
    pub from_addr: String,
    pub wake_at: i64,
    pub created_at: i64,
}

fn snooze_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SnoozeRow> {
    Ok(SnoozeRow {
        id: row.get(0)?,
        user_id: row.get(1)?,
        account_label: row.get(2)?,
        folder: row.get(3)?,
        uid: row.get(4)?,
        subject: row.get(5)?,
        from_addr: row.get(6)?,
        wake_at: row.get(7)?,
        created_at: row.get(8)?,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MailContactRow {
    pub email: String,
    pub name: String,
    pub uses: i64,
    pub last_used: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SavedSearchRow {
    pub id: String,
    pub name: String,
    pub query: String,
    pub created_at: i64,
}

const SNOOZE_COLUMNS: &str =
    "id, user_id, account_label, folder, uid, subject, from_addr, wake_at, created_at";

impl Store {
    // ------------------------------------------------------------ snooze --

    /// Hide one message until `wake_at`. Snoozing again moves the wake time.
    #[allow(clippy::too_many_arguments)]
    pub fn snooze_mail(
        &self,
        user_id: &str,
        account_label: &str,
        folder: &str,
        uid: u32,
        subject: &str,
        from_addr: &str,
        wake_at: i64,
    ) -> Result<SnoozeRow> {
        let id = new_id();
        let now = chrono::Utc::now().timestamp();
        let subject: String = subject.chars().take(300).collect();
        let from_addr: String = from_addr.chars().take(300).collect();
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        tx.execute(
            "DELETE FROM mail_snooze
             WHERE user_id = ?1 AND account_label = ?2 AND folder = ?3 AND uid = ?4",
            params![user_id, account_label, folder, uid],
        )?;
        tx.execute(
            "INSERT INTO mail_snooze (id, user_id, account_label, folder, uid,
                                      subject, from_addr, wake_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                user_id,
                account_label,
                folder,
                uid,
                subject,
                from_addr,
                wake_at,
                now
            ],
        )?;
        tx.commit()?;
        Ok(SnoozeRow {
            id,
            user_id: user_id.to_owned(),
            account_label: account_label.to_owned(),
            folder: folder.to_owned(),
            uid,
            subject,
            from_addr,
            wake_at,
            created_at: now,
        })
    }

    /// Bring a snoozed message back now. `false` when it was not snoozed.
    pub fn unsnooze_mail(
        &self,
        user_id: &str,
        account_label: &str,
        folder: &str,
        uid: u32,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "DELETE FROM mail_snooze
             WHERE user_id = ?1 AND account_label = ?2 AND folder = ?3 AND uid = ?4",
            params![user_id, account_label, folder, uid],
        )?;
        Ok(changed > 0)
    }

    pub fn list_snoozed_mail(
        &self,
        user_id: &str,
        account_label: Option<&str>,
    ) -> Result<Vec<SnoozeRow>> {
        let connection = self.connection()?;
        let mut out = Vec::new();
        if let Some(label) = account_label {
            let mut stmt = connection.prepare(&format!(
                "SELECT {SNOOZE_COLUMNS} FROM mail_snooze
                 WHERE user_id = ?1 AND account_label = ?2
                 ORDER BY wake_at ASC"
            ))?;
            for row in stmt.query_map(params![user_id, label], snooze_from_row)? {
                out.push(row?);
            }
        } else {
            let mut stmt = connection.prepare(&format!(
                "SELECT {SNOOZE_COLUMNS} FROM mail_snooze
                 WHERE user_id = ?1
                 ORDER BY wake_at ASC"
            ))?;
            for row in stmt.query_map(params![user_id], snooze_from_row)? {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// UIDs of one folder that are snoozed right now (hidden from its list).
    pub fn snoozed_uids(
        &self,
        user_id: &str,
        account_label: &str,
        folder: &str,
    ) -> Result<Vec<u32>> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT uid FROM mail_snooze
             WHERE user_id = ?1 AND account_label = ?2 AND folder = ?3",
        )?;
        let mut out = Vec::new();
        for row in stmt.query_map(params![user_id, account_label, folder], |r| {
            r.get::<_, u32>(0)
        })? {
            out.push(row?);
        }
        Ok(out)
    }

    /// Remove and return every snooze whose time has come. Cross-user on purpose:
    /// only the background wake loop calls it.
    pub fn claim_due_snoozes(&self, now_unix: i64, limit: usize) -> Result<Vec<SnoozeRow>> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(&format!(
            "DELETE FROM mail_snooze
             WHERE id IN (SELECT id FROM mail_snooze WHERE wake_at <= ?1
                          ORDER BY wake_at ASC LIMIT ?2)
             RETURNING {SNOOZE_COLUMNS}"
        ))?;
        let mut out = Vec::new();
        for row in stmt.query_map(params![now_unix, limit as i64], snooze_from_row)? {
            out.push(row?);
        }
        Ok(out)
    }

    // ---------------------------------------------------------- contacts --

    /// Remember addresses for autocomplete. `sent = true` counts a use (an
    /// address you wrote to ranks above one that only wrote to you).
    pub fn upsert_mail_contacts(
        &self,
        user_id: &str,
        contacts: &[(String, String)],
        sent: bool,
        now_unix: i64,
    ) -> Result<()> {
        if contacts.is_empty() {
            return Ok(());
        }
        let bump = i64::from(sent);
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        for (name, email) in contacts.iter().take(100) {
            let name: String = name.chars().take(120).collect();
            tx.execute(
                "INSERT INTO mail_contacts (user_id, email, name, uses, last_used)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(user_id, email) DO UPDATE SET
                    uses = uses + ?4,
                    last_used = MAX(last_used, excluded.last_used),
                    name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE name END",
                params![user_id, email, name, bump, now_unix],
            )?;
        }
        tx.execute(
            "DELETE FROM mail_contacts
             WHERE user_id = ?1 AND email NOT IN (
                 SELECT email FROM mail_contacts WHERE user_id = ?1
                 ORDER BY uses DESC, last_used DESC LIMIT ?2)",
            params![user_id, MAX_CONTACTS],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Contacts whose address or name contains `q` (case-insensitive, plain
    /// substring, no wildcards), most used first. An empty `q` lists the top ones.
    pub fn search_mail_contacts(
        &self,
        user_id: &str,
        q: &str,
        limit: usize,
    ) -> Result<Vec<MailContactRow>> {
        let needle = q.trim().to_lowercase();
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT email, name, uses, last_used FROM mail_contacts
             WHERE user_id = ?1
               AND (?2 = '' OR instr(lower(email), ?2) > 0 OR instr(lower(name), ?2) > 0)
             ORDER BY uses DESC, last_used DESC, email ASC
             LIMIT ?3",
        )?;
        let mut out = Vec::new();
        for row in stmt.query_map(params![user_id, needle, limit.clamp(1, 50) as i64], |r| {
            Ok(MailContactRow {
                email: r.get(0)?,
                name: r.get(1)?,
                uses: r.get(2)?,
                last_used: r.get(3)?,
            })
        })? {
            out.push(row?);
        }
        Ok(out)
    }

    // ---------------------------------------------------- saved searches --

    pub fn create_saved_search(
        &self,
        user_id: &str,
        name: &str,
        query: &str,
    ) -> Result<SavedSearchRow> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
            return Err(SeraError::Validation(
                "a saved search name is 1-40 characters".into(),
            ));
        }
        let query = query.trim();
        mail_ux::parse_query(query).map_err(SeraError::Validation)?;
        let id = new_id();
        let now = chrono::Utc::now().timestamp();
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        let existing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM mail_saved_searches WHERE user_id = ?1",
            params![user_id],
            |r| r.get(0),
        )?;
        if existing as usize >= MAX_SAVED_SEARCHES {
            return Err(SeraError::Validation(format!(
                "at most {MAX_SAVED_SEARCHES} saved searches are allowed"
            )));
        }
        tx.execute(
            "INSERT INTO mail_saved_searches (id, user_id, name, query, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, user_id, name, query, now],
        )?;
        tx.commit()?;
        Ok(SavedSearchRow {
            id,
            name: name.to_owned(),
            query: query.to_owned(),
            created_at: now,
        })
    }

    pub fn list_saved_searches(&self, user_id: &str) -> Result<Vec<SavedSearchRow>> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT id, name, query, created_at FROM mail_saved_searches
             WHERE user_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let mut out = Vec::new();
        for row in stmt.query_map(params![user_id], |r| {
            Ok(SavedSearchRow {
                id: r.get(0)?,
                name: r.get(1)?,
                query: r.get(2)?,
                created_at: r.get(3)?,
            })
        })? {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn delete_saved_search(&self, user_id: &str, id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "DELETE FROM mail_saved_searches WHERE id = ?1 AND user_id = ?2",
            params![id, user_id],
        )?;
        Ok(changed > 0)
    }

    // ------------------------------------------------- scheduled sends --

    /// Queue a send with an explicit kind (`later` or `undo`).
    #[allow(clippy::too_many_arguments)]
    pub fn create_scheduled_mail_kind(
        &self,
        user_id: &str,
        account_label: &str,
        to: &[String],
        cc: &[String],
        bcc: &[String],
        subject: &str,
        body: &str,
        body_html: Option<&str>,
        send_at: i64,
        kind: &str,
    ) -> Result<crate::store::ScheduledMailRow> {
        let row = self.create_scheduled_mail(
            user_id,
            account_label,
            to,
            cc,
            bcc,
            subject,
            body,
            body_html,
            send_at,
        )?;
        if kind != "later" {
            let connection = self.connection()?;
            connection.execute(
                "UPDATE scheduled_mail SET kind = ?3 WHERE id = ?1 AND user_id = ?2",
                params![row.id, user_id, kind],
            )?;
        }
        Ok(row)
    }

    /// Record one failed attempt for a row this process claimed. Transient
    /// failures go back to `pending` after a backoff; the rest, and the last
    /// attempt, become `failed`. Returns the status that was stored.
    pub fn retry_or_fail_scheduled_mail(
        &self,
        id: &str,
        error: &str,
        transient: bool,
        now_unix: i64,
    ) -> Result<&'static str> {
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        let attempts: i64 = tx
            .query_row(
                "SELECT attempts FROM scheduled_mail WHERE id = ?1 AND status = 'sending'",
                params![id],
                |r| r.get(0),
            )
            .map_err(|_| SeraError::NotFound(format!("scheduled mail {id}")))?;
        let attempts = attempts + 1;
        let clipped: String = error.chars().take(1000).collect();
        let delay = if transient {
            mail_ux::retry_delay_secs(attempts)
        } else {
            None
        };
        let status = match delay {
            Some(secs) => {
                tx.execute(
                    "UPDATE scheduled_mail
                     SET status = 'pending', attempts = ?2, last_error = ?3,
                         send_at = ?4, claimed_at = NULL
                     WHERE id = ?1 AND status = 'sending'",
                    params![id, attempts, clipped, now_unix + secs],
                )?;
                "pending"
            }
            None => {
                tx.execute(
                    "UPDATE scheduled_mail
                     SET status = 'failed', attempts = ?2, last_error = ?3
                     WHERE id = ?1 AND status = 'sending'",
                    params![id, attempts, clipped],
                )?;
                "failed"
            }
        };
        tx.commit()?;
        Ok(status)
    }

    /// Mark rows left in `sending` by a crashed or restarted process as failed.
    /// They are not retried: the message may already have been delivered, and a
    /// duplicate is worse than a late one. Cross-user; background loop only.
    pub fn recover_stale_scheduled_mail(&self, now_unix: i64) -> Result<usize> {
        let cutoff = now_unix - mail_ux::STALE_SENDING_SECS;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE scheduled_mail
             SET status = 'failed',
                 last_error = 'Interrupted while sending. It was not retried so it cannot be sent twice; check your Sent folder before sending again.'
             WHERE status = 'sending' AND (claimed_at IS NULL OR claimed_at < ?1)",
            params![cutoff],
        )?;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open(
            std::env::temp_dir().join(format!("serabot-store-mail-test-{}.db", new_id())),
            384,
            5000,
        )
        .unwrap()
    }

    fn member(store: &Store, subject: &str) -> String {
        store
            .create_user(
                &format!("microsoft:{subject}"),
                &format!("{subject}@example.example"),
                subject,
            )
            .unwrap()
            .id
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn snooze_hides_wakes_and_is_user_scoped() {
        let store = store();
        let alice = member(&store, "alice");
        let bob = member(&store, "bob");
        let row = store
            .snooze_mail(&alice, "", "INBOX", 7, "Hello", "a@example.example", 1000)
            .unwrap();
        assert_eq!(row.uid, 7);
        assert_eq!(store.snoozed_uids(&alice, "", "INBOX").unwrap(), vec![7]);
        assert!(store.snoozed_uids(&bob, "", "INBOX").unwrap().is_empty());
        assert!(
            store
                .snoozed_uids(&alice, "", "Archive")
                .unwrap()
                .is_empty()
        );
        assert!(!store.unsnooze_mail(&bob, "", "INBOX", 7).unwrap());

        // Snoozing again moves the wake time rather than adding a second row.
        store
            .snooze_mail(&alice, "", "INBOX", 7, "Hello", "a@example.example", 2000)
            .unwrap();
        let listed = store.list_snoozed_mail(&alice, None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].wake_at, 2000);
        assert!(store.list_snoozed_mail(&bob, None).unwrap().is_empty());

        assert!(store.claim_due_snoozes(1999, 10).unwrap().is_empty());
        let due = store.claim_due_snoozes(2000, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].user_id, alice);
        assert!(store.snoozed_uids(&alice, "", "INBOX").unwrap().is_empty());

        store
            .snooze_mail(&alice, "work", "INBOX", 9, "x", "y", 5000)
            .unwrap();
        assert!(store.unsnooze_mail(&alice, "work", "INBOX", 9).unwrap());
        assert!(!store.unsnooze_mail(&alice, "work", "INBOX", 9).unwrap());
    }

    #[test]
    fn contacts_rank_by_sends_and_search_is_plain_substring() {
        let store = store();
        let alice = member(&store, "alice");
        let bob = member(&store, "bob");
        let seen = vec![
            ("Newsletter".to_owned(), "news@list.example".to_owned()),
            (String::new(), "anna@example.example".to_owned()),
        ];
        store
            .upsert_mail_contacts(&alice, &seen, false, 100)
            .unwrap();
        let sent = vec![("Anna Lee".to_owned(), "anna@example.example".to_owned())];
        store
            .upsert_mail_contacts(&alice, &sent, true, 200)
            .unwrap();
        store
            .upsert_mail_contacts(&alice, &sent, true, 300)
            .unwrap();

        let all = store.search_mail_contacts(&alice, "", 10).unwrap();
        assert_eq!(all[0].email, "anna@example.example");
        assert_eq!(all[0].name, "Anna Lee");
        assert_eq!(all[0].uses, 2);
        assert_eq!(all[0].last_used, 300);
        assert_eq!(all[1].uses, 0);

        assert_eq!(
            store.search_mail_contacts(&alice, "LEE", 10).unwrap().len(),
            1
        );
        // `%` and `_` are plain characters, not wildcards.
        assert!(
            store
                .search_mail_contacts(&alice, "%", 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .search_mail_contacts(&alice, "a_n", 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .search_mail_contacts(&bob, "anna", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn saved_searches_validate_cap_and_scope() {
        let store = store();
        let alice = member(&store, "alice");
        let bob = member(&store, "bob");
        let row = store
            .create_saved_search(&alice, " Unread from Anna ", "from:anna is:unread")
            .unwrap();
        assert_eq!(row.name, "Unread from Anna");
        assert!(store.create_saved_search(&alice, "", "x").is_err());
        assert!(store.create_saved_search(&alice, "ok", "is:weird").is_err());
        assert!(
            store
                .create_saved_search(&alice, &"n".repeat(41), "x")
                .is_err()
        );
        assert_eq!(store.list_saved_searches(&alice).unwrap().len(), 1);
        assert!(store.list_saved_searches(&bob).unwrap().is_empty());
        assert!(!store.delete_saved_search(&bob, &row.id).unwrap());
        assert!(store.delete_saved_search(&alice, &row.id).unwrap());
        for i in 0..MAX_SAVED_SEARCHES {
            store
                .create_saved_search(&alice, &format!("s{i}"), "invoice")
                .unwrap();
        }
        assert!(
            store
                .create_saved_search(&alice, "one too many", "invoice")
                .is_err()
        );
    }

    #[test]
    fn scheduled_sends_retry_with_backoff_then_fail() {
        let store = store();
        let alice = member(&store, "alice");
        let to = strings(&["a@example.example"]);
        let row = store
            .create_scheduled_mail_kind(&alice, "", &to, &[], &[], "s", "b", None, 100, "undo")
            .unwrap();
        let claimed = store.claim_due_scheduled_mail(100, 10).unwrap();
        assert_eq!(claimed.len(), 1);

        assert_eq!(
            store
                .retry_or_fail_scheduled_mail(&row.id, "smtp down", true, 1000)
                .unwrap(),
            "pending"
        );
        assert!(store.claim_due_scheduled_mail(1059, 10).unwrap().is_empty());
        assert_eq!(store.claim_due_scheduled_mail(1060, 10).unwrap().len(), 1);
        assert_eq!(
            store
                .retry_or_fail_scheduled_mail(&row.id, "smtp down", true, 2000)
                .unwrap(),
            "pending"
        );
        assert_eq!(store.claim_due_scheduled_mail(2300, 10).unwrap().len(), 1);
        assert_eq!(
            store
                .retry_or_fail_scheduled_mail(&row.id, "smtp down", true, 3000)
                .unwrap(),
            "failed"
        );
        // A failed undo hold stays visible so the user learns the mail did not go.
        let listed = store.list_scheduled_mail(&alice, None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, "failed");
        assert!(
            listed[0]
                .last_error
                .as_deref()
                .unwrap()
                .contains("smtp down")
        );
    }

    #[test]
    fn permanent_errors_fail_at_once_and_undo_rows_hide_until_failed() {
        let store = store();
        let alice = member(&store, "alice");
        let to = strings(&["a@example.example"]);
        let later = store
            .create_scheduled_mail_kind(&alice, "", &to, &[], &[], "later", "b", None, 100, "later")
            .unwrap();
        let undo = store
            .create_scheduled_mail_kind(&alice, "", &to, &[], &[], "undo", "b", None, 100, "undo")
            .unwrap();
        let subjects: Vec<String> = store
            .list_scheduled_mail(&alice, None)
            .unwrap()
            .into_iter()
            .map(|r| r.subject)
            .collect();
        assert_eq!(subjects, vec!["later".to_owned()]);
        store.claim_due_scheduled_mail(100, 10).unwrap();
        assert_eq!(
            store
                .retry_or_fail_scheduled_mail(&later.id, "blocked", false, 200)
                .unwrap(),
            "failed"
        );
        assert!(store.complete_scheduled_mail(&undo.id, 201).is_ok());
    }

    #[test]
    fn stale_sending_rows_are_failed_not_retried() {
        let store = store();
        let alice = member(&store, "alice");
        let to = strings(&["a@example.example"]);
        let row = store
            .create_scheduled_mail_kind(&alice, "", &to, &[], &[], "s", "b", None, 100, "later")
            .unwrap();
        store.claim_due_scheduled_mail(100, 10).unwrap();
        assert_eq!(store.recover_stale_scheduled_mail(100 + 599).unwrap(), 0);
        assert_eq!(store.recover_stale_scheduled_mail(100 + 601).unwrap(), 1);
        let listed = store.list_scheduled_mail(&alice, None).unwrap();
        assert_eq!(listed[0].id, row.id);
        assert_eq!(listed[0].status, "failed");
        assert!(
            listed[0]
                .last_error
                .as_deref()
                .unwrap()
                .contains("Interrupted")
        );
        // Nothing is left to claim, so the message cannot go out twice.
        assert!(
            store
                .claim_due_scheduled_mail(10_000, 10)
                .unwrap()
                .is_empty()
        );
    }
}
