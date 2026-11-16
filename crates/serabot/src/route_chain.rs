//! Planning for a turn's model route chain.
//!
//! A turn is pinned to a head route plus up to two fallbacks (see
//! [`crate::model_router::PinnedRoute`]). Each model call walks the chain as a
//! list of *candidates*, each one provider, model and concrete account:
//!
//! 1. the head provider's accounts that are usable now (not disabled, not
//!    `needs_reauth`, not cooling, not quota-held), best first;
//! 2. fallback 1's accounts, then fallback 2's.
//!
//! A fallback on another provider is only a candidate when the bot's policy has
//! `allow_cross_provider` and the provider is egress-approved for the user.
//! Everything here is scoped to the acting user: holds, accounts and affinity
//! are looked up under `user_id` and never shared.
//!
//! Failures leave *holds* in `provider_holds` so the next call skips what just
//! failed: a rate limit until its `retry_after`, a quota until it resets, a
//! model failure for ten minutes. The attempt loop itself lives with the
//! runtime (`runtime/failover.rs`); this module plans and records.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;

use crate::accounts::{AccountStatus, DEFAULT_ACCOUNT};
use crate::config::FailoverPolicy;
use crate::domain::new_id;
use crate::error::Result;
use crate::llm_failure::FailKind;
use crate::model_router::{ModelRouter, ModelSelection, PinnedRoute};
use crate::store::{Store, now_string, to_string};

/// How long a model failure keeps a model off an account.
pub const MODEL_HOLD: Duration = Duration::from_secs(10 * 60);
/// Hold after a rate limit that gave no `retry_after`.
pub const DEFAULT_RATE_HOLD: Duration = Duration::from_secs(30);
/// Hold after "overloaded" or "provider down".
pub const OVERLOAD_HOLD: Duration = Duration::from_secs(60);
/// Hold after a quota failure that gave no reset time.
pub const DEFAULT_QUOTA_HOLD: Duration = Duration::from_secs(3600);
/// Hold after the provider forbade the account.
pub const FORBIDDEN_HOLD: Duration = Duration::from_secs(10 * 60);
/// Longest hold any failure may set.
const MAX_HOLD: Duration = Duration::from_secs(24 * 3600);
/// Oldest quota snapshot that may keep an account out of rotation.
pub const QUOTA_SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// The `model` of a hold that covers every model of an account.
pub const ALL_MODELS: &str = "*";

/// A reason kept on a route event or an `all_routes_failed` error. A stable
/// word, never provider text.
pub fn reason_code(kind: &FailKind) -> &'static str {
    match kind {
        FailKind::RateLimited { .. } => "rate_limited",
        FailKind::Overloaded => "overloaded",
        FailKind::ModelUnavailable => "model_unavailable",
        FailKind::QuotaExhausted { .. } => "quota_exhausted",
        FailKind::CredentialsRejected => "credentials_rejected",
        FailKind::AccountForbidden => "account_forbidden",
        FailKind::ProviderDown => "provider_down",
        FailKind::Transport => "transport",
        FailKind::ContextTooLong => "context_too_long",
    }
}

/// One active hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hold {
    pub provider: String,
    pub account_id: String,
    pub model: String,
    pub kind: String,
    pub until: DateTime<Utc>,
    pub reason: String,
}

/// One recorded switch, for the chat's "Switched to X (reason)" rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteEvent {
    pub id: String,
    pub conversation_id: Option<String>,
    pub task_id: Option<String>,
    pub bot: Option<String>,
    pub from_provider: Option<String>,
    pub from_model: Option<String>,
    pub from_account: Option<String>,
    pub to_provider: Option<String>,
    pub to_model: Option<String>,
    pub to_account: Option<String>,
    pub reason: String,
    pub created_at: String,
}

/// What a new route event carries.
pub struct NewRouteEvent<'a> {
    pub conversation_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub bot: Option<&'a str>,
    pub from: &'a ModelSelection,
    pub to: &'a ModelSelection,
    pub reason: &'a str,
}

