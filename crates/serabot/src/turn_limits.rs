//! Per-turn tool limits (0.24): how many tool calls and model rounds one bot
//! turn may use.
//!
//! The config sets the defaults. An owner can override both in Settings, and
//! the override is stored per user. Resolution order: the owner's override
//! when one exists, otherwise the config default. The result is clamped to
//! `[1, ceiling]`, and a value outside that range is refused when written.
//! Every store method takes the acting user's id.

use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Config;
use crate::error::{Result, SeraError};
use crate::store::Store;

pub const DEFAULT_TOOL_CALLS_PER_TURN: usize = 1500;
pub const DEFAULT_TOOL_ROUNDS_PER_TURN: usize = 200;
/// Hard server ceilings. No configuration or owner override goes above them.
pub const TOOL_CALLS_CEILING: usize = 1500;
pub const TOOL_ROUNDS_CEILING: usize = 1500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnLimits {
    pub max_tool_calls_per_turn: usize,
    pub max_tool_rounds: usize,
}

/// The limits the config alone gives. Model rounds take the higher of
/// `runtime.max_tool_iterations` and `llm.max_tool_rounds`, as the turn loop
/// always has.
pub fn config_defaults(config: &Config) -> TurnLimits {
    TurnLimits {
        max_tool_calls_per_turn: config.runtime.max_tool_calls_per_turn,
        max_tool_rounds: config
            .runtime
            .max_tool_iterations
            .max(config.llm.max_tool_rounds),
    }
}

/// The limits a turn runs under: the owner's override when set, otherwise the
/// config default, each clamped to `[1, ceiling]`.
pub fn resolve_turn_limits(default: TurnLimits, owner: Option<TurnLimits>) -> TurnLimits {
    let chosen = owner.unwrap_or(default);
    TurnLimits {
        max_tool_calls_per_turn: chosen.max_tool_calls_per_turn.clamp(1, TOOL_CALLS_CEILING),
        max_tool_rounds: chosen.max_tool_rounds.clamp(1, TOOL_ROUNDS_CEILING),
    }
}

/// Reject a write that is zero or above the ceiling.
pub fn validate_turn_limits(limits: TurnLimits) -> Result<TurnLimits> {
    if !(1..=TOOL_CALLS_CEILING).contains(&limits.max_tool_calls_per_turn) {
        return Err(SeraError::Validation(format!(
            "tool calls per turn must be between 1 and {TOOL_CALLS_CEILING}"
        )));
    }
    if !(1..=TOOL_ROUNDS_CEILING).contains(&limits.max_tool_rounds) {
        return Err(SeraError::Validation(format!(
            "tool rounds per turn must be between 1 and {TOOL_ROUNDS_CEILING}"
        )));
    }
    Ok(limits)
}

/// The owner-facing view: what applies now, the config default, the ceiling,
/// and whether an override is set.
pub fn turn_limits_view(defaults: TurnLimits, owner: Option<TurnLimits>) -> Value {
    let effective = resolve_turn_limits(defaults, owner);
    json!({
        "max_tool_calls_per_turn": effective.max_tool_calls_per_turn,
        "max_tool_rounds": effective.max_tool_rounds,
        "default": {
            "max_tool_calls_per_turn": defaults.max_tool_calls_per_turn,
            "max_tool_rounds": defaults.max_tool_rounds,
        },
        "ceiling": {
            "max_tool_calls_per_turn": TOOL_CALLS_CEILING,
            "max_tool_rounds": TOOL_ROUNDS_CEILING,
        },
        "overridden": owner.is_some(),
    })
}

