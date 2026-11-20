//! Cost tracking (0.69).
//!
//! The owner keeps a price table (USD per million input and output tokens per
//! provider and model). A member's estimated cost is their `model.used`
//! tokens times those prices; models without a price are counted as
//! "unpriced" instead of guessed. Each member may set a monthly budget: the
//! alert sweep warns at 80% and again at 100% of it, once per month each.
//! Estimates only: the provider's own bill is the truth.

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
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct CostLine {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// `None` when the model has no price in the table.
    pub usd: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct CostReport {
    pub usd: f64,
    pub unpriced_tokens: i64,
    pub lines: Vec<CostLine>,
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
    for value in [price.input_per_m, price.output_per_m] {
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
            "SELECT provider, model, input_per_m, output_per_m FROM model_prices
             ORDER BY provider, model",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ModelPrice {
                provider: row.get(0)?,
                model: row.get(1)?,
                input_per_m: row.get(2)?,
                output_per_m: row.get(3)?,
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
                "INSERT OR REPLACE INTO model_prices(provider, model, input_per_m, output_per_m, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    price.provider.trim(),
                    price.model.trim(),
                    price.input_per_m,
                    price.output_per_m,
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

    /// Estimated cost of `user_id`'s model calls since `since` (`None` user:
    /// every member, admin only).
    pub fn cost_report(&self, user_id: Option<&str>, since: DateTime<Utc>) -> Result<CostReport> {
        let prices = self.model_prices()?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT COALESCE(json_extract(details_json, '$.provider'), ''),
                    COALESCE(json_extract(details_json, '$.model'), ''),
                    COALESCE(SUM(CAST(json_extract(details_json, '$.usage.prompt_tokens') AS INTEGER)), 0),
                    COALESCE(SUM(CAST(json_extract(details_json, '$.usage.completion_tokens') AS INTEGER)), 0)
             FROM audit_log
             WHERE event = 'model.used' AND at >= ?1 AND (?2 IS NULL OR actor = ?2)
             GROUP BY 1, 2 ORDER BY 3 DESC",
        )?;
        let rows = statement.query_map(params![since.to_rfc3339(), user_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut report = CostReport::default();
        for row in rows {
            let (provider, model, prompt, completion) = row?;
            let price = find_price(&prices, &provider, &model);
            let usd = price.map(|price| {
                prompt.max(0) as f64 / 1_000_000.0 * price.input_per_m
                    + completion.max(0) as f64 / 1_000_000.0 * price.output_per_m
            });
            match usd {
                Some(value) => report.usd += value,
                None => report.unpriced_tokens += prompt.max(0) + completion.max(0),
            }
            report.lines.push(CostLine {
                provider,
                model,
                prompt_tokens: prompt,
                completion_tokens: completion,
                usd,
            });
        }
        Ok(report)
    }
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
