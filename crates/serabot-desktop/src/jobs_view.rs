//! Job search card (0.70) for the console: blocking calls on a fresh client
//! (run on a short worker thread by the UI) and the small helpers that turn
//! the server's `/v1/jobs/overview` JSON into the lines the card shows.

use serde_json::Value;

use crate::api::ApiClient;

fn client(credentials: &crate::worker::Credentials) -> Result<ApiClient, String> {
    ApiClient::new(credentials.endpoint.clone(), credentials.token.clone())
        .map_err(|error| error.to_string())
}

pub fn get_overview(credentials: &crate::worker::Credentials) -> Result<Value, String> {
    client(credentials)?
        .jobs_overview()
        .map_err(|error| error.to_string())
}

pub fn set_auto_watch(
    credentials: &crate::worker::Credentials,
    key: &str,
    on: bool,
) -> Result<Value, String> {
    client(credentials)?
        .jobs_source_auto_watch(key, on)
        .map_err(|error| error.to_string())
}

/// Run the watch now; the answer carries the report and a fresh overview.
pub fn watch_now(credentials: &crate::worker::Credentials) -> Result<Value, String> {
    client(credentials)?
        .jobs_watch()
        .map_err(|error| error.to_string())
}

fn list(value: &Value, empty: &str) -> String {
    let items: Vec<&str> = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if items.is_empty() {
        empty.to_owned()
    } else {
        items.join(", ")
    }
}

pub fn frequency_label(value: &str) -> &'static str {
    match value {
        "twice_daily" => "Twice a day",
        "weekly" => "Once a week",
        "off" => "Off",
        _ => "Once a day",
    }
}

/// `(label, value)` rows summarising the profile; empty when there is none.
pub fn profile_rows(profile: &Value) -> Vec<(&'static str, String)> {
    if !profile.is_object() {
        return Vec::new();
    }
    vec![
        ("Looking for", list(&profile["roles"], "anything")),
        ("Where", list(&profile["cities"], "anywhere")),
        ("Sectors", list(&profile["sectors"], "all")),
        (
            "Check",
            frequency_label(profile["frequency"].as_str().unwrap_or("daily")).to_owned(),
        ),
    ]
}

/// Pipeline counts in order: `(status label, count)`.
pub fn pipeline(counts: &Value) -> Vec<(String, i64)> {
    counts
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            let status = row["status"].as_str().unwrap_or_default();
            let mut label = status.to_owned();
            if let Some(first) = label.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            (label, row["count"].as_i64().unwrap_or(0))
        })
        .collect()
}

/// One line for a match: title, employer, city, deadline.
pub fn match_line(item: &Value) -> String {
    let mut parts = vec![item["title"].as_str().unwrap_or("Job").to_owned()];
    for key in ["employer", "city"] {
        if let Some(text) = item[key].as_str().filter(|t| !t.is_empty()) {
            parts.push(text.to_owned());
        }
    }
    if let Some(deadline) = item["deadline"].as_str().filter(|t| !t.is_empty()) {
        parts.push(format!("deadline {deadline}"));
    }
    parts.join(" \u{2014} ")
}

/// A web address safe to show as a link.
pub fn safe_link(url: &str) -> Option<&str> {
    (url.starts_with("https://") || url.starts_with("http://")).then_some(url)
}

/// What the last run did, in one sentence.
pub fn last_run_line(last: &Value) -> String {
    let Some(at) = last["at"].as_str() else {
        return "Not checked yet.".into();
    };
    let when = at.get(..16).unwrap_or(at).replace('T', " ");
    let report = &last["report"];
    if let Some(error) = report["error"].as_str() {
        return format!("Last check {when}: {error}");
    }
    let sources = report["sources"].as_array().map(Vec::len).unwrap_or(0);
    let new = report["new_matches"].as_array().map(Vec::len).unwrap_or(0);
    format!("Last check {when} UTC: {sources} source(s), {new} new match(es).")
}

pub fn source_status_label(auto_watch: bool, builtin: bool) -> String {
    format!(
        "{}{}",
        if auto_watch {
            "Auto-watch on"
        } else {
            "Auto-watch off"
        },
        if builtin { "" } else { " \u{b7} yours" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn profile_rows_and_pipeline_read_well() {
        assert!(profile_rows(&Value::Null).is_empty());
        let rows = profile_rows(
            &json!({"roles": ["comptable"], "cities": [], "sectors": ["public"], "frequency": "twice_daily"}),
        );
        assert_eq!(rows[0], ("Looking for", "comptable".to_owned()));
        assert_eq!(rows[1].1, "anywhere");
        assert_eq!(rows[3].1, "Twice a day");
        let counts =
            pipeline(&json!([{"status": "saved", "count": 2}, {"status": "applied", "count": 0}]));
        assert_eq!(
            counts,
            vec![("Saved".to_owned(), 2), ("Applied".to_owned(), 0)]
        );
    }

    #[test]
    fn match_lines_links_and_last_run() {
        let line = match_line(
            &json!({"title": "Comptable", "employer": "Beta", "city": "", "deadline": "2026-12-01"}),
        );
        assert_eq!(line, "Comptable \u{2014} Beta \u{2014} deadline 2026-12-01");
        assert_eq!(
            safe_link("https://x.example/a"),
            Some("https://x.example/a")
        );
        assert_eq!(safe_link("javascript:alert(1)"), None);
        assert_eq!(last_run_line(&Value::Null), "Not checked yet.");
        let line = last_run_line(
            &json!({"at": "2026-11-20T08:30:00+00:00", "report": {"sources": [1, 2], "new_matches": [1]}}),
        );
        assert!(line.contains("2 source(s), 1 new"));
        assert!(
            last_run_line(
                &json!({"at": "2026-11-20T08:30:00Z", "report": {"error": "no profile"}})
            )
            .contains("no profile")
        );
        assert_eq!(
            source_status_label(true, false),
            "Auto-watch on \u{b7} yours"
        );
    }
}
