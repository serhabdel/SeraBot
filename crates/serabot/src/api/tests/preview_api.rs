//! Artifact previews (0.28.0 C3): `GET /v1/artifacts/{hash}/preview` renders
//! synthetic pdf/pptx/xlsx/docx/md artifacts for the owner and 404s for any
//! other user, exactly like the download endpoint.

use super::*;

fn meta(filename: &str) -> crate::maker::ArtifactMeta {
    crate::maker::ArtifactMeta {
        title: filename.to_owned(),
        filename: filename.to_owned(),
        language: crate::domain::Language::En,
        source_artifact: None,
        created_at: chrono::Utc::now(),
    }
}

/// Synthetic one-page PDF whose content stream draws "Synthetic page one".
fn sample_pdf() -> Vec<u8> {
    use lopdf::{Dictionary, Document, Object, Stream};
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let mut font = Dictionary::new();
    font.set("Type", "Font");
    font.set("Subtype", "Type1");
    font.set("BaseFont", "Helvetica");
    let font_id = document.add_object(font);
    let mut fonts = Dictionary::new();
    fonts.set("F1", Object::Reference(font_id));
    let mut resources = Dictionary::new();
    resources.set("Font", fonts);
    let content = b"BT /F1 12 Tf 20 100 Td (Synthetic page one) Tj ET".to_vec();
    let content_id = document.add_object(Stream::new(Dictionary::new(), content));
    let mut page = Dictionary::new();
    page.set("Type", "Page");
    page.set("Parent", Object::Reference(pages_id));
    page.set("Contents", Object::Reference(content_id));
    page.set("Resources", resources.clone());
    let page_id = document.add_object(page);
    let mut pages = Dictionary::new();
    pages.set("Type", "Pages");
    pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
    pages.set("Count", Object::Integer(1));
    pages.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(200),
            Object::Integer(300),
        ]),
    );
    pages.set("Resources", resources);
    document.set_object(pages_id, pages);
    let mut catalog = Dictionary::new();
    catalog.set("Type", "Catalog");
    catalog.set("Pages", Object::Reference(pages_id));
    let catalog_id = document.add_object(catalog);
    document.trailer.set("Root", Object::Reference(catalog_id));
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn sample_pptx() -> Vec<u8> {
    use crate::deck::{Brand, BrandPalette, DeckSection};
    let brand = Brand {
        name: "Synthetic Brand".into(),
        palette: BrandPalette {
            background: "#FFFFFF".into(),
            foreground: "#111111".into(),
            accent: "#2F6F6A".into(),
            muted: "#6B7280".into(),
        },
        monogram_text: "SB".into(),
        footer: None,
        font_family: Some("helvetica".into()),
    };
    let spec = crate::deck::DeckSpec {
        title: "Synthetic deck".into(),
        subtitle: None,
        language: crate::domain::Language::En,
        date_label: None,
        author: None,
        sections: vec![DeckSection {
            part_title: None,
            title: "Synthetic section".into(),
            cards: vec![],
            bullets: vec!["Synthetic bullet one".into()],
        }],
        footer: None,
    };
    crate::deck_pptx::render_deck_pptx(&spec, &brand, 10)
        .unwrap()
        .bytes
}

fn sample_xlsx() -> Vec<u8> {
    use crate::workbooks::{CellValue, SheetSpec, WorkbookSpec};
    crate::workbooks::build(&WorkbookSpec {
        title: "Synthetic sheet".into(),
        language: None,
        sheets: vec![SheetSpec {
            name: "Sheet1".into(),
            header: vec!["name".into(), "amount".into()],
            rows: vec![vec![
                CellValue::Text("synthetic row".into()),
                CellValue::Number(42.0),
            ]],
        }],
    })
    .unwrap()
}

fn sample_docx() -> Vec<u8> {
    use crate::docx_ops::{DocxBlockSpec, DocxSpec};
    crate::docx_ops::build(&DocxSpec {
        title: "Synthetic doc".into(),
        subtitle: None,
        language: None,
        blocks: vec![
            DocxBlockSpec {
                kind: "heading".into(),
                level: Some(1),
                text: Some("Synthetic heading".into()),
                items: vec![],
                header: vec![],
                rows: vec![],
            },
            DocxBlockSpec {
                kind: "paragraph".into(),
                level: None,
                text: Some("Synthetic paragraph body.".into()),
                items: vec![],
                header: vec![],
                rows: vec![],
            },
        ],
    })
    .unwrap()
}

struct Fixture {
    hash: String,
    filename: &'static str,
}

fn save_fixture(
    artifacts: &Arc<ArtifactStore>,
    store: &Store,
    user_id: &str,
    bytes: Vec<u8>,
    filename: &'static str,
) -> Fixture {
    let stored = artifacts.save(&bytes, filename, meta(filename)).unwrap();
    store
        .record_conversation_artifact(
            user_id,
            "dir:main",
            None,
            "test_tool",
            &stored.hash,
            filename,
            None,
        )
        .unwrap();
    Fixture {
        hash: stored.hash,
        filename,
    }
}