impl Store {
    /// Hold `model` (or [`ALL_MODELS`]) of one account off until `until`.
    /// A later hold of the same kind replaces the earlier one.
    #[allow(clippy::too_many_arguments)]
    pub fn set_provider_hold(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        model: &str,
        kind: &str,
        until: DateTime<Utc>,
        reason: &str,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO provider_holds(user_id, provider, account_id, model, kind, until, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(user_id, provider, account_id, model, kind)
             DO UPDATE SET until = excluded.until, reason = excluded.reason",
            params![
                user_id,
                provider,
                account_id,
                model,
                kind,
                to_string(until),
                reason
            ],
        )?;
        Ok(())
    }

    /// The acting user's holds that have not expired. Expired rows are
    /// removed on the way.
    pub fn active_provider_holds(&self, user_id: &str, now: DateTime<Utc>) -> Result<Vec<Hold>> {
        let connection = self.connection()?;
        let cutoff = to_string(now);
        connection.execute(
            "DELETE FROM provider_holds WHERE user_id = ?1 AND until <= ?2",
            params![user_id, cutoff],
        )?;
        let mut statement = connection.prepare(
            "SELECT provider, account_id, model, kind, until, reason FROM provider_holds
             WHERE user_id = ?1 ORDER BY until LIMIT 256",
        )?;
        let rows = statement.query_map(params![user_id], |row| {
            let until: String = row.get(4)?;
            Ok(Hold {
                provider: row.get(0)?,
                account_id: row.get(1)?,
                model: row.get(2)?,
                kind: row.get(3)?,
                until: DateTime::parse_from_rfc3339(&until)
                    .map(|value| value.with_timezone(&Utc))
                    .unwrap_or(now),
                reason: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Remove every hold of one account (a fresh sign-in or a manual retry).
    pub fn clear_provider_holds(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
    ) -> Result<usize> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "DELETE FROM provider_holds WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3",
            params![user_id, provider, account_id],
        )?)
    }

    /// Note that an account just served a call.
    pub fn touch_provider_account(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE provider_accounts SET last_used_at = ?4
             WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3",
            params![user_id, provider, account_id, now_string()],
        )?;
        Ok(())
    }

    /// Record a switch. Returns the event id.
    pub fn record_route_event(&self, user_id: &str, event: &NewRouteEvent<'_>) -> Result<String> {
        let id = new_id();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO route_events(id, user_id, conversation_id, task_id, bot,
                 from_provider, from_model, from_account, to_provider, to_model, to_account,
                 reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                id,
                user_id,
                event.conversation_id,
                event.task_id,
                event.bot,
                event.from.provider,
                event.from.model,
                event.from.account,
                event.to.provider,
                event.to.model,
                event.to.account,
                event.reason,
                now_string()
            ],
        )?;
        Ok(id)
    }

