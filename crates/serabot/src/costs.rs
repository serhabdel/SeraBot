//! Cost tracking (0.69).
//!
//! The owner keeps a price table (USD per million input and output tokens per
//! provider and model). A member's estimated cost is their `model.used`
//! tokens times those prices; models without a price are counted as
//! "unpriced" instead of guessed. Each member may set a monthly budget: the
//! alert sweep warns at 80% and again at 100% of it, once per month each.
//! Estimates only: the provider's own bill is the truth.
//!
//! 0.70: cache reads are priced apart (`cached_per_m`), and calls made
//! through a plan sign-in (ChatGPT/Codex, Copilot, Antigravity, Claude plan,
//! Muse, OpenCode) cost nothing per token: they are shown as "plan", with
//! what the same tokens would have cost at API prices, so a member can
//! compare their subscription with the API.

use chrono::{DateTime, Datelike, TimeZone, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SeraError};
use crate::store::Store;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelPrice {
    pub provider: String,
    pub model: String,
    /// USD per million input (prompt) tokens.
    pub input_per_m: f64,
    /// USD per million output (completion) tokens.
    pub output_per_m: f64,
    /// USD per million cached (cache-read) input tokens; `None` prices them
    /// as normal input.
    #[serde(default)]
    pub cached_per_m: Option<f64>,
}

impl ModelPrice {
    /// USD for these token counts at this price.
    pub fn usd(&self, prompt: i64, cached: i64, completion: i64) -> f64 {
        let million = 1_000_000.0;
        prompt.max(0) as f64 / million * self.input_per_m
            + cached.max(0) as f64 / million * self.cached_per_m.unwrap_or(self.input_per_m)
            + completion.max(0) as f64 / million * self.output_per_m
    }
}

/// Provider kinds that are plan (subscription) sign-ins, not pay-per-token.
pub const PLAN_KINDS: &[&str] = &[
    "openai-codex",
    "github-copilot",
    "google-antigravity",
    "muse-code",
    "opencode-zen",
    "opencode-go",
];

/// Whether a model call ran on a plan sign-in: a plan provider kind, or an
/// Anthropic account signed in with a Claude plan (OAuth) rather than a key.
pub fn is_plan_use(store: &Store, user_id: &str, kind: &str, account: &Option<String>) -> bool {
    if PLAN_KINDS.contains(&kind) {
        return true;
    }
    let Some(account) = account else {
        return false;
    };
    let Ok(connection) = store.connection() else {
        return false;
    };
    connection
        .query_row(
            "SELECT auth_kind FROM provider_accounts WHERE user_id = ?1 AND account_id = ?2 LIMIT 1",
            params![user_id, account],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .is_some_and(|auth| auth != "api_key")
}

/// What a member pays per month for one plan sign-in.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PlanPrice {
    pub provider: String,
    pub monthly_usd: f64,
}

/// One plan compared with the API (0.70).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct PlanSaving {
    pub provider: String,
    /// What the member entered for the plan; `None` when not set.
    pub monthly_usd: Option<f64>,
    /// What this period's plan tokens would cost at API prices.
    pub api_equivalent_usd: f64,
    /// Tokens on this plan that have no API price to compare with.
    pub unpriced_tokens: i64,
    /// `api_equivalent_usd - monthly_usd` (negative: the plan cost more).
    pub saving_usd: Option<f64>,
}