async fn get_preview(app: &Router, token: &str, hash: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            &format!("/v1/artifacts/{hash}/preview"),
            token,
            None,
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = read_json(response).await;
    (status, body)
}

#[tokio::test]
async fn preview_renders_every_supported_type_for_the_owner() {
    let (app, _, store, artifacts) = app_with_artifacts(true, Policy::empty(), temp_base());
    let alice = store
        .create_user("subject-pa", "pa@example.example", "Synthetic A")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "preview-a");
    store
        .create_conversation(&alice.id, "dir:main", "direct", None)
        .unwrap();

    let fixtures = [
        save_fixture(&artifacts, &store, &alice.id, sample_pdf(), "synthetic.pdf"),
        save_fixture(
            &artifacts,
            &store,
            &alice.id,
            sample_pptx(),
            "synthetic.pptx",
        ),
        save_fixture(
            &artifacts,
            &store,
            &alice.id,
            sample_xlsx(),
            "synthetic.xlsx",
        ),
        save_fixture(
            &artifacts,
            &store,
            &alice.id,
            sample_docx(),
            "synthetic.docx",
        ),
        save_fixture(
            &artifacts,
            &store,
            &alice.id,
            b"# Synthetic notes\nhello preview".to_vec(),
            "synthetic.md",
        ),
    ];

    let mut kinds = Vec::new();
    for fixture in &fixtures {
        let (status, body) = get_preview(&app, &alice_token, &fixture.hash).await;
        assert_eq!(status, StatusCode::OK, "{} -> {body}", fixture.filename);
        assert_eq!(body["hash"], fixture.hash);
        assert_eq!(body["filename"], fixture.filename);
        kinds.push(body["kind"].as_str().unwrap().to_owned());
    }
    assert_eq!(kinds, ["pdf", "pptx", "xlsx", "docx", "md"]);

    // Payload spot checks.
    let (status, body) = get_preview(&app, &alice_token, &fixtures[0].hash).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["payload"]["content_type"], "application/json");
    assert_eq!(body["payload"]["preview"]["page_count"], 1);
    assert!(
        body["payload"]["preview"]["text"]
            .as_str()
            .unwrap()
            .contains("Synthetic page one")
    );

    let (_, body) = get_preview(&app, &alice_token, &fixtures[1].hash).await;
    assert!(
        body["payload"]["preview"]
            .as_str()
            .unwrap()
            .contains("--- slide 1 ---")
    );

    let (_, body) = get_preview(&app, &alice_token, &fixtures[2].hash).await;
    assert_eq!(body["payload"]["content_type"], "application/json");
    assert_eq!(body["payload"]["preview"]["sheet"], "Sheet1");
    let rows = body["payload"]["preview"]["rows"].as_array().unwrap();
    assert_eq!(rows[0][0], "name");
    assert_eq!(rows[1][0], "synthetic row");
    assert_eq!(rows[1][1], 42.0);

    let (_, body) = get_preview(&app, &alice_token, &fixtures[3].hash).await;
    let blocks = body["payload"]["preview"]["blocks"].as_array().unwrap();
    // The built document carries its title block plus the two spec blocks.
    assert!(blocks.len() >= 2);
    assert!(blocks.iter().any(|block| {
        block["text"]
            .as_str()
            .unwrap()
            .contains("Synthetic heading")
    }));

    let (_, body) = get_preview(&app, &alice_token, &fixtures[4].hash).await;
    assert_eq!(body["payload"]["content_type"], "text/plain; charset=utf-8");
    assert!(
        body["payload"]["preview"]
            .as_str()
            .unwrap()
            .contains("# Synthetic notes")
    );
}

#[tokio::test]
async fn preview_never_leaks_across_users_and_rejects_unsupported_types() {
    let (app, _, store, artifacts) = app_with_artifacts(true, Policy::empty(), temp_base());
    let alice = store
        .create_user("subject-pb", "pb@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-pc", "pc@example.example", "Synthetic B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "preview-b");
    let bob_token = mint_for(&store, &bob.id, "preview-c");
    store
        .create_conversation(&alice.id, "dir:main", "direct", None)
        .unwrap();

    let md = save_fixture(
        &artifacts,
        &store,
        &alice.id,
        b"# Alice only".to_vec(),
        "alice.md",
    );
    let png = save_fixture(
        &artifacts,
        &store,
        &alice.id,
        vec![0x89, b'P', b'N', b'G', 1, 2, 3],
        "synthetic.png",
    );

    // Another user gets the same 404 as a missing hash.
    let (status, _) = get_preview(&app, &bob_token, &md.hash).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_preview(&app, &alice_token, &"f".repeat(64)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The owner still gets her own preview.
    let (status, _) = get_preview(&app, &alice_token, &md.hash).await;
    assert_eq!(status, StatusCode::OK);
    // A type the server cannot render is a 422, not a crash.
    let (status, _) = get_preview(&app, &alice_token, &png.hash).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Unauthenticated is a 401.
    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            &format!("/v1/artifacts/{}/preview", md.hash),
            "not-a-real-token-but-long-enough-xxx",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