    /// The acting user's switches, newest first. Pass a conversation to
    /// narrow to it.
    pub fn route_events(
        &self,
        user_id: &str,
        conversation_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RouteEvent>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, conversation_id, task_id, bot, from_provider, from_model, from_account,
                    to_provider, to_model, to_account, reason, created_at
             FROM route_events
             WHERE user_id = ?1 AND (?2 IS NULL OR conversation_id = ?2)
             ORDER BY created_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![user_id, conversation_id, limit.min(200) as i64],
            |row| {
                Ok(RouteEvent {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    task_id: row.get(2)?,
                    bot: row.get(3)?,
                    from_provider: row.get(4)?,
                    from_model: row.get(5)?,
                    from_account: row.get(6)?,
                    to_provider: row.get(7)?,
                    to_model: row.get(8)?,
                    to_account: row.get(9)?,
                    reason: row.get(10)?,
                    created_at: row.get(11)?,
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// One account as the planner sees it.
#[derive(Debug, Clone)]
pub struct PoolAccount {
    pub id: String,
    pub status: AccountStatus,
    pub priority: i64,
    pub weight: i64,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub quota: Option<Value>,
    pub last_used_at: Option<String>,
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// The acting user's accounts for `provider`, plus the pre-accounts
/// `default` credential when one exists. Empty when the user serves the
/// provider by a grant or a server key.
pub fn pool_for(store: &Store, user_id: &str, provider: &str) -> Result<Vec<PoolAccount>> {
    let mut pool: Vec<PoolAccount> = store
        .provider_accounts(user_id, Some(provider))?
        .into_iter()
        .map(|account| PoolAccount {
            id: account.account_id,
            status: account.status,
            priority: account.priority,
            weight: account.weight,
            cooldown_until: account.cooldown_until.as_deref().and_then(parse_time),
            quota: account.quota,
            last_used_at: account.last_used_at,
        })
        .collect();
    if store
        .usable_account_ids(user_id, provider)?
        .iter()
        .any(|id| id == DEFAULT_ACCOUNT)
    {
        pool.push(PoolAccount {
            id: DEFAULT_ACCOUNT.into(),
            status: AccountStatus::Ok,
            priority: 0,
            weight: 1,
            cooldown_until: None,
            quota: None,
            last_used_at: None,
        });
    }
    Ok(pool)
}

/// When a quota snapshot keeps its account out of rotation: a snapshot at most
/// [`QUOTA_SNAPSHOT_MAX_AGE`] old with a window at zero remaining and a reset
/// in the future. Returns that reset.
pub fn quota_held_until(quota: &Value, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let sampled = parse_time(quota.get("sampled_at")?.as_str()?)?;
    let age = now.signed_duration_since(sampled);
    if age < chrono::Duration::zero() || age.to_std().ok()? > QUOTA_SNAPSHOT_MAX_AGE {
        return None;
    }
    quota
        .get("windows")?
        .as_array()?
        .iter()
        .filter(|window| {
            window
                .get("remaining")
                .and_then(Value::as_f64)
                .is_some_and(|remaining| remaining <= 0.0)
        })
        .filter_map(|window| parse_time(window.get("resets_at")?.as_str()?))
        .filter(|reset| *reset > now)
        .max()
}

/// Why an account cannot be used right now, with the time it may come back
/// when that is known. `None` means usable.
fn account_block(
    account: &PoolAccount,
    provider: &str,
    model: &str,
    holds: &[Hold],
    now: DateTime<Utc>,
) -> Option<(&'static str, Option<DateTime<Utc>>)> {
    match account.status {
        AccountStatus::Disabled => return Some(("disabled", None)),
        AccountStatus::NeedsReauth => return Some(("needs_reauth", None)),
        _ => {}
    }
    if let Some(until) = account.cooldown_until
        && until > now
    {
        return Some(("cooling", Some(until)));
    }
    if let Some(hold) = holds.iter().find(|hold| {
        hold.provider == provider
            && hold.account_id == account.id
            && (hold.model == ALL_MODELS || hold.model == model)
            && hold.until > now
    }) {
        return Some(("held", Some(hold.until)));
    }
    if let Some(until) = account
        .quota
        .as_ref()
        .and_then(|quota| quota_held_until(quota, now))
    {
        return Some(("quota_held", Some(until)));
    }
    None
}

/// One model call's candidate: provider, model, a concrete account (`None`
/// when the provider is served by a grant or a server key) and the thinking
/// level to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub selection: ModelSelection,
    pub thinking: Option<String>,
    /// 0 is the head; 1 and 2 are the fallbacks.
    pub hop: usize,
}

/// The ordered candidates for one model call.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub candidates: Vec<Candidate>,
    /// Why a hop or account contributed nothing, as `(label, reason)`.
    pub skipped: Vec<(String, &'static str)>,
    /// Earliest time a held or cooling account may be tried again.
    pub earliest_ready: Option<DateTime<Utc>>,
}

/// The key under which the account that last served a conversation's provider
/// is remembered, so the next call prefers it.
pub fn affinity_key(user_id: &str, conversation_id: &str, provider: &str) -> String {
    format!("route_affinity:{user_id}:{conversation_id}:{provider}")
}

/// Order a hop's usable accounts: the pinned account first, then the
/// conversation's last account, then priority descending, weight descending,
/// least recently used first.
fn order_pool(
    mut usable: Vec<PoolAccount>,
    pinned: Option<&str>,
    affinity: Option<&str>,
) -> Vec<PoolAccount> {
    usable.sort_by(|left, right| {
        let rank = |account: &PoolAccount| {
            if Some(account.id.as_str()) == pinned {
                0
            } else if Some(account.id.as_str()) == affinity {
                1
            } else {
                2
            }
        };
        rank(left)
            .cmp(&rank(right))
            .then(right.priority.cmp(&left.priority))
            .then(right.weight.cmp(&left.weight))
            .then(left.last_used_at.cmp(&right.last_used_at))
            .then(left.id.cmp(&right.id))
    });
    usable
}

/// Build the candidate list for one model call of a pinned chain.
#[allow(clippy::too_many_arguments)]
pub fn plan_candidates(
    store: &Store,
    router: &ModelRouter,
    user_id: &str,
    route: &PinnedRoute,
    head_thinking: Option<&str>,
    conversation_id: &str,
    now: DateTime<Utc>,
) -> Result<Plan> {
    let policy: FailoverPolicy = route.failover.unwrap_or_default();
    let mut hops: Vec<(ModelSelection, Option<String>)> =
        vec![(route.selection.clone(), head_thinking.map(str::to_owned))];
    hops.extend(
        route
            .fallbacks
            .iter()
            .map(|hop| (hop.selection.clone(), hop.thinking.clone())),
    );
    let holds = store.active_provider_holds(user_id, now)?;
    let single_hop = hops.len() == 1;
    let mut plan = Plan::default();
    let note_ready = |plan: &mut Plan, until: Option<DateTime<Utc>>| {
        if let Some(until) = until {
            plan.earliest_ready = Some(plan.earliest_ready.map_or(until, |at| at.min(until)));
        }
    };
    for (index, (selection, thinking)) in hops.iter().enumerate() {
        let label = format!("{}/{}", selection.provider, selection.model);
        if index > 0 {
            if !policy.enabled {
                break;
            }
            if selection.provider != route.selection.provider && !policy.allow_cross_provider {
                plan.skipped.push((label, "cross_provider_off"));
                continue;
            }
            if !router.provider_ready(user_id, selection) {
                plan.skipped.push((label, "not_ready"));
                continue;
            }
        }
        let pool = pool_for(store, user_id, &selection.provider)?;
        if pool.is_empty() {
            plan.candidates.push(Candidate {
                selection: ModelSelection {
                    account: None,
                    ..selection.clone()
                },
                thinking: thinking.clone(),
                hop: index,
            });
            continue;
        }
        let mut usable = Vec::new();
        for account in &pool {
            match account_block(account, &selection.provider, &selection.model, &holds, now) {
                None => usable.push(account.clone()),
                Some((reason, until)) => {
                    plan.skipped
                        .push((format!("{label}#{}", account.id), reason));
                    note_ready(&mut plan, until);
                }
            }
        }
        // The only route there is: never refuse to try it because of a hold
        // that a call on it would have set. A disabled or signed-out account
        // still cannot be used.
        if usable.is_empty() && single_hop {
            usable = pool
                .iter()
                .filter(|account| {
                    !matches!(
                        account.status,
                        AccountStatus::Disabled | AccountStatus::NeedsReauth
                    )
                })
                .cloned()
                .collect();
        }
        let pinned = selection
            .account
            .as_deref()
            .filter(|id| usable.iter().any(|account| account.id == *id));
        let affinity = store
            .kv_get(&affinity_key(user_id, conversation_id, &selection.provider))?
            .filter(|id| usable.iter().any(|account| account.id == *id));
        let ordered = order_pool(usable, pinned, affinity.as_deref());
        let take = if policy.switch_account_first {
            ordered.len()
        } else {
            ordered.len().min(1)
        };
        for account in ordered.into_iter().take(take) {
            plan.candidates.push(Candidate {
                selection: ModelSelection {
                    account: Some(account.id),
                    ..selection.clone()
                },
                thinking: thinking.clone(),
                hop: index,
            });
        }
    }
    let cap = if policy.enabled {
        usize::from(policy.max_switches) + 1
    } else {
        1
    };
    plan.candidates.truncate(cap);
    Ok(plan)
}

/// What a failure did to the account, for the audit trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cooldown {
    pub kind: &'static str,
    pub until: DateTime<Utc>,
}

fn clamp_hold(wait: Duration) -> chrono::Duration {
    chrono::Duration::from_std(wait.clamp(Duration::from_secs(1), MAX_HOLD))
        .unwrap_or_else(|_| chrono::Duration::seconds(60))
}

/// Record what a failed call means for its account: a hold, a cooldown on the
/// account row, or `needs_reauth`. Candidates with no concrete account (a
/// grant or a server key) leave no state. Returns the cooldown that was set.
pub fn record_failure(
    store: &Store,
    user_id: &str,
    candidate: &Candidate,
    kind: &FailKind,
    now: DateTime<Utc>,
) -> Result<Option<Cooldown>> {
    let Some(account) = candidate.selection.account.as_deref() else {
        return Ok(None);
    };
    let provider = candidate.selection.provider.as_str();
    let model = candidate.selection.model.as_str();
    let reason = reason_code(kind);
    let hold = |model: &str, hold_kind: &'static str, wait: chrono::Duration| -> Result<Cooldown> {
        let until = now + wait;
        store.set_provider_hold(user_id, provider, account, model, hold_kind, until, reason)?;
        Ok(Cooldown {
            kind: hold_kind,
            until,
        })
    };
    Ok(match kind {
        FailKind::RateLimited { retry_after } => {
            let cooldown = hold(
                ALL_MODELS,
                "rate_limit",
                clamp_hold(retry_after.unwrap_or(DEFAULT_RATE_HOLD)),
            )?;
            store.set_provider_account_status(
                user_id,
                provider,
                account,
                AccountStatus::RateLimited,
                Some(reason),
                Some(&to_string(cooldown.until)),
            )?;
            Some(cooldown)
        }
        FailKind::QuotaExhausted { resets_at } => {
            let wait = resets_at
                .map(|at| {
                    at.signed_duration_since(now)
                        .max(chrono::Duration::seconds(1))
                })
                .unwrap_or_else(|| clamp_hold(DEFAULT_QUOTA_HOLD))
                .min(chrono::Duration::seconds(MAX_HOLD.as_secs() as i64));
            let cooldown = hold(ALL_MODELS, "quota", wait)?;
            store.set_provider_account_status(
                user_id,
                provider,
                account,
                AccountStatus::QuotaExhausted,
                Some(reason),
                Some(&to_string(cooldown.until)),
            )?;
            Some(cooldown)
        }
        FailKind::ModelUnavailable => Some(hold(model, "model", clamp_hold(MODEL_HOLD))?),
        FailKind::Overloaded | FailKind::ProviderDown => {
            Some(hold(model, "overloaded", clamp_hold(OVERLOAD_HOLD))?)
        }
        FailKind::AccountForbidden => {
            Some(hold(ALL_MODELS, "forbidden", clamp_hold(FORBIDDEN_HOLD))?)
        }
        FailKind::CredentialsRejected => {
            // A refresh was already tried inside the client. The account needs
            // a new sign-in; the hold covers a pre-accounts credential, which
            // has no row to flag.
            store.set_provider_account_status(
                user_id,
                provider,
                account,
                AccountStatus::NeedsReauth,
                Some(reason),
                None,
            )?;
            Some(hold(ALL_MODELS, "credentials", clamp_hold(FORBIDDEN_HOLD))?)
        }
        FailKind::Transport | FailKind::ContextTooLong => None,
    })
}

/// Note a successful call: the account is healthy again, was just used, and
/// is the conversation's preferred account for this provider.
pub fn record_success(
    store: &Store,
    user_id: &str,
    conversation_id: &str,
    candidate: &Candidate,
) -> Result<()> {
    let Some(account) = candidate.selection.account.as_deref() else {
        return Ok(());
    };
    let provider = candidate.selection.provider.as_str();
    if let Some(row) = store.provider_account(user_id, provider, account)?
        && matches!(
            row.status,
            AccountStatus::RateLimited | AccountStatus::QuotaExhausted
        )
    {
        store.set_provider_account_status(
            user_id,
            provider,
            account,
            AccountStatus::Ok,
            None,
            None,
        )?;
    }
    store.touch_provider_account(user_id, provider, account)?;
    store.kv_set(&affinity_key(user_id, conversation_id, provider), account)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(text: &str) -> DateTime<Utc> {
        parse_time(text).unwrap()
    }

    #[test]
    fn a_fresh_zero_window_with_a_future_reset_holds_the_account() {
        let now = at("2026-11-16T10:00:00Z");
        let quota = json!({
            "sampled_at": "2026-11-16T09:55:00Z",
            "windows": [
                {"remaining": 0.4, "resets_at": "2026-11-16T12:00:00Z"},
                {"remaining": 0.0, "resets_at": "2026-11-16T11:00:00Z"}
            ]
        });
        assert_eq!(
            quota_held_until(&quota, now),
            Some(at("2026-11-16T11:00:00Z"))
        );
    }

    #[test]
    fn a_stale_or_reset_or_nonzero_snapshot_does_not_hold() {
        let now = at("2026-11-16T10:00:00Z");
        let stale = json!({
            "sampled_at": "2026-11-16T09:40:00Z",
            "windows": [{"remaining": 0.0, "resets_at": "2026-11-16T11:00:00Z"}]
        });
        assert_eq!(quota_held_until(&stale, now), None);
        let reset = json!({
            "sampled_at": "2026-11-16T09:59:00Z",
            "windows": [{"remaining": 0.0, "resets_at": "2026-11-16T09:59:30Z"}]
        });
        assert_eq!(quota_held_until(&reset, now), None);
        let some_left = json!({
            "sampled_at": "2026-11-16T09:59:00Z",
            "windows": [{"remaining": 0.01, "resets_at": "2026-11-16T11:00:00Z"}]
        });
        assert_eq!(quota_held_until(&some_left, now), None);
        assert_eq!(quota_held_until(&json!({"windows": []}), now), None);
    }

    fn account(id: &str, priority: i64, weight: i64, last: Option<&str>) -> PoolAccount {
        PoolAccount {
            id: id.into(),
            status: AccountStatus::Ok,
            priority,
            weight,
            cooldown_until: None,
            quota: None,
            last_used_at: last.map(str::to_owned),
        }
    }

    #[test]
    fn pool_order_is_pin_then_affinity_then_priority_weight_and_idle_first() {
        let pool = vec![
            account("a", 0, 1, Some("2026-11-16T09:00:00Z")),
            account("b", 5, 1, Some("2026-11-16T09:00:00Z")),
            account("c", 5, 3, Some("2026-11-16T09:30:00Z")),
            account("d", 5, 3, Some("2026-11-16T09:10:00Z")),
        ];
        let ids = |ordered: Vec<PoolAccount>| {
            ordered
                .into_iter()
                .map(|account| account.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(order_pool(pool.clone(), None, None)),
            ["d", "c", "b", "a"]
        );
        assert_eq!(
            ids(order_pool(pool.clone(), None, Some("a"))),
            ["a", "d", "c", "b"]
        );
        assert_eq!(
            ids(order_pool(pool, Some("b"), Some("a"))),
            ["b", "a", "d", "c"]
        );
    }

    #[test]
    fn blocked_accounts_explain_themselves() {
        let now = at("2026-11-16T10:00:00Z");
        let mut item = account("a", 0, 1, None);
        assert_eq!(account_block(&item, "p", "m", &[], now), None);
        item.status = AccountStatus::NeedsReauth;
        assert_eq!(
            account_block(&item, "p", "m", &[], now),
            Some(("needs_reauth", None))
        );
        item.status = AccountStatus::RateLimited;
        item.cooldown_until = Some(at("2026-11-16T10:00:30Z"));
        assert_eq!(
            account_block(&item, "p", "m", &[], now).map(|block| block.0),
            Some("cooling")
        );
        item.cooldown_until = Some(at("2026-11-16T09:59:00Z"));
        assert_eq!(account_block(&item, "p", "m", &[], now), None);
        let hold = Hold {
            provider: "p".into(),
            account_id: "a".into(),
            model: "m".into(),
            kind: "model".into(),
            until: at("2026-11-16T10:05:00Z"),
            reason: "model_unavailable".into(),
        };
        assert_eq!(
            account_block(&item, "p", "m", std::slice::from_ref(&hold), now).map(|block| block.0),
            Some("held")
        );
        // A hold on one model leaves the account's other models usable.
        assert_eq!(account_block(&item, "p", "other", &[hold], now), None);
    }
}