/// Token totals in millions and their cost.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Totals {
    pub input_m: f64,
    pub cached_m: f64,
    pub output_m: f64,
    /// Pay-per-token cost (plans excluded).
    pub usd: f64,
    /// What the plan tokens would have cost at API prices.
    pub plan_api_equivalent_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct CostLine {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// Cache-read input tokens (0.70; 0 for calls recorded earlier).
    pub cached_tokens: i64,
    /// `None` when the model has no price, or ran on a plan.
    pub usd: Option<f64>,
    /// Ran on a plan sign-in: no per-token cost.
    pub plan: bool,
    /// For plan rows: the same tokens at API prices, when a price is known.
    pub api_equivalent_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct CostReport {
    /// Pay-per-token cost (plans excluded).
    pub usd: f64,
    pub unpriced_tokens: i64,
    pub lines: Vec<CostLine>,
    pub totals: Totals,
}

/// The first instant of the month `now` is in (UTC).
pub fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

fn valid_price(price: &ModelPrice) -> Result<()> {
    let name_ok = |value: &str| {
        !value.trim().is_empty()
            && value.len() <= 120
            && !value.chars().any(|character| character.is_control())
    };
    if !name_ok(&price.provider) || !name_ok(&price.model) {
        return Err(SeraError::Validation(
            "provider and model must be 1-120 plain characters".into(),
        ));
    }
    for value in [price.input_per_m, price.output_per_m]
        .into_iter()
        .chain(price.cached_per_m)
    {
        if !value.is_finite() || !(0.0..=10_000.0).contains(&value) {
            return Err(SeraError::Validation(
                "prices must be 0-10000 USD per million tokens".into(),
            ));
        }
    }
    Ok(())
}

impl Store {
    pub fn model_prices(&self) -> Result<Vec<ModelPrice>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT provider, model, input_per_m, output_per_m, cached_per_m FROM model_prices
             ORDER BY provider, model",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ModelPrice {
                provider: row.get(0)?,
                model: row.get(1)?,
                input_per_m: row.get(2)?,
                output_per_m: row.get(3)?,
                cached_per_m: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Replace the whole price table (owner only; checked by the caller).
    pub fn set_model_prices(&self, prices: &[ModelPrice]) -> Result<()> {
        if prices.len() > 500 {
            return Err(SeraError::Validation("at most 500 prices".into()));
        }
        for price in prices {
            valid_price(price)?;
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        tx.execute("DELETE FROM model_prices", [])?;
        let now = Utc::now().to_rfc3339();
        for price in prices {
            tx.execute(
                "INSERT OR REPLACE INTO model_prices(provider, model, input_per_m, output_per_m, cached_per_m, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    price.provider.trim(),
                    price.model.trim(),
                    price.input_per_m,
                    price.output_per_m,
                    price.cached_per_m,
                    now
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn monthly_budget(&self, user_id: &str) -> Result<Option<f64>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT monthly_usd FROM usage_budgets WHERE user_id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Set (or with `None`, clear) the member's monthly budget.
    pub fn set_monthly_budget(&self, user_id: &str, usd: Option<f64>) -> Result<()> {
        let connection = self.connection()?;
        match usd {
            Some(value) if value.is_finite() && (0.0..=1_000_000.0).contains(&value) => {
                connection.execute(
                    "INSERT INTO usage_budgets(user_id, monthly_usd, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(user_id) DO UPDATE SET monthly_usd = excluded.monthly_usd,
                        updated_at = excluded.updated_at",
                    params![user_id, value, Utc::now().to_rfc3339()],
                )?;
            }
            Some(_) => {
                return Err(SeraError::Validation(
                    "the budget must be 0-1000000 USD".into(),
                ));
            }
            None => {
                connection.execute(
                    "DELETE FROM usage_budgets WHERE user_id = ?1",
                    params![user_id],
                )?;
            }
        }
        Ok(())
    }

    /// Whether the member gets the weekly digest (default on).
    pub fn weekly_digest(&self, user_id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let value: Option<i64> = connection
            .query_row(
                "SELECT weekly_digest FROM alert_settings WHERE user_id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.is_none_or(|value| value != 0))
    }

    pub fn set_weekly_digest(&self, user_id: &str, on: bool) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO alert_settings(user_id, telegram, whatsapp_conversation_id, updated_at, weekly_digest)
             VALUES (?1, 1, NULL, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET weekly_digest = excluded.weekly_digest,
                updated_at = excluded.updated_at",
            params![user_id, Utc::now().to_rfc3339(), i64::from(on)],
        )?;
        Ok(())
    }

    /// What the member pays for each plan sign-in.
    pub fn plan_prices(&self, user_id: &str) -> Result<Vec<PlanPrice>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT provider, monthly_usd FROM plan_prices WHERE user_id = ?1 ORDER BY provider",
        )?;
        let rows = statement.query_map(params![user_id], |row| {
            Ok(PlanPrice {
                provider: row.get(0)?,
                monthly_usd: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Replace the member's plan prices.
    pub fn set_plan_prices(&self, user_id: &str, plans: &[PlanPrice]) -> Result<()> {
        if plans.len() > 20 {
            return Err(SeraError::Validation("at most 20 plans".into()));
        }
        for plan in plans {
            if plan.provider.trim().is_empty()
                || plan.provider.len() > 60
                || plan.provider.chars().any(char::is_control)
                || !plan.monthly_usd.is_finite()
                || !(0.0..=10_000.0).contains(&plan.monthly_usd)
            {
                return Err(SeraError::Validation(
                    "a plan needs a provider and a monthly price of 0-10000 USD".into(),
                ));
            }
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        tx.execute(
            "DELETE FROM plan_prices WHERE user_id = ?1",
            params![user_id],
        )?;
        let now = Utc::now().to_rfc3339();
        for plan in plans {
            tx.execute(
                "INSERT OR REPLACE INTO plan_prices(user_id, provider, monthly_usd, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![user_id, plan.provider.trim(), plan.monthly_usd, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Estimated cost of `user_id`'s model calls since `since` (`None` user:
    /// every member, admin only).
    pub fn cost_report(&self, user_id: Option<&str>, since: DateTime<Utc>) -> Result<CostReport> {
        let prices = self.model_prices()?;
        let rows: Vec<(String, String, String, bool, i64, i64, i64)> = {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT COALESCE(json_extract(details_json, '$.provider'), ''),
                        COALESCE(json_extract(details_json, '$.model'), ''),
                        COALESCE(json_extract(details_json, '$.kind'), json_extract(details_json, '$.provider'), ''),
                        COALESCE(json_extract(details_json, '$.plan'), 0),
                        COALESCE(SUM(CAST(json_extract(details_json, '$.usage.prompt_tokens') AS INTEGER)), 0),
                        COALESCE(SUM(CAST(json_extract(details_json, '$.usage.cached_tokens') AS INTEGER)), 0),
                        COALESCE(SUM(CAST(json_extract(details_json, '$.usage.completion_tokens') AS INTEGER)), 0)
                 FROM audit_log
                 WHERE event = 'model.used' AND at >= ?1 AND (?2 IS NULL OR actor = ?2)
                 GROUP BY 1, 2, 3, 4 ORDER BY 5 DESC",
            )?;
            let mapped = statement.query_map(params![since.to_rfc3339(), user_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut report = CostReport::default();
        for (provider, model, kind, plan_flag, prompt, cached, completion) in rows {
            let plan = plan_flag || PLAN_KINDS.contains(&kind.as_str());
            report.totals.input_m += prompt.max(0) as f64 / 1_000_000.0;
            report.totals.cached_m += cached.max(0) as f64 / 1_000_000.0;
            report.totals.output_m += completion.max(0) as f64 / 1_000_000.0;
            let tokens = prompt.max(0) + cached.max(0) + completion.max(0);
            let (usd, api_equivalent_usd) = if plan {
                let equivalent = find_price(&prices, &kind, &model)
                    .or_else(|| find_price(&prices, &provider, &model))
                    .or_else(|| find_price_any_provider(&prices, &model))
                    .map(|price| price.usd(prompt, cached, completion));
                if let Some(value) = equivalent {
                    report.totals.plan_api_equivalent_usd += value;
                }
                (None, equivalent)
            } else {
                let price = find_price(&prices, &provider, &model)
                    .or_else(|| find_price(&prices, &kind, &model));
                let usd = price.map(|price| price.usd(prompt, cached, completion));
                match usd {
                    Some(value) => report.usd += value,
                    None => report.unpriced_tokens += tokens,
                }
                (usd, None)
            };
            report.lines.push(CostLine {
                provider: if kind.is_empty() { provider } else { kind },
                model,
                prompt_tokens: prompt,
                completion_tokens: completion,
                cached_tokens: cached,
                usd,
                plan,
                api_equivalent_usd,
            });
        }
        report.totals.usd = report.usd;
        Ok(report)
    }

    /// Plans compared with the API for `user_id` over `report`'s period.
    pub fn plan_savings(&self, user_id: &str, report: &CostReport) -> Result<Vec<PlanSaving>> {
        let prices = self.plan_prices(user_id)?;
        let mut out: Vec<PlanSaving> = Vec::new();
        for line in report.lines.iter().filter(|line| line.plan) {
            let entry = match out.iter_mut().find(|entry| entry.provider == line.provider) {
                Some(entry) => entry,
                None => {
                    out.push(PlanSaving {
                        provider: line.provider.clone(),
                        ..PlanSaving::default()
                    });
                    out.last_mut().expect("just pushed")
                }
            };
            match line.api_equivalent_usd {
                Some(value) => entry.api_equivalent_usd += value,
                None => {
                    entry.unpriced_tokens +=
                        line.prompt_tokens + line.cached_tokens + line.completion_tokens
                }
            }
        }
        for plan in &prices {
            if !out.iter().any(|entry| entry.provider == plan.provider) {
                out.push(PlanSaving {
                    provider: plan.provider.clone(),
                    ..PlanSaving::default()
                });
            }
        }
        for entry in &mut out {
            entry.monthly_usd = prices
                .iter()
                .find(|plan| plan.provider == entry.provider)
                .map(|plan| plan.monthly_usd);
            entry.saving_usd = entry
                .monthly_usd
                .map(|monthly| entry.api_equivalent_usd - monthly);
        }
        Ok(out)
    }
}

/// A price for `model` under any provider (plan sign-ins serve other
/// providers' models, for example Claude or Gemini through Antigravity).
/// A `vendor/model` id also matches `model`.
pub fn find_price_any_provider<'a>(
    prices: &'a [ModelPrice],
    model: &str,
) -> Option<&'a ModelPrice> {
    let bare = model.rsplit('/').next().unwrap_or(model);
    prices
        .iter()
        .find(|price| price.model.eq_ignore_ascii_case(model))
        .or_else(|| {
            prices.iter().find(|price| {
                price
                    .model
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .eq_ignore_ascii_case(bare)
            })
        })
}

/// One suggested price with where it came from (0.70). Researched from
/// public pages and NOT verified: the owner checks before saving.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SuggestedPrice {
    pub provider: String,
    pub model: String,
    pub input_per_m: f64,
    #[serde(default)]
    pub cached_input_per_m: Option<f64>,
    pub output_per_m: f64,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Deserialize)]
struct SuggestedFile {
    researched_at: String,
    prices: Vec<serde_json::Value>,
}

/// The shipped suggestion list: only rows with both prices.
pub fn suggested_prices() -> (String, Vec<SuggestedPrice>) {
    let Ok(file) = serde_json::from_str::<SuggestedFile>(include_str!(
        "../../../config/suggested_prices.json"
    )) else {
        return (String::new(), Vec::new());
    };
    let rows = file
        .prices
        .into_iter()
        .filter_map(|row| serde_json::from_value::<SuggestedPrice>(row).ok())
        .filter(|row| row.input_per_m.is_finite() && row.output_per_m.is_finite())
        .collect();
    (file.researched_at, rows)
}

/// The price for a provider and model: an exact match first, then a row
/// whose model is `*` for that provider (a provider-wide price).
pub fn find_price<'a>(
    prices: &'a [ModelPrice],
    provider: &str,
    model: &str,
) -> Option<&'a ModelPrice> {
    prices
        .iter()
        .find(|price| {
            price.provider.eq_ignore_ascii_case(provider) && price.model.eq_ignore_ascii_case(model)
        })
        .or_else(|| {
            prices
                .iter()
                .find(|price| price.provider.eq_ignore_ascii_case(provider) && price.model == "*")
        })
}

/// Which budget threshold (80 or 100) this month's spend has crossed, if any.
pub fn budget_level(spent: f64, budget: f64) -> Option<u8> {
    if budget <= 0.0 {
        return None;
    }
    if spent >= budget {
        Some(100)
    } else if spent >= budget * 0.8 {
        Some(80)
    } else {
        None
    }
}

/// Tokens as millions with two decimals ("1.25M"); small counts keep three.
pub fn millions(tokens: f64) -> String {
    if tokens < 0.01 && tokens > 0.0 {
        format!("{tokens:.3}M")
    } else {
        format!("{tokens:.2}M")
    }
}

/// "$1.23" style, with more digits for small amounts.
pub fn usd(value: f64) -> String {
    if value < 0.1 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn used(store: &Store, user: &str, model: &str, prompt: i64, completion: i64) {
        store
            .record_audit(
                user,
                "model.used",
                None,
                None,
                None,
                None,
                None,
                Some(&json!({"provider": "p", "model": model,
                    "usage": {"prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion}})),
            )
            .unwrap();
    }

    #[test]
    fn cache_reads_and_plans_are_priced_apart() {
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        // 1M uncached + 1M cached input, 1M output on a paid API model.
        store
            .record_audit(
                &admin,
                "model.used",
                None,
                None,
                None,
                None,
                None,
                Some(&json!({"provider": "default", "kind": "anthropic", "plan": false, "model": "claude-x",
                    "usage": {"prompt_tokens": 1_000_000, "cached_tokens": 1_000_000, "completion_tokens": 1_000_000}})),
            )
            .unwrap();
        // The same model through a plan sign-in (Antigravity serves Claude).
        store
            .record_audit(
                &admin,
                "model.used",
                None,
                None,
                None,
                None,
                None,
                Some(&json!({"provider": "ag", "kind": "google-antigravity", "plan": true, "model": "claude-x",
                    "usage": {"prompt_tokens": 2_000_000, "cached_tokens": 0, "completion_tokens": 0}})),
            )
            .unwrap();
        store
            .set_model_prices(&[ModelPrice {
                provider: "anthropic".into(),
                model: "claude-x".into(),
                input_per_m: 3.0,
                output_per_m: 15.0,
                cached_per_m: Some(0.3),
            }])
            .unwrap();
        let since = Utc::now() - chrono::Duration::days(1);
        let report = store.cost_report(Some(&admin), since).unwrap();
        // 3 + 0.3 + 15 for the paid call; the plan call costs nothing.
        assert!((report.usd - 18.3).abs() < 1e-9, "{report:?}");
        let plan = report.lines.iter().find(|line| line.plan).unwrap();
        assert_eq!(plan.usd, None);
        assert!((plan.api_equivalent_usd.unwrap() - 6.0).abs() < 1e-9);
        assert!((report.totals.input_m - 3.0).abs() < 1e-9);
        assert!((report.totals.cached_m - 1.0).abs() < 1e-9);
        assert!((report.totals.plan_api_equivalent_usd - 6.0).abs() < 1e-9);
        store
            .set_plan_prices(
                &admin,
                &[PlanPrice {
                    provider: "google-antigravity".into(),
                    monthly_usd: 20.0,
                }],
            )
            .unwrap();
        let savings = store.plan_savings(&admin, &report).unwrap();
        assert_eq!(savings.len(), 1);
        assert_eq!(savings[0].monthly_usd, Some(20.0));
        assert!((savings[0].saving_usd.unwrap() + 14.0).abs() < 1e-9, "{savings:?}");
        assert!(store
            .set_plan_prices(
                &admin,
                &[PlanPrice {
                    provider: "x".into(),
                    monthly_usd: -1.0,
                }],
            )
            .is_err());
    }

    #[test]
    fn suggested_prices_ship_and_parse() {
        let (researched, rows) = suggested_prices();
        assert!(!researched.is_empty());
        assert!(rows.len() >= 10);
        assert!(rows.iter().all(|row| row.input_per_m >= 0.0 && row.output_per_m >= 0.0));
        assert_eq!(millions(1_250_000.0 / 1_000_000.0), "1.25M");
    }

    #[test]
    fn costs_use_the_price_table_and_count_unpriced_tokens() {
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        used(&store, &admin, "big", 1_000_000, 500_000);
        used(&store, &admin, "other", 10, 20);
        store
            .set_model_prices(&[ModelPrice {
                provider: "p".into(),
                model: "big".into(),
                input_per_m: 3.0,
                output_per_m: 15.0,
                cached_per_m: None,
            }])
            .unwrap();
        let report = store
            .cost_report(Some(&admin), Utc::now() - chrono::Duration::days(1))
            .unwrap();
        assert!((report.usd - 10.5).abs() < 1e-9, "{report:?}");
        assert_eq!(report.unpriced_tokens, 30);
        // A provider-wide price covers the rest.
        store
            .set_model_prices(&[ModelPrice {
                provider: "p".into(),
                model: "*".into(),
                input_per_m: 1.0,
                output_per_m: 1.0,
                cached_per_m: None,
            }])
            .unwrap();
        let report = store
            .cost_report(Some(&admin), Utc::now() - chrono::Duration::days(1))
            .unwrap();
        assert_eq!(report.unpriced_tokens, 0);
        let member = store
            .create_user("subject-cost", "c@example.example", "C")
            .unwrap()
            .id;
        assert_eq!(
            store
                .cost_report(Some(&member), Utc::now() - chrono::Duration::days(1))
                .unwrap()
                .lines
                .len(),
            0
        );
    }

    #[test]
    fn prices_and_budgets_are_validated() {
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        assert!(
            store
                .set_model_prices(&[ModelPrice {
                    provider: "p".into(),
                    model: "m".into(),
                    input_per_m: -1.0,
                    output_per_m: 1.0,
                    cached_per_m: None,
                }])
                .is_err()
        );
        assert!(store.set_monthly_budget(&admin, Some(f64::NAN)).is_err());
        store.set_monthly_budget(&admin, Some(20.0)).unwrap();
        assert_eq!(store.monthly_budget(&admin).unwrap(), Some(20.0));
        store.set_monthly_budget(&admin, None).unwrap();
        assert_eq!(store.monthly_budget(&admin).unwrap(), None);
        assert!(store.weekly_digest(&admin).unwrap());
        store.set_weekly_digest(&admin, false).unwrap();
        assert!(!store.weekly_digest(&admin).unwrap());
    }

    #[test]
    fn budget_levels_and_month_start() {
        assert_eq!(budget_level(5.0, 10.0), None);
        assert_eq!(budget_level(8.0, 10.0), Some(80));
        assert_eq!(budget_level(12.0, 10.0), Some(100));
        assert_eq!(budget_level(1.0, 0.0), None);
        let now = Utc.with_ymd_and_hms(2026, 11, 20, 15, 4, 5).unwrap();
        assert_eq!(
            month_start(now),
            Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap()
        );
        assert_eq!(usd(0.01234), "$0.0123");
        assert_eq!(usd(12.345), "$12.35");
    }
}
