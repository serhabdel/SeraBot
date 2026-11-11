//! v0.33.0: a member's own feeds show their URL, and the digest is per user.
//!
//! Synthetic feeds and keys only. Admin-defined feeds stay hidden from members.

use super::*;

const ADMIN_FEED: &str = "https://example.invalid/admin-feed?key=v033-admin-key";
const MEMBER_FEED: &str = "https://example.invalid/member-feed?key=v033-member-key";
const ADMIN_SOURCE: &str = "v033-admin-source";
const OWN_FEED: &str = "v033-own-feed";

fn base_with_admin_source() -> std::path::PathBuf {
    let base = temp_base();
    std::fs::create_dir_all(base.join("config")).unwrap();
    std::fs::write(
        base.join("config").join("sources.toml"),
        format!("[[sources]]\nname = \"{ADMIN_SOURCE}\"\nurl = \"{ADMIN_FEED}\"\nenabled = true\n"),
    )
    .unwrap();
    base
}

async fn get_json(app: &Router, token: &str, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request_with_token("GET", uri, token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    read_json(response).await
}

fn row<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("no row {name}: {body}"))
}

fn seed_item(
    store: &Store,
    user_id: &str,
    source: &str,
    title: &str,
    first_seen: &str,
    status: &str,
) {
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO opportunities(id, source, external_id, title, summary, url, status, evidence,
                                       revision, first_seen, last_seen, user_id)
             VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, '{}', 1, ?7, ?7, ?8)",
            rusqlite::params![
                crate::domain::new_id(),
                source,
                title,
                title,
                format!("https://example.invalid/{title}"),
                status,
                first_seen,
                user_id
            ],
        )
        .unwrap();
}

#[tokio::test]
async fn a_member_sees_the_url_of_their_own_feed_and_not_an_admin_feed() {
    let (app, _, store) = app_with_root(true, Policy::empty(), base_with_admin_source());
    let admin_token = mint_for(&store, &admin(&store), "v033-admin");
    let member = store
        .create_user("sub-v033-own", "v033-own@example.example", "V033 Own")
        .unwrap();
    let member_token = mint_for(&store, &member.id, "v033-own-member");
    store
        .add_member_rss_source(&member.id, OWN_FEED, MEMBER_FEED)
        .unwrap();

    let sources = get_json(&app, &member_token, "/v1/sources").await;
    assert_eq!(row(&sources, OWN_FEED)["url"], MEMBER_FEED, "{sources}");
    assert!(row(&sources, ADMIN_SOURCE)["url"].is_null(), "{sources}");
    let text = sources.to_string();
    assert!(
        !text.contains(ADMIN_FEED) && !text.contains("v033-admin-key"),
        "{text}"
    );

    let monitoring = get_json(&app, &member_token, "/v1/monitoring/sources").await;
    assert_eq!(
        row(&monitoring, OWN_FEED)["url"],
        MEMBER_FEED,
        "{monitoring}"
    );
    assert_eq!(row(&monitoring, OWN_FEED)["kind"], "rss", "{monitoring}");
    assert!(
        row(&monitoring, ADMIN_SOURCE)["url"].is_null(),
        "{monitoring}"
    );
    assert!(
        !monitoring.to_string().contains("v033-admin-key"),
        "{monitoring}"
    );

    // The admin does not see the member's feed in their own list, and still sees
    // the admin feed URL.
    let admin_sources = get_json(&app, &admin_token, "/v1/sources").await;
    assert_eq!(
        row(&admin_sources, ADMIN_SOURCE)["url"],
        ADMIN_FEED,
        "{admin_sources}"
    );
}

#[tokio::test]
async fn another_member_never_sees_someone_elses_feed() {
    let (app, _, store) = app_with_root(true, Policy::empty(), base_with_admin_source());
    let owner = store
        .create_user("sub-v033-a", "v033-a@example.example", "A")
        .unwrap();
    let other = store
        .create_user("sub-v033-b", "v033-b@example.example", "B")
        .unwrap();
    store
        .add_member_rss_source(&owner.id, OWN_FEED, MEMBER_FEED)
        .unwrap();
    let other_token = mint_for(&store, &other.id, "v033-other");

    let sources = get_json(&app, &other_token, "/v1/sources").await;
    assert!(
        !sources.to_string().contains(OWN_FEED) && !sources.to_string().contains(MEMBER_FEED),
        "{sources}"
    );
}

#[tokio::test]
async fn digest_lists_only_the_callers_recent_open_items_and_failing_sources() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("sub-v033-d1", "v033-d1@example.example", "D1")
        .unwrap();
    let other = store
        .create_user("sub-v033-d2", "v033-d2@example.example", "D2")
        .unwrap();
    let member_token = mint_for(&store, &member.id, "v033-digest-member");
    let now = chrono::Utc::now();
    let recent = crate::digest::stored_time(now - chrono::Duration::hours(2));
    let older = crate::digest::stored_time(now - chrono::Duration::hours(50));
    seed_item(
        &store,
        &member.id,
        "v033-news",
        "fresh-item",
        &recent,
        "open",
    );
    seed_item(
        &store,
        &member.id,
        "v033-news",
        "older-item",
        &older,
        "open",
    );
    seed_item(
        &store,
        &member.id,
        "v033-news",
        "gone-item",
        &recent,
        "withdrawn",
    );
    seed_item(
        &store,
        &other.id,
        "v033-news",
        "other-item",
        &recent,
        "open",
    );
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO source_runs(id, source, started_at, finished_at, ok, error, items_seen, user_id)
             VALUES ('run-v033-digest', 'v033-broken', '2026-11-11T10:00:00Z',
                     '2026-11-11T10:01:00Z', 0, 'synthetic failure', 0, ?1)",
            rusqlite::params![member.id],
        )
        .unwrap();

    let day = get_json(&app, &member_token, "/v1/digest?hours=24").await;
    assert_eq!(day["count"], 1, "{day}");
    assert_eq!(day["items"][0]["title"], "fresh-item", "{day}");
    assert!(!day.to_string().contains("other-item"), "{day}");
    assert!(
        !day.to_string().contains("gone-item"),
        "withdrawn items stay out: {day}"
    );
    assert_eq!(day["failing_sources"], json!(["v033-broken"]), "{day}");

    let wide = get_json(&app, &member_token, "/v1/digest?hours=72").await;
    assert_eq!(wide["count"], 2, "{wide}");

    let since = crate::digest::stored_time(now - chrono::Duration::hours(1));
    let narrow = get_json(&app, &member_token, &format!("/v1/digest?since={since}")).await;
    assert_eq!(narrow["count"], 0, "{narrow}");

    let bad = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/digest?since=yesterday",
            &member_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn the_digest_command_reply_matches_the_endpoint_scope() {
    let (_app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("sub-v033-c", "v033-c@example.example", "C")
        .unwrap();
    let now = chrono::Utc::now();
    let recent = crate::digest::stored_time(now - chrono::Duration::hours(3));
    seed_item(
        &store,
        &member.id,
        "v033-news",
        "chat-item",
        &recent,
        "open",
    );

    let reply = crate::digest::command_reply(&store, &member.id, "", now).unwrap();
    assert!(reply.contains("chat-item"), "{reply}");
    assert!(
        reply.starts_with("1 new item(s) in the last 24 hour(s):"),
        "{reply}"
    );

    let bad = crate::digest::command_reply(&store, &member.id, "0", now).unwrap();
    assert_eq!(
        bad,
        "Use /digest or /digest <hours>, with hours from 1 to 168."
    );
}
