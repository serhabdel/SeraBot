//! Automated A8 desktop smoke (Big Wave, steps 1–7 in `tasks/big-wave.md`).
//!
//! The owner's live WebView2 smoke cannot run in CI. This test drives the same
//! endpoints the console calls for each step, against a seeded admin and a
//! seeded member: the admin sees the operator views, the member sees only their
//! own work, and member requests to admin-only views are refused with 403.
//! Every secret used here is synthetic.

use super::*;
use crate::plans::{PlanItem, PlanStatus};
use crate::scheduler::ensure_daily_routine;
use chrono_tz::Europe::Rome;

const SECRET_PROBE: &str = "synthetic-a8-probe-token";
const BEARER_SHAPED: &str = "Bearer abcdef0123456789abcdef";
const ADMIN_SOURCE: &str = "a8-admin-source";
const ADMIN_FEED_URL: &str = "https://example.invalid/a8-feed?access=a8-feed-key";

async fn send(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

async fn ok_json(app: &Router, token: &str, uri: &str) -> Value {
    let response = send(app, token, "GET", uri, None).await;
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    read_json(response).await
}

fn goals(workflows: &Value) -> Vec<&str> {
    workflows["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["goal"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a8_smoke_steps_one_to_seven_hold_for_admin_and_member() {
    // One configured source, so the health row exists for the admin's run to attach to.
    let base = temp_base();
    std::fs::create_dir_all(base.join("config")).unwrap();
    std::fs::write(
        base.join("config").join("sources.toml"),
        format!(
            "[[sources]]\nname = \"{ADMIN_SOURCE}\"\nurl = \"{ADMIN_FEED_URL}\"\nenabled = true\n"
        ),
    )
    .unwrap();
    let (app, _, store) = app_with_root(true, Policy::empty(), base);
    let admin_id = admin(&store);
    let admin_token = mint_for(&store, &admin_id, "a8-admin");
    let member = store
        .create_user(
            "subject-a8-member",
            "a8-member@example.example",
            "Synthetic Member",
        )
        .unwrap();
    let member_token = mint_for(&store, &member.id, "a8-member");

    // The admin's own work: a routine, an active plan, a failed source run and a queued task.
    ensure_daily_routine(&store, &admin_id, "morning_brief", "07:30", Rome).unwrap();
    store
        .create_conversation(&admin_id, "dir:a8-admin", "direct", None)
        .unwrap();
    store
        .set_plan(
            &admin_id,
            "dir:a8-admin",
            "Admin plan",
            &[PlanItem {
                text: "admin step".into(),
                status: PlanStatus::Doing,
            }],
        )
        .unwrap();
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO source_runs(id, source, started_at, finished_at, ok, error, items_seen, user_id)
             VALUES ('run-a8-admin', ?2, '2026-11-10T10:00:00Z', '2026-11-10T10:01:00Z', 0,
                     'synthetic failure', 0, ?1)",
            rusqlite::params![admin_id, ADMIN_SOURCE],
        )
        .unwrap();
    store
        .enqueue_task(
            &admin_id,
            "turn",
            "main",
            &json!({"conversation_id": "dir:a8-admin"}),
            0,
            chrono::Utc::now(),
        )
        .unwrap();

    // Step 1 — Workflows: routines, source health and active plans load for the admin.
    let routines = ok_json(&app, &admin_token, "/v1/routines").await;
    assert_eq!(routines["routines"][0]["name"], "morning_brief");
    let workflows = ok_json(&app, &admin_token, "/v1/workflows/active").await;
    assert_eq!(goals(&workflows), ["Admin plan"], "{workflows}");
    let sources = ok_json(&app, &admin_token, "/v1/monitoring/sources").await;
    assert!(
        sources.to_string().contains(ADMIN_SOURCE),
        "admin source health row missing: {sources}"
    );
    assert_eq!(
        sources["sources"][0]["url"], ADMIN_FEED_URL,
        "admin must see the feed URL: {sources}"
    );
    let admin_source_list = ok_json(&app, &admin_token, "/v1/sources").await;
    assert_eq!(
        admin_source_list["sources"][0]["url"], ADMIN_FEED_URL,
        "admin must see the feed URL on GET /v1/sources: {admin_source_list}"
    );

    // Step 2 — Jobs: the queued task is listed, and traces are reachable from it.
    let jobs = ok_json(&app, &admin_token, "/v1/jobs").await;
    assert_eq!(jobs["tasks"].as_array().unwrap().len(), 1, "{jobs}");
    assert_eq!(jobs["reliability"]["traces_path"], "/v1/traces");
    let traces = ok_json(
        &app,
        &admin_token,
        "/v1/traces?conversation_id=dir:a8-admin",
    )
    .await;
    assert!(traces["traces"].is_array(), "{traces}");

    // Step 3 — Logs: `q=tool=` returns a redacted tool line with no bearer-shaped secret.
    // Shared with the logs tests: the ring and file writer are process-wide.
    let _ = super::shared_log_dir();
    let marker = format!("a8-marker-{}", uuid::Uuid::new_v4());
    tracing::info!("tool=a8.echo {marker} {BEARER_SHAPED}");
    let logs = ok_json(&app, &admin_token, "/v1/logs?q=tool=").await;
    let text = logs.to_string();
    assert!(text.contains(&marker), "tool line not in logs: {logs}");
    assert!(
        !text.contains("abcdef0123456789abcdef"),
        "bearer secret leaked into logs: {logs}"
    );

    // Step 4 — Vault Test: the probe returns only the four status fields, never the token.
    let put = send(
        &app,
        &admin_token,
        "PUT",
        "/v1/vault/providers/onepassword",
        Some(json!({"token": SECRET_PROBE})),
    )
    .await;
    assert_eq!(put.status(), StatusCode::OK);
    let probe = read_json(
        send(
            &app,
            &admin_token,
            "POST",
            "/v1/vault/providers/onepassword/test",
            None,
        )
        .await,
    )
    .await;
    let mut keys: Vec<&str> = probe
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["cli_found", "configured", "message", "ok"],
        "{probe}"
    );
    assert!(!probe.to_string().contains(SECRET_PROBE));
    if probe["cli_found"] == false {
        assert_eq!(
            probe["ok"], false,
            "a missing CLI must fail gracefully: {probe}"
        );
    }

    // Step 5 — Usage health card: admin gets uptime, memory and storage counts; quota is present.
    let status = ok_json(&app, &admin_token, "/v1/admin/status").await;
    assert!(status["uptime_secs"].is_u64(), "{status}");
    assert!(status["process"]["memory_bytes"].is_number(), "{status}");
    assert!(status["storage"]["categories"].is_array(), "{status}");
    let usage = ok_json(&app, &admin_token, "/v1/usage").await;
    assert!(
        usage["quota"]["daily_tasks"]["limit"].is_number(),
        "{usage}"
    );

    // Step 6 — Member Workflows: the member sees only their own routine, plan and rows.
    ensure_daily_routine(&store, &member.id, "morning_brief", "08:00", Rome).unwrap();
    store
        .create_conversation(&member.id, "dir:a8-member", "direct", None)
        .unwrap();
    store
        .set_plan(
            &member.id,
            "dir:a8-member",
            "Member plan",
            &[PlanItem {
                text: "member step".into(),
                status: PlanStatus::Pending,
            }],
        )
        .unwrap();
    let member_routines = ok_json(&app, &member_token, "/v1/routines").await;
    assert_eq!(
        member_routines["routines"].as_array().unwrap().len(),
        1,
        "{member_routines}"
    );
    assert_eq!(member_routines["routines"][0]["config"]["time"], "08:00");
    let member_workflows = ok_json(&app, &member_token, "/v1/workflows/active").await;
    assert_eq!(
        goals(&member_workflows),
        ["Member plan"],
        "{member_workflows}"
    );
    // A member can start a watch of the admin's sources, so a failed fetch of the keyed feed
    // is stored under the member's own run row. The member may see that run, but the feed
    // key must not appear in `last_run.error`. The admin's run error must not leak.
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO source_runs(id, source, started_at, finished_at, ok, error, items_seen, user_id)
             VALUES ('run-a8-member', ?1, '2026-11-10T11:00:00Z', '2026-11-10T11:01:00Z', 0, ?2, 0, ?3)",
            rusqlite::params![
                ADMIN_SOURCE,
                format!("fetch failed: fetch {ADMIN_FEED_URL}: connection refused"),
                member.id
            ],
        )
        .unwrap();
    // The configured source list is shared config, so the name shows for everyone; the admin's
    // run status (last run, error text) must not.
    let member_sources = ok_json(&app, &member_token, "/v1/monitoring/sources").await;
    let member_rows = member_sources["sources"].as_array().unwrap();
    assert_eq!(member_rows[0]["name"], ADMIN_SOURCE, "{member_sources}");
    assert!(
        member_rows.iter().all(|row| row["url"].is_null()),
        "feed URL must be redacted for a member: {member_sources}"
    );
    assert!(
        !member_sources.to_string().contains(ADMIN_FEED_URL),
        "feed URL leaked to member: {member_sources}"
    );
    let last_error = member_rows[0]["last_run"]["error"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        last_error,
        "fetch failed: fetch https://example.invalid/a8-feed?[redacted]: connection refused",
        "{member_sources}"
    );
    assert!(
        !member_sources.to_string().contains("a8-feed-key"),
        "feed key leaked into last_run.error for a member: {member_sources}"
    );
    assert!(
        !member_sources.to_string().contains("synthetic failure"),
        "admin run error leaked to member: {member_sources}"
    );
    let member_source_list = ok_json(&app, &member_token, "/v1/sources").await;
    assert_eq!(
        member_source_list["sources"][0]["name"], ADMIN_SOURCE,
        "{member_source_list}"
    );
    assert!(
        member_source_list["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["url"].is_null()),
        "GET /v1/sources must redact feed URLs for a member: {member_source_list}"
    );
    assert!(
        !member_source_list.to_string().contains("a8-feed-key"),
        "feed key leaked through GET /v1/sources: {member_source_list}"
    );
    let member_jobs = ok_json(&app, &member_token, "/v1/jobs").await;
    assert!(
        member_jobs["tasks"].as_array().unwrap().is_empty(),
        "admin task leaked to member: {member_jobs}"
    );

    // Step 7 — Member Usage: no health card; admin-only views answer 403.
    for uri in ["/v1/admin/status", "/v1/logs", "/v1/admin/grants"] {
        let member_denied = send(&app, &member_token, "GET", uri, None).await;
        assert_eq!(
            member_denied.status(),
            StatusCode::FORBIDDEN,
            "member must get 403 from admin-only {uri}"
        );
    }
    let member_usage = ok_json(&app, &member_token, "/v1/usage").await;
    assert_eq!(member_usage["scope"], "me", "{member_usage}");
    assert!(member_usage.get("uptime_secs").is_none(), "{member_usage}");
    assert!(member_usage.get("storage").is_none(), "{member_usage}");

    // No secrets in responses. Every body a member can read, error bodies included,
    // lacks the probe secret, the admin's bearer token, the bearer-shaped probe and
    // the admin feed URL. The admin's own views also never echo a bearer token.
    let member_views = [
        "/v1/routines",
        "/v1/workflows/active",
        "/v1/monitoring/sources",
        "/v1/sources",
        "/v1/jobs",
        "/v1/usage",
        "/v1/admin/status",
        "/v1/logs",
        "/v1/admin/grants",
    ];
    let mut member_bodies = String::new();
    for uri in member_views {
        let response = send(&app, &member_token, "GET", uri, None).await;
        member_bodies.push_str(&read_body(response).await);
    }
    for secret in [
        SECRET_PROBE,
        BEARER_SHAPED,
        admin_token.as_str(),
        member_token.as_str(),
        ADMIN_FEED_URL,
    ] {
        assert!(
            !member_bodies.contains(secret),
            "secret leaked into a member response: {secret}"
        );
    }
    let mut admin_bodies = String::new();
    for uri in ["/v1/monitoring/sources", "/v1/admin/status", "/v1/vault"] {
        let response = send(&app, &admin_token, "GET", uri, None).await;
        admin_bodies.push_str(&read_body(response).await);
    }
    for secret in [SECRET_PROBE, admin_token.as_str(), member_token.as_str()] {
        assert!(
            !admin_bodies.contains(secret),
            "secret leaked into an admin response: {secret}"
        );
    }
}
