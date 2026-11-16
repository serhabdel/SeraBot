//! `suggest_marketplace_app` over a synthetic catalog.

use std::collections::HashSet;

use super::*;

fn manifest(slug: &str, name: &str, tagline: &str, url: &str, auth: Value) -> PluginManifest {
    serde_json::from_value(json!({
        "schemaVersion": 2,
        "slug": slug,
        "name": name,
        "tagline": tagline,
        "description": format!("{name} for synthetic tests."),
        "category": "design",
        "iconUrl": "https://icons.example/icon.png",
        "version": "1.0.0",
        "minServerVersion": "0.51.0",
        "apps": [{
            "id": "main",
            "name": name,
            "server": {"transport": "http", "url": url},
            "auth": auth,
        }],
        "permissions": {"egress": ["mcp.example"]},
        "websiteUrl": "https://example.example",
        "privacyPolicyUrl": "https://example.example/privacy",
        "termsUrl": "https://example.example/terms",
    }))
    .unwrap()
}

fn catalog() -> Vec<PluginManifest> {
    vec![
        manifest(
            "poster-studio",
            "Poster Studio",
            "Design posters and social images.",
            "https://mcp.poster.example/mcp",
            json!([{"id": "login", "kind": "oauth", "oauth": {"scopes": []}}]),
        ),
        manifest(
            "poster-notes",
            "Poster Notes",
            "Notes about posters.",
            "https://mcp.notes.example/mcp",
            json!([{"id": "key", "kind": "api-key", "fields": []}]),
        ),
        manifest(
            "poster-open",
            "Poster Open",
            "Open poster tools.",
            "https://mcp.open.example/mcp",
            json!([]),
        ),
        manifest(
            "poster-placeholder",
            "Poster Placeholder",
            "Design posters (placeholder host).",
            "https://mcp.poster.example.invalid/mcp",
            json!([{"id": "login", "kind": "oauth", "oauth": {"scopes": []}}]),
        ),
        manifest(
            "poster-link",
            "Poster Link",
            "Design posters (link-out sign-in).",
            "https://mcp.link.example/mcp",
            json!([{"id": "link", "kind": "oauth-link"}]),
        ),
        manifest(
            "ledger",
            "Ledger",
            "Track accounts.",
            "https://mcp.ledger.example/mcp",
            json!([]),
        ),
    ]
}

struct World {
    store: Store,
    user: String,
    other: String,
    tool: SuggestMarketplaceApp,
}

fn world() -> World {
    let store = Store::open_in_memory(384).unwrap();
    let user = store
        .create_user("microsoft:suggest-a", "a@example.example", "A")
        .unwrap()
        .id;
    let other = store
        .create_user("microsoft:suggest-b", "b@example.example", "B")
        .unwrap()
        .id;
    World {
        tool: SuggestMarketplaceApp::new(store.clone(), catalog()),
        store,
        user,
        other,
    }
}

fn context(user: &str) -> ToolContext {
    ToolContext {
        user_id: user.into(),
        actor: user.into(),
        bot: "main".into(),
        task_id: None,
        turn_id: None,
        conversation_id: None,
        allowed_tools: HashSet::new(),
        require_approval: HashSet::new(),
    }
}

fn slugs(output: &ToolOutput) -> Vec<String> {
    output
        .cards
        .iter()
        .map(|card| match card {
            ResultCard::Install { slug, .. } => slug.clone(),
            other => panic!("not an install card: {other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn it_shows_install_cards_for_apps_that_can_run() {
    let world = world();
    let output = world
        .tool
        .execute(json!({"query": "design a poster"}), &context(&world.user))
        .await
        .unwrap();
    // Name matches rank first; the placeholder host and the link-out sign-in
    // never appear; an unrelated app does not match.
    let shown = slugs(&output);
    assert_eq!(shown, ["poster-notes", "poster-open", "poster-studio"]);
    let ResultCard::Install { needs, .. } = &output.cards[2] else {
        panic!("install card");
    };
    assert_eq!(needs, "sign_in");
    let needs: Vec<&str> = output
        .cards
        .iter()
        .map(|card| match card {
            ResultCard::Install { needs, .. } => needs.as_str(),
            _ => "",
        })
        .collect();
    assert_eq!(needs, ["api_key", "none", "sign_in"]);
    assert!(output.content.contains("Nothing was installed"));
    // The card serialises as a typed card clients can read.
    let card = serde_json::to_value(&output.cards[0]).unwrap();
    assert_eq!(card["type"], "install");
    assert_eq!(card["slug"], "poster-notes");
}

#[tokio::test]
async fn it_is_read_only_and_installs_nothing() {
    let world = world();
    let spec = world.tool.spec();
    assert_eq!(spec.name, "suggest_marketplace_app");
    assert!(!spec.external_effect);
    assert!(!world.tool.requires_approval(&json!({"query": "poster"})));
    world
        .tool
        .execute(json!({"query": "poster"}), &context(&world.user))
        .await
        .unwrap();
    assert!(
        world
            .store
            .marketplace_installs(&world.user)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_app_the_member_already_runs_is_not_suggested_again() {
    let world = world();
    world
        .store
        .marketplace_install_insert(&world.user, "poster-studio", "1.0.0")
        .unwrap();
    world
        .store
        .marketplace_install_set_enabled(&world.user, "poster-studio", true)
        .unwrap();
    let mine = world
        .tool
        .execute(json!({"query": "poster"}), &context(&world.user))
        .await
        .unwrap();
    assert!(!slugs(&mine).contains(&"poster-studio".to_string()));
    // Another member's installs do not hide anything.
    let theirs = world
        .tool
        .execute(json!({"query": "poster"}), &context(&world.other))
        .await
        .unwrap();
    assert!(slugs(&theirs).contains(&"poster-studio".to_string()));
}

#[tokio::test]
async fn no_match_says_so_and_shows_no_card() {
    let world = world();
    let output = world
        .tool
        .execute(json!({"query": "quantum knitting"}), &context(&world.user))
        .await
        .unwrap();
    assert!(output.cards.is_empty());
    assert!(output.content.contains("Do not suggest one"));
}

#[tokio::test]
async fn a_missing_query_is_refused() {
    let world = world();
    for args in [json!({}), json!({"query": "   "}), json!({"query": 7})] {
        assert!(
            world
                .tool
                .execute(args, &context(&world.user))
                .await
                .is_err()
        );
    }
}

#[test]
fn only_runnable_apps_count() {
    let catalog = catalog();
    let by_slug = |slug: &str| catalog.iter().find(|m| m.slug == slug).unwrap();
    assert_eq!(runnable(by_slug("poster-studio")), Some("sign_in"));
    assert_eq!(runnable(by_slug("poster-placeholder")), None);
    assert_eq!(runnable(by_slug("poster-link")), None);
    // The shipped manifests are all placeholders today, so none is suggested.
    let (shipped, _) = crate::marketplace::load_catalog_with_builtin(std::path::Path::new("none"));
    assert!(shipped.iter().all(|manifest| runnable(manifest).is_none()));
}