impl Store {
    /// The acting user's override, when one is set.
    pub fn get_user_turn_limits(&self, user_id: &str) -> Result<Option<TurnLimits>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT max_tool_calls_per_turn, max_tool_rounds FROM user_turn_limits
                 WHERE user_id = ?1",
                params![user_id],
                |row| {
                    Ok(TurnLimits {
                        max_tool_calls_per_turn: row.get::<_, i64>(0)?.max(0) as usize,
                        max_tool_rounds: row.get::<_, i64>(1)?.max(0) as usize,
                    })
                },
            )
            .optional()?)
    }

    /// Set the acting user's override. The limits are validated first.
    pub fn set_user_turn_limits(&self, user_id: &str, limits: TurnLimits) -> Result<()> {
        let limits = validate_turn_limits(limits)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO user_turn_limits(user_id, max_tool_calls_per_turn, max_tool_rounds, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO UPDATE SET
                 max_tool_calls_per_turn = excluded.max_tool_calls_per_turn,
                 max_tool_rounds = excluded.max_tool_rounds,
                 updated_at = excluded.updated_at",
            params![
                user_id,
                limits.max_tool_calls_per_turn as i64,
                limits.max_tool_rounds as i64,
                Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            ],
        )?;
        Ok(())
    }

    /// Remove the acting user's override, so the config defaults apply again.
    pub fn clear_user_turn_limits(&self, user_id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let removed = connection.execute(
            "DELETE FROM user_turn_limits WHERE user_id = ?1",
            params![user_id],
        )?;
        Ok(removed == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> TurnLimits {
        TurnLimits {
            max_tool_calls_per_turn: DEFAULT_TOOL_CALLS_PER_TURN,
            max_tool_rounds: DEFAULT_TOOL_ROUNDS_PER_TURN,
        }
    }

    #[test]
    fn owner_override_wins_over_config_default() {
        let owner = TurnLimits {
            max_tool_calls_per_turn: 40,
            max_tool_rounds: 12,
        };
        assert_eq!(resolve_turn_limits(defaults(), Some(owner)), owner);
        assert_eq!(resolve_turn_limits(defaults(), None), defaults());
    }

    #[test]
    fn resolved_values_are_clamped_to_the_ceiling() {
        let huge = TurnLimits {
            max_tool_calls_per_turn: 10_000,
            max_tool_rounds: 10_000,
        };
        let clamped = resolve_turn_limits(huge, None);
        assert_eq!(clamped.max_tool_calls_per_turn, TOOL_CALLS_CEILING);
        assert_eq!(clamped.max_tool_rounds, TOOL_ROUNDS_CEILING);
        let zero = TurnLimits {
            max_tool_calls_per_turn: 0,
            max_tool_rounds: 0,
        };
        let floored = resolve_turn_limits(zero, Some(zero));
        assert_eq!(floored.max_tool_calls_per_turn, 1);
        assert_eq!(floored.max_tool_rounds, 1);
    }

    #[test]
    fn writes_outside_the_range_are_refused() {
        let ok = TurnLimits {
            max_tool_calls_per_turn: TOOL_CALLS_CEILING,
            max_tool_rounds: TOOL_ROUNDS_CEILING,
        };
        assert!(validate_turn_limits(ok).is_ok());
        for bad in [
            TurnLimits {
                max_tool_calls_per_turn: 0,
                max_tool_rounds: 5,
            },
            TurnLimits {
                max_tool_calls_per_turn: TOOL_CALLS_CEILING + 1,
                max_tool_rounds: 5,
            },
            TurnLimits {
                max_tool_calls_per_turn: 5,
                max_tool_rounds: TOOL_ROUNDS_CEILING + 1,
            },
        ] {
            assert!(matches!(
                validate_turn_limits(bad),
                Err(SeraError::Validation(_))
            ));
        }
    }

    #[test]
    fn config_defaults_take_the_higher_round_setting() {
        let mut config = Config::default();
        config.runtime.max_tool_iterations = 7;
        config.llm.max_tool_rounds = 9;
        config.runtime.max_tool_calls_per_turn = 33;
        let limits = config_defaults(&config);
        assert_eq!(limits.max_tool_rounds, 9);
        assert_eq!(limits.max_tool_calls_per_turn, 33);
    }

    #[test]
    fn view_reports_effective_default_and_ceiling() {
        let owner = TurnLimits {
            max_tool_calls_per_turn: 20,
            max_tool_rounds: 10,
        };
        let view = turn_limits_view(defaults(), Some(owner));
        assert_eq!(view["max_tool_calls_per_turn"], 20);
        assert_eq!(view["default"]["max_tool_calls_per_turn"], 1500);
        assert_eq!(view["ceiling"]["max_tool_rounds"], 1500);
        assert_eq!(view["overridden"], true);
        assert_eq!(turn_limits_view(defaults(), None)["overridden"], false);
    }
}
