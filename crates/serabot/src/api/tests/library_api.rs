use super::*;

fn upload_request(token: &str, name: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/library/upload")
        .header("authorization", format!("Bearer {token}"));
    if let Some(name) = name {
        builder = builder.header("x-filename", name);
    }
    builder.body(Body::from(body)).unwrap()
}

async fn get(app: &Router, token: &str, uri: &str) -> Response {
    app.clone()
        .oneshot(request_with_token("GET", uri, token, None))
        .await
        .unwrap()
}

#[tokio::test]
async fn library_lists_uploads_and_generated_items_per_user() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-la", "la@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-lb", "lb@example.example", "Synthetic B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "lib-a");
    let bob_token = mint_for(&store, &bob.id, "lib-b");

    // A bot-made artifact in Alice's conversation shows up as `generated`,
    // with the conversation title.
    store
        .create_conversation_titled(&alice.id, "dir:main", "direct", None, Some("Main chat"))
        .unwrap();
    store
        .record_conversation_artifact(
            &alice.id,
            "dir:main",
            None,
            "deck_build",
            &"a".repeat(64),
            "deck.pdf",
            Some("Quarterly deck"),
        )
        .unwrap();

    // Upload validation: missing header and empty body are 400s, oversized 413.
    let response = app
        .clone()
        .oneshot(upload_request(&alice_token, None, b"hello".to_vec()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .clone()
        .oneshot(upload_request(&alice_token, Some("empty.txt"), Vec::new()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .clone()
        .oneshot(upload_request(
            &alice_token,
            Some("big.bin"),
            vec![b'a'; crate::maker::MAX_ARTIFACT_BYTES + 1],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // A real upload lands in the artifact store and the library list.
    let response = app
        .clone()
        .oneshot(upload_request(
            &alice_token,
            Some("brand notes.md"),
            b"# Synthetic brand notes".to_vec(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let item = &body["item"];
    assert_eq!(item["kind"], "uploaded");
    assert_eq!(item["filename"], "brand notes.md");
    assert_eq!(item["mime"], "text/plain; charset=utf-8");
    assert_eq!(item["size_bytes"], 23);
    let hash = item["id"].as_str().unwrap().to_owned();
    // OneDrive is not set up in tests: the mirror reports it, the upload still succeeds.
    assert_eq!(body["onedrive"], "OneDrive is not set up on this server");

    let list = read_json(get(&app, &alice_token, "/v1/library").await).await;
    assert_eq!(list["enabled"], true);
    assert_eq!(list["onedrive"]["mirror"], true);
    assert_eq!(list["onedrive"]["connected"], false);
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    let uploaded = items.iter().find(|i| i["kind"] == "uploaded").unwrap();
    assert_eq!(uploaded["id"], hash);
    assert_eq!(uploaded["tool"], "library_upload");
    let generated = items.iter().find(|i| i["kind"] == "generated").unwrap();
    assert_eq!(generated["tool"], "deck_build");
    assert_eq!(generated["conversation_title"], "Main chat");

    // The uploaded artifact is downloadable by its owner through the existing
    // per-user artifact endpoint, and invisible to another member.
    let own = get(&app, &alice_token, &format!("/v1/artifacts/{hash}")).await;
    assert_eq!(own.status(), StatusCode::OK);
    let other = get(&app, &bob_token, &format!("/v1/artifacts/{hash}")).await;
    assert_eq!(other.status(), StatusCode::NOT_FOUND);

    // Bob's library is empty; Alice's items never leak into it.
    let bob_list = read_json(get(&app, &bob_token, "/v1/library").await).await;
    assert_eq!(bob_list["items"].as_array().unwrap().len(), 0);

    // The upload is audited under the acting user.
    let audit = store.list_audit(10).unwrap();
    assert!(
        audit
            .iter()
            .any(|entry| entry.actor == alice.id && entry.event == "library.upload")
    );
}

#[test]
fn library_items_are_scoped_in_the_store() {
    let root = temp_base();
    let (_, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-sa", "sa@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-sb", "sb@example.example", "Synthetic B")
        .unwrap();
    store
        .create_conversation(&alice.id, "dir:main", "direct", None)
        .unwrap();
    store
        .record_conversation_artifact(
            &alice.id,
            "dir:main",
            None,
            "deck_build",
            &"b".repeat(64),
            "deck.pdf",
            None,
        )
        .unwrap();
    store
        .record_conversation_artifact(
            &alice.id,
            crate::library::LIBRARY_SCOPE,
            None,
            crate::library::LIBRARY_UPLOAD_TOOL,
            &"c".repeat(64),
            "notes.md",
            None,
        )
        .unwrap();

    let items = store.list_library_items(&alice.id, 100).unwrap();
    assert_eq!(items.len(), 2);
    let kinds: std::collections::HashSet<&str> =
        items.iter().map(|item| item.kind.as_str()).collect();
    assert!(kinds.contains("generated") && kinds.contains("uploaded"));
    assert!(store.list_library_items(&bob.id, 100).unwrap().is_empty());
}

async fn upload_bytes(app: &Router, token: &str, filename: &str, bytes: Vec<u8>) -> Value {
    let response = app
        .clone()
        .oneshot(upload_request(token, Some(filename), bytes))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{filename}");
    read_json(response).await
}

async fn read_bytes(response: Response) -> Vec<u8> {
    http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

fn sample_deck_pptx() -> Vec<u8> {
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

/// The sample deck with the first text run on slide 1 replaced by 5000
/// characters, so the per-slide cap can be checked end to end. The zip
/// surgery keeps every other part byte-identical to the rendered deck.
fn pptx_with_long_slide_text() -> Vec<u8> {
    use std::io::{Cursor, Read, Write};
    use zip::write::SimpleFileOptions;
    let deck = sample_deck_pptx();
    let mut archive = zip::ZipArchive::new(Cursor::new(deck)).unwrap();
    let mut slide_xml = String::new();
    archive
        .by_name("ppt/slides/slide1.xml")
        .unwrap()
        .read_to_string(&mut slide_xml)
        .unwrap();
    let start = slide_xml.find("<a:t>").unwrap() + "<a:t>".len();
    let end = slide_xml[start..].find("</a:t>").unwrap() + start;
    slide_xml.replace_range(start..end, &"x".repeat(5000));

    let mut out = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut out);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().to_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            writer.start_file(name.clone(), options).unwrap();
            if name == "ppt/slides/slide1.xml" {
                writer.write_all(slide_xml.as_bytes()).unwrap();
            } else {
                writer.write_all(&bytes).unwrap();
            }
        }
        writer.finish().unwrap();
    }
    out.into_inner()
}

fn sample_workbook_xlsx() -> Vec<u8> {
    use crate::workbooks::{CellValue, SheetSpec, WorkbookSpec};
    crate::workbooks::build(&WorkbookSpec {
        title: "Synthetic sheet".into(),
        language: None,
        sheets: vec![
            SheetSpec {
                name: "Data".into(),
                header: vec!["item".into(), "qty".into()],
                rows: vec![vec![
                    CellValue::Text("widgets".into()),
                    CellValue::Number(5.0),
                ]],
            },
            SheetSpec {
                name: "Notes".into(),
                header: vec![],
                rows: vec![vec![CellValue::Text("a note".into())]],
            },
        ],
    })
    .unwrap()
}

/// Six sheets; the first has a 500-character cell and 25 data rows, so the
/// sheet, row and cell caps can all be checked in one upload.
fn big_workbook_xlsx() -> Vec<u8> {
    use crate::workbooks::{CellValue, SheetSpec, WorkbookSpec};
    let mut sheets = vec![SheetSpec {
        name: "Wide".into(),
        header: vec!["label".into()],
        rows: (0..25)
            .map(|n| {
                if n == 0 {
                    vec![CellValue::Text("y".repeat(500))]
                } else {
                    vec![CellValue::Text(format!("row {n}"))]
                }
            })
            .collect(),
    }];
    for n in 1..=5 {
        sheets.push(SheetSpec {
            name: format!("Extra{n}"),
            header: vec![],
            rows: vec![vec![CellValue::Text("x".into())]],
        });
    }
    crate::workbooks::build(&WorkbookSpec {
        title: "Big workbook".into(),
        language: None,
        sheets,
    })
    .unwrap()
}

fn test_png(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbImage::new(width, height);
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn test_png_rgba(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbaImage::new(width, height);
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

#[tokio::test]
async fn preview_doc_returns_pptx_slide_titles_and_text() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-pd-a", "pda@example.example", "Synthetic A")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "pd-a");

    let uploaded = upload_bytes(&app, &alice_token, "slides.pptx", sample_deck_pptx()).await;
    let hash = uploaded["item"]["id"].as_str().unwrap().to_owned();

    let body = read_json(
        get(
            &app,
            &alice_token,
            &format!("/v1/library/preview-doc?hash={hash}&kind=pptx"),
        )
        .await,
    )
    .await;
    assert_eq!(body["kind"], "pptx");
    assert_eq!(body["filename"], "slides.pptx");
    assert_eq!(body["hash"], hash);
    let slide_count = body["slide_count"].as_u64().unwrap();
    assert!(slide_count >= 2, "{body}");
    let slides = body["slides"].as_array().unwrap();
    assert_eq!(slides.len() as u64, slide_count);
    assert!(
        slides
            .iter()
            .all(|s| !s["title"].as_str().unwrap().is_empty()),
        "{body}"
    );
    assert!(
        slides
            .iter()
            .any(|s| s["text"].as_str().unwrap().contains("Synthetic section")),
        "{body}"
    );
    assert!(
        slides
            .iter()
            .any(|s| s["text"].as_str().unwrap().contains("Synthetic bullet one")),
        "{body}"
    );
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn preview_doc_returns_xlsx_sheet_names_and_first_rows() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-pd-b", "pdb@example.example", "Synthetic B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "pd-b");

    let uploaded = upload_bytes(&app, &alice_token, "sheet.xlsx", sample_workbook_xlsx()).await;
    let hash = uploaded["item"]["id"].as_str().unwrap().to_owned();

    let body = read_json(
        get(
            &app,
            &alice_token,
            &format!("/v1/library/preview-doc?hash={hash}&kind=xlsx"),
        )
        .await,
    )
    .await;
    assert_eq!(body["kind"], "xlsx");
    assert_eq!(body["filename"], "sheet.xlsx");
    assert_eq!(body["sheet_count"], 2);
    let sheets = body["sheets"].as_array().unwrap();
    assert_eq!(sheets[0]["name"], "Data");
    assert_eq!(sheets[0]["range"], "A1:B2");
    assert_eq!(sheets[0]["rows"][0], json!(["item", "qty"]));
    assert_eq!(sheets[0]["rows"][1], json!(["widgets", 5.0]));
    assert_eq!(sheets[0]["truncated"], false);
    assert_eq!(sheets[1]["name"], "Notes");
    assert_eq!(sheets[1]["rows"][0], json!(["a note"]));
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn preview_doc_rejects_foreign_hashes_and_bad_requests() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-pd-c", "pdc@example.example", "Synthetic C")
        .unwrap();
    let bob = store
        .create_user("subject-pd-d", "pdd@example.example", "Synthetic D")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "pd-c");
    let bob_token = mint_for(&store, &bob.id, "pd-d");

    let uploaded = upload_bytes(&app, &alice_token, "slides.pptx", sample_deck_pptx()).await;
    let hash = uploaded["item"]["id"].as_str().unwrap().to_owned();

    // Another member's hash looks missing.
    let response = get(
        &app,
        &bob_token,
        &format!("/v1/library/preview-doc?hash={hash}&kind=pptx"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    // A well-formed hash nobody owns is a 404 too.
    let response = get(
        &app,
        &alice_token,
        &format!("/v1/library/preview-doc?hash={}&kind=pptx", "d".repeat(64)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    // Unknown kind, malformed hash, missing kind, and a kind that does not
    // match the file are all 400s.
    for uri in [
        format!("/v1/library/preview-doc?hash={hash}&kind=pdf"),
        "/v1/library/preview-doc?hash=xyz&kind=pptx".into(),
        format!("/v1/library/preview-doc?hash={hash}"),
        format!("/v1/library/preview-doc?hash={hash}&kind=xlsx"),
    ] {
        let response = get(&app, &alice_token, &uri).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
}

#[tokio::test]
async fn preview_doc_caps_long_text_many_rows_and_many_sheets() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-pd-e", "pde@example.example", "Synthetic E")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "pd-e");

    let uploaded = upload_bytes(&app, &alice_token, "long.pptx", pptx_with_long_slide_text()).await;
    let hash = uploaded["item"]["id"].as_str().unwrap().to_owned();
    let body = read_json(
        get(
            &app,
            &alice_token,
            &format!("/v1/library/preview-doc?hash={hash}&kind=pptx"),
        )
        .await,
    )
    .await;
    let first = &body["slides"][0];
    assert_eq!(first["text"].as_str().unwrap().chars().count(), 2000);
    assert_eq!(first["truncated"], true);
    assert_eq!(body["truncated"], true);
    // The parser caps titles at 200 characters.
    assert!(
        first["title"].as_str().unwrap().chars().count() <= 200,
        "{}",
        first["title"]
    );

    let uploaded = upload_bytes(&app, &alice_token, "big.xlsx", big_workbook_xlsx()).await;
    let hash = uploaded["item"]["id"].as_str().unwrap().to_owned();
    let body = read_json(
        get(
            &app,
            &alice_token,
            &format!("/v1/library/preview-doc?hash={hash}&kind=xlsx"),
        )
        .await,
    )
    .await;
    assert_eq!(body["sheet_count"], 6);
    let sheets = body["sheets"].as_array().unwrap();
    assert_eq!(sheets.len(), 5);
    assert_eq!(body["truncated"], true);
    let wide = sheets
        .iter()
        .find(|sheet| sheet["name"] == "Wide")
        .expect("Wide sheet");
    assert_eq!(wide["rows"].as_array().unwrap().len(), 20);
    assert_eq!(wide["truncated"], true);
    // Row 0 is the header; the 500-character cell is the first data row.
    let long_cell = wide["rows"][1][0].as_str().unwrap();
    assert_eq!(long_cell.chars().count(), 200);
    assert!(long_cell.chars().all(|c| c == 'y'));
}

#[tokio::test]
async fn thumbnail_downscales_images_and_is_uid_scoped() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-th-a", "tha@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-th-b", "thb@example.example", "Synthetic B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "th-a");
    let bob_token = mint_for(&store, &bob.id, "th-b");

    let uploaded = upload_bytes(&app, &alice_token, "photo.png", test_png(800, 400)).await;
    let hash = uploaded["item"]["id"].as_str().unwrap().to_owned();

    // Opaque pictures downscale to JPEG at the requested width.
    let response = get(
        &app,
        &alice_token,
        &format!("/v1/library/thumbnail?hash={hash}&w=320"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "image/jpeg"
    );
    let cache = response
        .headers()
        .get("cache-control")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        cache.contains("private") && cache.contains("max-age=86400"),
        "{cache}"
    );
    let bytes = read_bytes(response).await;
    let thumb = image::load_from_memory(&bytes).unwrap();
    assert_eq!(thumb.width(), 320);
    assert_eq!(thumb.height(), 160);

    // The default width is 320.
    let response = get(
        &app,
        &alice_token,
        &format!("/v1/library/thumbnail?hash={hash}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = read_bytes(response).await;
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 320);

    // Pictures with an alpha channel stay PNG.
    let uploaded = upload_bytes(&app, &alice_token, "alpha.png", test_png_rgba(800, 400)).await;
    let alpha_hash = uploaded["item"]["id"].as_str().unwrap().to_owned();
    let response = get(
        &app,
        &alice_token,
        &format!("/v1/library/thumbnail?hash={alpha_hash}&w=320"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").unwrap(), "image/png");
    let bytes = read_bytes(response).await;
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 320);

    // A picture that already fits is served untouched.
    let uploaded = upload_bytes(&app, &alice_token, "small.png", test_png(100, 50)).await;
    let small_hash = uploaded["item"]["id"].as_str().unwrap().to_owned();
    let response = get(
        &app,
        &alice_token,
        &format!("/v1/library/thumbnail?hash={small_hash}&w=320"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").unwrap(), "image/png");
    let bytes = read_bytes(response).await;
    let small = image::load_from_memory(&bytes).unwrap();
    assert_eq!((small.width(), small.height()), (100, 50));

    // Another member's hash is a 404; a malformed hash, a non-numeric width
    // and a non-image file are 400s.
    let response = get(
        &app,
        &bob_token,
        &format!("/v1/library/thumbnail?hash={hash}&w=320"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    for uri in [
        "/v1/library/thumbnail?hash=xyz&w=320".to_string(),
        format!("/v1/library/thumbnail?hash={hash}&w=wide"),
    ] {
        let response = get(&app, &alice_token, &uri).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
    let uploaded = upload_bytes(&app, &alice_token, "notes.md", b"# notes".to_vec()).await;
    let md_hash = uploaded["item"]["id"].as_str().unwrap().to_owned();
    let response = get(
        &app,
        &alice_token,
        &format!("/v1/library/thumbnail?hash={md_hash}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
