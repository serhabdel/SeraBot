use std::collections::HashSet;
use std::sync::Arc;

use super::super::banners::BannerCandidate;
use super::super::manager::BrowserManager;
use super::super::testing::{FakeLauncher, FakeResolver, manager_with_base};
use super::super::tools::register_browser_tools;
use super::*;
use crate::gateway::{ToolRegistry, is_destructive_tool};
use crate::maker::ArtifactStore;
use crate::store::Store;

struct H {
    registry: ToolRegistry,
    launcher: Arc<FakeLauncher>,
    manager: Arc<BrowserManager>,
    store: Store,
    artifacts: Arc<ArtifactStore>,
    base: std::path::PathBuf,
}

fn harness(auto_dismiss: bool) -> H {
    let base = std::env::temp_dir().join(format!("serabot-ext-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let (manager, launcher) = manager_with_base(4, Duration::from_secs(600), base.clone());
    let store = Store::open_in_memory(384).unwrap();
    let artifacts = Arc::new(ArtifactStore::new(base.join("artifacts")).unwrap());
    let mut registry = ToolRegistry::default();
    register_browser_tools(
        &mut registry,
        BrowserToolDeps {
            manager: manager.clone(),
            resolver: Arc::new(FakeResolver::default()),
            artifacts: Some(artifacts.clone()),
            store: store.clone(),
            policy: Arc::new(crate::policy::Policy::from_text("forbiddenterm\n")),
            auto_dismiss_banners: auto_dismiss,
        },
    )
    .unwrap();
    H {
        registry,
        launcher,
        manager,
        store,
        artifacts,
        base,
    }
}

fn ctx(user: &str) -> ToolContext {
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

async fn run(h: &H, user: &str, tool: &str, args: Value) -> Result<ToolOutput> {
    h.registry
        .get(tool)
        .unwrap()
        .execute(args, &ctx(user))
        .await
}

async fn open(h: &H, user: &str) {
    run(
        h,
        user,
        "browser_navigate",
        json!({"url": "https://93.184.216.34/"}),
    )
    .await
    .unwrap();
}

fn button(text: &str) -> BannerCandidate {
    BannerCandidate {
        text: text.into(),
        tag: "button".into(),
        ..Default::default()
    }
}

#[test]
fn every_new_tool_is_registered_and_upload_needs_approval() {
    let h = harness(true);
    let specs = h.registry.specs();
    for name in [
        "browser_find",
        "browser_wait",
        "browser_select",
        "browser_check",
        "browser_scroll",
        "browser_hover",
        "browser_upload",
        "browser_download",
        "browser_pdf",
        "browser_dismiss_banners",
        "browser_forget_logins",
    ] {
        assert!(specs.iter().any(|s| s.name == name), "{name}");
    }
    assert!(is_destructive_tool("browser_upload"));
    let upload = h.registry.get("browser_upload").unwrap();
    assert!(upload.requires_approval(&json!({})));
    assert!(!upload.allow_standing_grant(&json!({})));
    assert!(
        !h.registry
            .get("browser_find")
            .unwrap()
            .requires_approval(&json!({}))
    );
    assert!(
        !h.registry
            .get("browser_click")
            .unwrap()
            .requires_approval(&json!({"ref": "e1"}))
    );
}

#[tokio::test]
async fn snapshot_modes() {
    let h = harness(false);
    open(&h, "alice").await;
    let full = run(&h, "alice", "browser_snapshot", json!({}))
        .await
        .unwrap();
    assert!(full.content.contains("textbox \"Password\""));
    let compact = run(
        &h,
        "alice",
        "browser_snapshot",
        json!({"mode": "interactive"}),
    )
    .await
    .unwrap();
    assert!(compact.content.contains("[e1] textbox"));
    let text = run(&h, "alice", "browser_snapshot", json!({"mode": "text"}))
        .await
        .unwrap();
    assert!(text.content.contains("Readable article text"));
    assert!(text.content.contains("no element refs"));
    assert!(
        h.launcher
            .calls_for("alice")
            .iter()
            .any(|c| c.starts_with("page_text"))
    );
    let bad = run(&h, "alice", "browser_snapshot", json!({"mode": "huge"})).await;
    assert!(matches!(bad, Err(SeraError::Validation(_))));
}

#[tokio::test]
async fn find_returns_refs_and_needs_a_criterion() {
    let h = harness(false);
    open(&h, "alice").await;
    let found = run(
        &h,
        "alice",
        "browser_find",
        json!({"role": "textbox", "name": "pass"}),
    )
    .await
    .unwrap();
    assert!(
        found.content.contains("e1 textbox \"Password\""),
        "{}",
        found.content
    );
    let none = run(&h, "alice", "browser_find", json!({"role": "slider"}))
        .await
        .unwrap();
    assert!(none.content.contains("no match"));
    let by_text = run(&h, "alice", "browser_find", json!({"text": "password"}))
        .await
        .unwrap();
    assert!(by_text.content.contains("e1"));
    assert!(matches!(
        run(&h, "alice", "browser_find", json!({})).await,
        Err(SeraError::Validation(_))
    ));
    assert!(matches!(
        run(
            &h,
            "alice",
            "browser_find",
            json!({"name": "x".repeat(500)})
        )
        .await,
        Err(SeraError::Validation(_))
    ));
}

#[tokio::test]
async fn wait_modes_and_bounds() {
    let h = harness(false);
    open(&h, "alice").await;
    let seen = run(&h, "alice", "browser_wait", json!({"text": "password"}))
        .await
        .unwrap();
    assert!(seen.content.contains("found after"));
    let element = run(&h, "alice", "browser_wait", json!({"role": "textbox"}))
        .await
        .unwrap();
    assert!(element.content.contains("e1 textbox"));
    let ready = run(&h, "alice", "browser_wait", json!({"state": "networkidle"}))
        .await
        .unwrap();
    assert!(ready.content.contains("the page is ready"));
    assert!(
        h.launcher
            .calls_for("alice")
            .contains(&"wait_load NetworkIdle".to_owned())
    );
    let missing = run(
        &h,
        "alice",
        "browser_wait",
        json!({"text": "never shows up", "timeout_secs": 1}),
    )
    .await
    .unwrap();
    assert!(missing.content.contains("timed out after 1s"));
    for bad in [
        json!({}),
        json!({"text": "a", "state": "load"}),
        json!({"text": "a", "timeout_secs": 31}),
        json!({"text": "a", "timeout_secs": 0}),
        json!({"state": "idle"}),
    ] {
        assert!(
            matches!(
                run(&h, "alice", "browser_wait", bad.clone()).await,
                Err(SeraError::Validation(_))
            ),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn select_check_hover_scroll_use_refs_and_validate() {
    let h = harness(false);
    open(&h, "alice").await;
    let out = run(
        &h,
        "alice",
        "browser_select",
        json!({"ref": "e1", "option": "Paris"}),
    )
    .await
    .unwrap();
    assert!(out.content.contains("selected \"Paris\""));
    let out = run(&h, "alice", "browser_check", json!({"ref": "e1"}))
        .await
        .unwrap();
    assert!(out.content.contains("now checked"));
    let out = run(
        &h,
        "alice",
        "browser_check",
        json!({"ref": "e1", "checked": false}),
    )
    .await
    .unwrap();
    assert!(out.content.contains("now unchecked"));
    run(&h, "alice", "browser_hover", json!({"ref": "e1"}))
        .await
        .unwrap();
    let out = run(
        &h,
        "alice",
        "browser_scroll",
        json!({"direction": "down", "amount": 1000}),
    )
    .await
    .unwrap();
    assert!(out.content.contains("scrolled down 1000px"));
    run(&h, "alice", "browser_scroll", json!({"to_ref": "e1"}))
        .await
        .unwrap();
    run(
        &h,
        "alice",
        "browser_scroll",
        json!({"direction": "up", "ref": "e1"}),
    )
    .await
    .unwrap();
    let calls = h.launcher.calls_for("alice");
    for expected in [
        "select e1 Paris",
        "check e1 true",
        "check e1 false",
        "hover e1",
        "scroll_by page 0 1000",
        "scroll_to e1",
        "scroll_by e1 0 -600",
    ] {
        assert!(
            calls.contains(&expected.to_owned()),
            "{expected}: {calls:?}"
        );
    }
    for (tool, bad) in [
        ("browser_select", json!({"ref": "bogus", "option": "x"})),
        ("browser_select", json!({"ref": "e1"})),
        ("browser_check", json!({"ref": "../etc"})),
        ("browser_hover", json!({"ref": ""})),
        ("browser_scroll", json!({"direction": "sideways"})),
        ("browser_scroll", json!({"amount": 0})),
        ("browser_scroll", json!({"amount": 99999})),
        ("browser_scroll", json!({"to_ref": "e1", "direction": "up"})),
    ] {
        assert!(
            matches!(
                run(&h, "alice", tool, bad.clone()).await,
                Err(SeraError::Validation(_))
            ),
            "{tool} {bad}"
        );
    }
}

#[tokio::test]
async fn an_actionability_failure_is_an_error_not_a_success() {
    let h = harness(false);
    open(&h, "alice").await;
    h.launcher.fail_next(
        "alice",
        "the button is covered by a cookie banner; close it first",
    );
    let error = run(&h, "alice", "browser_click", json!({"ref": "e1"}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("covered by a cookie banner"));
    h.launcher.fail_next("alice", "the field is disabled");
    let error = run(
        &h,
        "alice",
        "browser_select",
        json!({"ref": "e1", "option": "x"}),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().ends_with("the field is disabled"));
}

#[tokio::test]
async fn dismiss_banners_prefers_reject_and_never_clicks_payment() {
    let h = harness(false);
    open(&h, "alice").await;
    h.launcher.set_banners(
        "alice",
        vec![
            button("Accept all"),
            button("Reject all"),
            button("Manage preferences"),
        ],
    );
    let out = run(&h, "alice", "browser_dismiss_banners", json!({}))
        .await
        .unwrap();
    assert!(
        out.content.contains("rejected non-essential"),
        "{}",
        out.content
    );
    assert!(
        h.launcher
            .calls_for("alice")
            .contains(&"banner 1".to_owned())
    );
    let none = run(&h, "alice", "browser_dismiss_banners", json!({}))
        .await
        .unwrap();
    assert!(none.content.contains("no cookie or consent banner"));
    h.launcher
        .set_banners("alice", vec![button("Pay now"), button("Subscribe")]);
    let unsafe_only = run(&h, "alice", "browser_dismiss_banners", json!({}))
        .await
        .unwrap();
    assert!(
        unsafe_only.content.contains("no safe button"),
        "{}",
        unsafe_only.content
    );
    assert_eq!(
        h.launcher
            .calls_for("alice")
            .iter()
            .filter(|c| c.starts_with("banner"))
            .count(),
        1
    );
}

#[tokio::test]
async fn navigation_dismisses_a_banner_when_configured() {
    let on = harness(true);
    open(&on, "alice").await;
    on.launcher.set_banners(
        "alice",
        vec![button("Tout accepter"), button("Tout refuser")],
    );
    let out = run(
        &on,
        "alice",
        "browser_navigate",
        json!({"url": "https://93.184.216.34/b"}),
    )
    .await
    .unwrap();
    assert!(
        out.content.contains("a cookie banner was dismissed"),
        "{}",
        out.content
    );
    assert!(
        on.launcher
            .calls_for("alice")
            .contains(&"banner 1".to_owned())
    );

    let off = harness(false);
    open(&off, "alice").await;
    off.launcher.set_banners("alice", vec![button("Accept")]);
    run(
        &off,
        "alice",
        "browser_navigate",
        json!({"url": "https://93.184.216.34/b"}),
    )
    .await
    .unwrap();
    assert!(
        !off.launcher
            .calls_for("alice")
            .iter()
            .any(|c| c.starts_with("banner"))
    );
}

#[tokio::test]
async fn light_mode_is_a_per_session_switch() {
    let h = harness(false);
    run(
        &h,
        "alice",
        "browser_navigate",
        json!({"url": "https://93.184.216.34/", "light": true}),
    )
    .await
    .unwrap();
    run(
        &h,
        "alice",
        "browser_navigate",
        json!({"url": "https://93.184.216.34/2"}),
    )
    .await
    .unwrap();
    let calls = h.launcher.calls_for("alice");
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("block_heavy"))
            .count(),
        1
    );
    assert!(calls.contains(&"block_heavy true".to_owned()));
}

fn own_artifact(h: &H, user: &str, name: &str, bytes: &[u8]) -> String {
    let stored = h
        .artifacts
        .save(
            bytes,
            name,
            ArtifactMeta {
                title: name.into(),
                filename: name.into(),
                language: Language::En,
                source_artifact: None,
                created_at: Utc::now(),
            },
        )
        .unwrap();
    h.store
        .record_conversation_artifact(user, "conv", None, "test", &stored.hash, name, None)
        .unwrap();
    stored.hash
}

fn new_user(h: &H, sub: &str) -> String {
    h.store
        .create_user(
            &format!("microsoft:{sub}"),
            &format!("{sub}@example.example"),
            sub,
        )
        .unwrap()
        .id
}

#[tokio::test]
async fn upload_binds_file_and_site_and_cleans_up() {
    let h = harness(false);
    let user = new_user(&h, "ext-u");
    open(&h, &user).await;
    let hash = own_artifact(&h, &user, "cv.pdf", b"%PDF-private");
    let tool = h.registry.get("browser_upload").unwrap();
    let args = json!({"ref": "e1", "artifact": hash});
    let details = tool.approval_details_for(&args, &ctx(&user)).await.unwrap();
    assert_eq!(details.destination.as_deref(), Some("93.184.216.34"));
    assert_eq!(details.artifact_hash.as_deref(), Some(hash.as_str()));
    assert!(details.summary.contains("cv.pdf") && details.summary.contains("93.184.216.34"));
    assert_eq!(
        tool.live_destination(&args, &ctx(&user))
            .await
            .unwrap()
            .as_deref(),
        Some("93.184.216.34")
    );
    let out = tool.execute(args.clone(), &ctx(&user)).await.unwrap();
    assert!(out.content.contains("attached \"cv.pdf\""));
    assert!(
        h.launcher
            .calls_for(&user)
            .contains(&"upload e1 12 bytes".to_owned())
    );
    // The temp copy lived under the browser's own temp dir and is gone.
    let paths = h.launcher.uploaded_paths(&user);
    assert_eq!(paths.len(), 1);
    assert!(paths[0].starts_with(h.base.join(".tmp").join(&user)));
    assert!(!paths[0].exists());
    assert!(!paths[0].parent().unwrap().exists());
}

#[tokio::test]
async fn upload_refuses_foreign_artifacts_and_blocklisted_names() {
    let h = harness(false);
    let alice = new_user(&h, "ext-a");
    let bob = new_user(&h, "ext-b");
    open(&h, &bob).await;
    let hash = own_artifact(&h, &alice, "alice-private.pdf", b"%PDF-alice");
    let tool = h.registry.get("browser_upload").unwrap();
    let args = json!({"ref": "e1", "artifact": hash});
    assert!(matches!(
        tool.approval_details_for(&args, &ctx(&bob)).await,
        Err(SeraError::Forbidden(_))
    ));
    assert!(matches!(
        tool.execute(args, &ctx(&bob)).await,
        Err(SeraError::Forbidden(_))
    ));
    let blocked = own_artifact(&h, &bob, "forbiddenterm-plan.pdf", b"%PDF-x");
    assert!(matches!(
        tool.execute(json!({"ref": "e1", "artifact": blocked}), &ctx(&bob))
            .await,
        Err(SeraError::PolicyViolation)
    ));
    assert!(h.launcher.uploaded_paths(&bob).is_empty());
}

#[tokio::test]
async fn download_and_pdf_become_artifacts() {
    let h = harness(false);
    open(&h, "alice").await;
    let file = run(&h, "alice", "browser_download", json!({"ref": "e1"}))
        .await
        .unwrap();
    assert_eq!(file.filename.as_deref(), Some("report.csv"));
    let hash = file.artifact_hash.as_deref().unwrap();
    assert!(file.content.contains("report.csv"));
    assert!(file.content.contains("text/csv"));
    assert!(file.content.contains(hash));
    assert!(file.content.contains("xlsx_import"));
    let file_data = file.data.as_ref().unwrap();
    assert_eq!(file_data["artifact_hash"].as_str(), Some(hash));
    assert_eq!(file_data["mime"].as_str(), Some("text/csv"));
    let stored = h.artifacts.load(hash).unwrap();
    assert_eq!(stored.bytes, b"downloaded-bytes");
    assert!(!file.content.contains("downloaded-bytes"));
    let pdf = run(&h, "alice", "browser_pdf", json!({})).await.unwrap();
    assert!(pdf.filename.as_deref().unwrap().ends_with(".pdf"));
    assert!(pdf.content.contains("pdf_inspect"));
    assert_eq!(
        pdf.data.as_ref().unwrap()["mime"].as_str(),
        Some("application/pdf")
    );
    assert!(matches!(
        run(
            &h,
            "alice",
            "browser_download",
            json!({"ref": "e1", "timeout_secs": 99})
        )
        .await,
        Err(SeraError::Validation(_))
    ));
}

#[tokio::test]
async fn download_url_and_current_modes_validate_targets_and_save_artifacts() {
    let h = harness(false);
    open(&h, "alice").await;

    let (doc, page, layer) = printpdf::PdfDocument::new(
        "Sample PDF",
        printpdf::Mm(210.0),
        printpdf::Mm(297.0),
        "Layer 1",
    );
    let font = doc
        .add_builtin_font(printpdf::BuiltinFont::Helvetica)
        .unwrap();
    let long_line = format!("Invoice summary 2026 {}", "word ".repeat(400));
    doc.get_page(page).get_layer(layer).use_text(
        &long_line,
        12.0,
        printpdf::Mm(20.0),
        printpdf::Mm(260.0),
        &font,
    );
    let pdf_bytes = doc.save_to_bytes().unwrap();

    h.launcher.set_fake_download(
        "alice",
        "statement.pdf",
        "application/pdf",
        pdf_bytes.clone(),
    );
    let by_url = run(
        &h,
        "alice",
        "browser_download",
        json!({"url": "https://93.184.216.34/docs/statement.pdf"}),
    )
    .await
    .unwrap();
    let url_hash = by_url.artifact_hash.as_deref().unwrap();
    assert_eq!(by_url.filename.as_deref(), Some("statement.pdf"));
    assert_eq!(
        by_url.data.as_ref().unwrap()["mime"].as_str(),
        Some("application/pdf")
    );
    assert!(by_url.content.contains("statement.pdf"));
    assert!(by_url.content.contains(url_hash));
    assert!(by_url.content.contains("pdf_inspect"));
    assert!(by_url.content.contains("document_index"));
    assert!(by_url.content.contains(UNTRUSTED_BANNER));
    assert!(by_url.content.contains("Invoice summary 2026"));
    let preview_part = by_url
        .content
        .split("First page text preview:\n")
        .nth(1)
        .unwrap();
    // Bounded to ~1500 chars + trailing "...".
    assert!(preview_part.chars().count() <= PDF_PREVIEW_CHARS + 3);
    assert_eq!(h.artifacts.load(url_hash).unwrap().bytes, pdf_bytes);

    h.launcher.set_fake_download(
        "alice",
        "viewer.pdf",
        "application/pdf",
        b"%PDF-1.4 viewer".to_vec(),
    );
    let current = run(&h, "alice", "browser_download", json!({"current": true}))
        .await
        .unwrap();
    assert_eq!(current.filename.as_deref(), Some("viewer.pdf"));
    assert!(current.content.contains("pdf_inspect"));

    // A valid PDF with no extractable text (scanned/image-only) includes a clear hint.
    let (empty_doc, _page, _layer) = printpdf::PdfDocument::new(
        "Scanned PDF",
        printpdf::Mm(210.0),
        printpdf::Mm(297.0),
        "Layer 1",
    );
    let empty_pdf_bytes = empty_doc.save_to_bytes().unwrap();
    h.launcher
        .set_fake_download("alice", "scanned.pdf", "application/pdf", empty_pdf_bytes);
    let scanned = run(
        &h,
        "alice",
        "browser_download",
        json!({"url": "https://93.184.216.34/docs/scanned.pdf"}),
    )
    .await
    .unwrap();
    assert_eq!(scanned.filename.as_deref(), Some("scanned.pdf"));
    assert!(
        scanned.content.contains("scanned or image-only")
            && scanned.content.contains("pdf_to_images")
            && scanned.content.contains("OCR"),
        "{}",
        scanned.content
    );
    assert!(!scanned.content.contains("First page text preview:"));

    // Next-tool guidance for docx as well.
    h.launcher.set_fake_download(
        "alice",
        "brief.docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        b"PK\x03\x04docx".to_vec(),
    );
    let docx = run(
        &h,
        "alice",
        "browser_download",
        json!({"url": "https://93.184.216.34/brief.docx"}),
    )
    .await
    .unwrap();
    assert!(docx.content.contains("docx_import"));

    // Target mode validation: must provide exactly one of ref, url, or current=true.
    for bad in [
        json!({}),
        json!({"current": false}),
        json!({"ref": "e1", "url": "https://93.184.216.34/a.pdf"}),
        json!({"ref": "e1", "current": true}),
        json!({"url": "https://93.184.216.34/a.pdf", "current": true}),
    ] {
        assert!(
            matches!(
                run(&h, "alice", "browser_download", bad.clone()).await,
                Err(SeraError::Validation(_))
            ),
            "expected validation error for {bad}"
        );
    }

    // SSRF: url mode must reject loopback, link-local metadata, localhost, and file URLs.
    for blocked in [
        "http://127.0.0.1/secret.pdf",
        "http://169.254.169.254/latest/meta-data",
        "http://localhost/report.pdf",
        "file:///etc/passwd",
    ] {
        assert!(
            matches!(
                run(&h, "alice", "browser_download", json!({"url": blocked})).await,
                Err(SeraError::Validation(_))
            ),
            "expected SSRF validation error for {blocked}"
        );
    }

    // Blocklist filename check.
    h.launcher.set_fake_download(
        "alice",
        "forbiddenterm.pdf",
        "application/pdf",
        b"%PDF-1.4".to_vec(),
    );
    assert!(matches!(
        run(
            &h,
            "alice",
            "browser_download",
            json!({"url": "https://93.184.216.34/forbiddenterm.pdf"})
        )
        .await,
        Err(SeraError::PolicyViolation)
    ));

    // Size limit enforcement.
    h.launcher.set_fake_download(
        "alice",
        "huge.pdf",
        "application/pdf",
        vec![b'a'; (MAX_FILE_BYTES as usize) + 1],
    );
    assert!(matches!(
        run(
            &h,
            "alice",
            "browser_download",
            json!({"url": "https://93.184.216.34/huge.pdf"})
        )
        .await,
        Err(SeraError::Validation(_))
    ));
}

#[tokio::test]
async fn forget_logins_wipes_only_the_actors_profile() {
    let h = harness(false);
    let alice = new_user(&h, "ext-fa");
    let bob = new_user(&h, "ext-fb");
    for user in [&alice, &bob] {
        let dir = crate::browser_profiles::profile_dir(&h.base, user, "default").unwrap();
        std::fs::create_dir_all(dir.join("Default")).unwrap();
        std::fs::write(dir.join("Default").join("Cookies"), user.as_bytes()).unwrap();
    }
    open(&h, &alice).await;
    let out = run(&h, &alice, "browser_forget_logins", json!({}))
        .await
        .unwrap();
    assert!(out.content.contains("deleted"));
    let alice_dir = crate::browser_profiles::profile_dir(&h.base, &alice, "default").unwrap();
    let bob_dir = crate::browser_profiles::profile_dir(&h.base, &bob, "default").unwrap();
    assert!(!alice_dir.exists());
    assert_eq!(
        std::fs::read(bob_dir.join("Default").join("Cookies")).unwrap(),
        bob.as_bytes()
    );
    assert!(!h.manager.info(&alice, "default").await.active);
    let again = run(&h, &alice, "browser_forget_logins", json!({}))
        .await
        .unwrap();
    assert!(again.content.contains("no saved logins"));
    let forget = h.registry.get("browser_forget_logins").unwrap();
    assert!(forget.requires_approval(&json!({})));
}

#[tokio::test]
async fn forget_logins_is_refused_while_the_user_drives() {
    let h = harness(false);
    let user = new_user(&h, "ext-fc");
    open(&h, &user).await;
    h.manager
        .set_controller(&user, "default", super::super::manager::Controller::User)
        .await
        .unwrap();
    assert!(
        run(&h, &user, "browser_forget_logins", json!({}))
            .await
            .is_err()
    );
}

#[test]
fn temp_dirs_are_per_user_and_reject_bad_ids() {
    let h = harness(false);
    let a = h.manager.temp_dir("alice").unwrap();
    let b = h.manager.temp_dir("bob").unwrap();
    assert_ne!(a, b);
    assert!(a.starts_with(h.base.join(".tmp")));
    assert!(h.manager.temp_dir("../x").is_err());
}
