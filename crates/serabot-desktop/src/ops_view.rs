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
    fn labels_read_well() {
        assert_eq!(alert_kind_label("signin"), "Sign-in expired");
        assert_eq!(alert_kind_label("zzz"), "Alert");
        assert_eq!(onedrive_line("uploaded"), "Copied to OneDrive");
        assert_eq!(onedrive_line(""), "OneDrive copy off");
        assert!(onedrive_line("failed: 401").contains("401"));
    }
}
