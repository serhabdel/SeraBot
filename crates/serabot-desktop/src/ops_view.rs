//! Alerts and backups (0.68) for the console: blocking calls on a fresh
//! client, run on a short worker thread by the UI (same pattern as the relay
//! card), plus small helpers the UI and tests share.

use serde_json::Value;

use crate::api::ApiClient;

fn client(credentials: &crate::worker::Credentials) -> Result<ApiClient, String> {
    ApiClient::new(credentials.endpoint.clone(), credentials.token.clone())
        .map_err(|error| error.to_string())
}

pub fn get_alerts(credentials: &crate::worker::Credentials) -> Result<Value, String> {
    client(credentials)?
        .alerts_get()
        .map_err(|error| error.to_string())
}

pub fn put_alerts(
    credentials: &crate::worker::Credentials,
    telegram: bool,
    whatsapp: Option<String>,
) -> Result<Value, String> {
    client(credentials)?
        .alerts_put(telegram, whatsapp.as_deref())
        .map_err(|error| error.to_string())
}

pub fn update_alerts(
    credentials: &crate::worker::Credentials,
    body: Value,
) -> Result<Value, String> {
    client(credentials)?
        .alerts_update(&body)
        .map_err(|error| error.to_string())
}

pub fn digest_now(credentials: &crate::worker::Credentials) -> Result<Value, String> {
    client(credentials)?
        .digest_now()
        .map_err(|error| error.to_string())
}

pub fn get_costs(credentials: &crate::worker::Credentials) -> Result<Value, String> {
    client(credentials)?
        .costs_get()
        .map_err(|error| error.to_string())
}

pub fn put_budget(
    credentials: &crate::worker::Credentials,
    monthly_usd: Option<f64>,
) -> Result<Value, String> {
    client(credentials)?
        .budget_put(monthly_usd)
        .map_err(|error| error.to_string())
}

pub fn put_prices(
    credentials: &crate::worker::Credentials,
    prices: Value,
) -> Result<Value, String> {
    client(credentials)?
        .prices_put(&prices)
        .map_err(|error| error.to_string())
}

/// Parse a budget field: empty clears, otherwise a non-negative number
/// (a comma works as the decimal mark).
pub fn parse_budget(text: &str) -> Result<Option<f64>, String> {
    let text = text.trim().trim_start_matches('$').replace(',', ".");
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<f64>() {
        Ok(value) if value.is_finite() && value >= 0.0 => Ok(Some(value)),
        _ => Err("Enter a number of dollars, for example 20".into()),
    }
}

/// One price row typed in the console: `provider model input output`.
pub fn parse_price_line(line: &str) -> Result<Value, String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() != 4 {
        return Err(format!(
            "\"{line}\": write provider, model, input and output price per million tokens"
        ));
    }
    let number = |text: &str| {
        text.trim_start_matches('$')
            .replace(',', ".")
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
    };
    let (Some(input), Some(output)) = (number(parts[2]), number(parts[3])) else {
        return Err(format!("\"{line}\": the prices must be numbers"));
    };
    Ok(serde_json::json!({
        "provider": parts[0], "model": parts[1],
        "input_per_m": input, "output_per_m": output,
    }))
}

/// "$1.23" style, with more digits below ten cents.
pub fn usd(value: f64) -> String {
    if value < 0.1 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

pub fn get_backups(credentials: &crate::worker::Credentials) -> Result<Value, String> {
    client(credentials)?
        .backups_get()
        .map_err(|error| error.to_string())
}

pub fn run_backup(credentials: &crate::worker::Credentials) -> Result<Value, String> {
    client(credentials)?
        .backups_run()
        .map_err(|error| error.to_string())
}

/// Readable label for an alert kind.
pub fn alert_kind_label(kind: &str) -> &'static str {
    match kind {
        "failover" => "Model failing over",
        "signin" => "Sign-in expired",
        "tunnel" => "PC tunnel dropped",
        "disk" => "Disk nearly full",
        "memory" => "Memory high",
        "backup" => "Backup failed",
        "budget" => "Budget",
        "digest" => "Weekly digest",
        _ => "Alert",
    }
}

/// "uploaded" / "off" / a reason, as a short line.
pub fn onedrive_line(onedrive: &str) -> String {
    match onedrive {
        "uploaded" => "Copied to OneDrive".into(),
        "off" | "" => "OneDrive copy off".into(),
        other => format!("OneDrive: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_and_price_lines_parse() {
        assert_eq!(parse_budget(""), Ok(None));
        assert_eq!(parse_budget("$20,5"), Ok(Some(20.5)));
        assert!(parse_budget("-3").is_err());
        let price = parse_price_line("anthropic claude-x 3 15").unwrap();
        assert_eq!(price["output_per_m"], 15.0);
        assert!(parse_price_line("anthropic 3 15").is_err());
        assert!(parse_price_line("a b x 1").is_err());
        assert_eq!(usd(0.5), "$0.50");
    }

    #[test]
    fn labels_read_well() {
        assert_eq!(alert_kind_label("signin"), "Sign-in expired");
        assert_eq!(alert_kind_label("zzz"), "Alert");
        assert_eq!(onedrive_line("uploaded"), "Copied to OneDrive");
        assert_eq!(onedrive_line(""), "OneDrive copy off");
        assert!(onedrive_line("failed: 401").contains("401"));
    }
}
