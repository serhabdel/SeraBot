//! The Chromium actions against a scripted DevTools server (no Chromium).

use super::*;
use crate::browser::manager::BrowserBackend;
use crate::browser::testing::{CdpLog, scripted_cdp};
use std::sync::{Arc, Mutex};

/// What the scripted page looks like.
#[derive(Clone, Default)]
struct Page {
    detached: bool,
    disabled: bool,
    readonly: bool,
    covered_by: Option<Value>,
    tag: &'static str,
    input_type: &'static str,
    zero_box: bool,
    checked: bool,
    /// Events to emit after a mouse release, and the file to write first.
    on_release: Vec<Value>,
    download_file: Option<Vec<u8>>,
}

fn probe_json(page: &Page, point: bool) -> String {
    let mut value = json!({
        "attached": true, "display_none": false, "hidden": false,
        "disabled": page.disabled, "aria_disabled": false, "readonly": page.readonly,
        "tag": if page.tag.is_empty() { "button" } else { page.tag },
        "input_type": page.input_type, "covered": false,
    });
    if point && let Some(cover) = &page.covered_by {
        value["covered"] = json!(true);
        value["cover"] = cover.clone();
    }
    value.to_string()
}

async fn backend_with(page: Page) -> (ChromiumBackend, CdpLog, std::path::PathBuf) {
    let state = Arc::new(Mutex::new(page));
    let download_path: Arc<Mutex<String>> = Arc::default();
    let handler_state = state.clone();
    let (cdp, log) = scripted_cdp(move |method, params| {
        let page = handler_state.lock().unwrap();
        let mut events = Vec::new();
        let result = match method {
            "DOM.resolveNode" if page.detached => json!({"__error": "No node with given id"}),
            "DOM.resolveNode" => json!({"object": {"objectId": "O1"}}),
            "DOM.getBoxModel" if page.zero_box => {
                json!({"model": {"content": [5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 5.0]}})
            }
            "DOM.getBoxModel" => {
                json!({"model": {"content": [10.0, 10.0, 30.0, 10.0, 30.0, 20.0, 10.0, 20.0]}})
            }
            "Runtime.callFunctionOn" => {
                let function = params["functionDeclaration"].as_str().unwrap_or_default();
                let value = if function.contains("attached:") {
                    probe_json(&page, !params["arguments"][0]["value"].is_null())
                } else if function.contains("checkable") {
                    json!({"checkable": true, "checked": page.checked, "radio": false}).to_string()
                } else if function.contains("not_select") {
                    if params["arguments"][0]["value"] == "Nowhere" {
                        json!({"error": "no_option", "options": ["Paris", "Lyon"]}).to_string()
                    } else {
                        json!({"label": "Paris"}).to_string()
                    }
                } else {
                    "null".to_owned()
                };
                json!({"result": {"type": "string", "value": value}})
            }
            "Browser.setDownloadBehavior" => {
                if let Some(path) = params["downloadPath"].as_str() {
                    *download_path.lock().unwrap() = path.to_owned();
                }
                json!({})
            }
            "Input.dispatchMouseEvent" => {
                if params["type"] == "mouseReleased" {
                    if let Some(bytes) = page.download_file.clone() {
                        let dir = download_path.lock().unwrap().clone();
                        if !dir.is_empty() {
                            std::fs::write(Path::new(&dir).join("guid-1"), bytes).unwrap();
                        }
                    }
                    events.extend(page.on_release.clone());
                }
                json!({})
            }
            "Page.printToPDF" => {
                use base64::Engine;
                json!({"data": base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.4 test")})
            }
            _ => json!({}),
        };
        (result, events)
    })
    .await;
    let root = std::env::temp_dir().join(format!("serabot-actions-{}", uuid::Uuid::new_v4()));
    let backend = ChromiumBackend::for_test(cdp, root.clone());
    backend.set_refs(vec![RefTarget {
        backend_node_id: 42,
        role: "button".into(),
        name: "Pay".into(),
    }]);
    (backend, log, root)
}

fn textbox(backend: &ChromiumBackend) {
    backend.set_refs(vec![RefTarget {
        backend_node_id: 1,
        role: "textbox".into(),
        name: "Email".into(),
    }]);
}

fn methods(log: &CdpLog) -> Vec<String> {
    log.lock().unwrap().iter().map(|(m, _)| m.clone()).collect()
}

#[tokio::test]
async fn a_ready_button_is_clicked_at_the_centre_of_its_box() {
    let (backend, log, _) = backend_with(Page::default()).await;
    backend.click_ref("e1").await.unwrap();
    let calls = log.lock().unwrap().clone();
    let press = calls
        .iter()
        .find(|(m, p)| m == "Input.dispatchMouseEvent" && p["type"] == "mousePressed")
        .expect("clicked");
    assert_eq!(press.1["x"], 20.0);
    assert_eq!(press.1["y"], 15.0);
    // The point test ran at the click point, before the click.
    assert!(
        calls
            .iter()
            .any(|(m, p)| m == "Runtime.callFunctionOn" && p["arguments"][0]["value"] == 20.0)
    );
}

#[tokio::test]
async fn a_covered_button_fails_with_the_cover_named_and_is_not_clicked() {
    let page = Page {
        covered_by: Some(
            json!({"tag": "div", "id": "onetrust-banner-sdk", "text": "We use cookies"}),
        ),
        ..Default::default()
    };
    let (backend, log, _) = backend_with(page).await;
    let error = backend.click_ref("e1").await.unwrap_err().to_string();
    assert!(
        error.contains("the button is covered by a cookie banner"),
        "{error}"
    );
    assert!(!methods(&log).contains(&"Input.dispatchMouseEvent".to_owned()));
}

#[tokio::test]
async fn disabled_detached_and_zero_size_elements_fail_clearly() {
    let (backend, log, _) = backend_with(Page {
        disabled: true,
        ..Default::default()
    })
    .await;
    let error = backend.click_ref("e1").await.unwrap_err().to_string();
    assert!(error.contains("the button is disabled"), "{error}");
    // Refused before scrolling or measuring.
    assert!(!methods(&log).contains(&"DOM.getBoxModel".to_owned()));

    let (backend, _, _) = backend_with(Page {
        detached: true,
        ..Default::default()
    })
    .await;
    let error = backend.click_ref("e1").await.unwrap_err().to_string();
    assert!(error.contains("no longer on the page"), "{error}");

    let (backend, log, _) = backend_with(Page {
        zero_box: true,
        ..Default::default()
    })
    .await;
    let error = backend.click_ref("e1").await.unwrap_err().to_string();
    assert!(error.contains("not visible"), "{error}");
    assert!(!methods(&log).contains(&"Input.dispatchMouseEvent".to_owned()));
}

#[tokio::test]
async fn typing_into_a_disabled_or_read_only_field_fails_before_any_input() {
    let (backend, log, _) = backend_with(Page {
        disabled: true,
        tag: "input",
        ..Default::default()
    })
    .await;
    textbox(&backend);
    let error = backend
        .type_ref("e1", "a@b.c", false)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.ends_with("the field is disabled"), "{error}");
    let (backend, log2, _) = backend_with(Page {
        readonly: true,
        tag: "input",
        ..Default::default()
    })
    .await;
    textbox(&backend);
    let error = backend
        .type_ref("e1", "x", false)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.ends_with("the field is read-only"), "{error}");
    for log in [log, log2] {
        assert!(!methods(&log).contains(&"Input.insertText".to_owned()));
        assert!(!methods(&log).contains(&"DOM.focus".to_owned()));
    }
}

#[tokio::test]
async fn typing_works_when_a_banner_covers_the_field() {
    let page = Page {
        tag: "input",
        covered_by: Some(json!({"tag": "div", "id": "cookie-banner"})),
        ..Default::default()
    };
    let (backend, log, _) = backend_with(page).await;
    textbox(&backend);
    backend.type_ref("e1", "hello", false).await.unwrap();
    assert!(methods(&log).contains(&"Input.insertText".to_owned()));
}

#[tokio::test]
async fn hover_moves_without_pressing_and_respects_cover() {
    let (backend, log, _) = backend_with(Page::default()).await;
    backend.do_hover("e1").await.unwrap();
    let calls = log.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|(m, p)| m == "Input.dispatchMouseEvent" && p["type"] == "mouseMoved")
    );
    assert!(!calls.iter().any(|(_, p)| p["type"] == "mousePressed"));
    assert_eq!(backend.pointer().unwrap().action, "hover");
    let covered = Page {
        covered_by: Some(json!({"tag": "div", "role": "dialog"})),
        ..Default::default()
    };
    let (backend, _, _) = backend_with(covered).await;
    let error = backend.do_hover("e1").await.unwrap_err().to_string();
    assert!(error.contains("popup or dialog"), "{error}");
}

#[tokio::test]
async fn select_reports_the_label_or_the_available_options() {
    let (backend, _, _) = backend_with(Page {
        tag: "select",
        ..Default::default()
    })
    .await;
    backend.set_refs(vec![RefTarget {
        backend_node_id: 1,
        role: "combobox".into(),
        name: "City".into(),
    }]);
    assert_eq!(backend.do_select("e1", "paris").await.unwrap(), "Paris");
    let error = backend
        .do_select("e1", "Nowhere")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("no option matches") && error.contains("Paris | Lyon"),
        "{error}"
    );
}

#[tokio::test]
async fn check_clicks_only_when_the_state_differs_and_verifies() {
    let page = Page {
        tag: "input",
        input_type: "checkbox",
        ..Default::default()
    };
    let (backend, log, _) = backend_with(page).await;
    backend.set_refs(vec![RefTarget {
        backend_node_id: 1,
        role: "checkbox".into(),
        name: "Agree".into(),
    }]);
    // Already unchecked: nothing to do.
    assert!(!backend.do_set_checked("e1", false).await.unwrap());
    assert!(!methods(&log).contains(&"Input.dispatchMouseEvent".to_owned()));
    // The scripted page does not toggle on a click, so verification fails.
    let error = backend
        .do_set_checked("e1", true)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("did not change"), "{error}");
    assert!(methods(&log).contains(&"Input.dispatchMouseEvent".to_owned()));
}

#[tokio::test]
async fn upload_needs_a_file_input() {
    let page = Page {
        tag: "input",
        input_type: "file",
        ..Default::default()
    };
    let (backend, log, _) = backend_with(page).await;
    backend
        .do_upload("e1", Path::new("/tmp/x/report.pdf"))
        .await
        .unwrap();
    let calls = log.lock().unwrap().clone();
    let set = calls
        .iter()
        .find(|(m, _)| m == "DOM.setFileInputFiles")
        .unwrap();
    assert_eq!(set.1["files"][0], "/tmp/x/report.pdf");
    assert_eq!(set.1["backendNodeId"], 42);
    let (backend, log, _) = backend_with(Page {
        tag: "input",
        input_type: "text",
        ..Default::default()
    })
    .await;
    let error = backend
        .do_upload("e1", Path::new("/tmp/x"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a file input"), "{error}");
    assert!(!methods(&log).contains(&"DOM.setFileInputFiles".to_owned()));
}

fn begin(url: &str) -> Value {
    json!({"method": "Browser.downloadWillBegin", "params": {"guid": "guid-1", "url": url, "suggestedFilename": "../report.csv"}})
}

fn progress(state: &str, received: u64, total: u64) -> Value {
    json!({"method": "Browser.downloadProgress", "params": {"guid": "guid-1", "state": state, "receivedBytes": received, "totalBytes": total}})
}

#[tokio::test]
async fn a_download_is_read_then_deleted_and_downloads_are_refused_again() {
    let page = Page {
        download_file: Some(b"a,b\n1,2\n".to_vec()),
        on_release: vec![
            begin("https://93.184.216.34/r.csv"),
            progress("completed", 8, 8),
        ],
        ..Default::default()
    };
    let (backend, log, root) = backend_with(page).await;
    let file = backend
        .do_download("e1", Duration::from_secs(5), 1024)
        .await
        .unwrap();
    assert_eq!(file.filename, "report.csv");
    assert_eq!(file.bytes, b"a,b\n1,2\n");
    let calls = log.lock().unwrap().clone();
    let behaviours: Vec<_> = calls
        .iter()
        .filter(|(m, _)| m == "Browser.setDownloadBehavior")
        .map(|(_, p)| p["behavior"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(behaviours, vec!["allowAndName", "deny"]);
    // The per-download folder is gone.
    assert_eq!(std::fs::read_dir(&root).map(|d| d.count()).unwrap_or(0), 0);
}

#[tokio::test]
async fn an_oversized_or_internal_download_is_cancelled() {
    let big = Page {
        on_release: vec![
            begin("https://93.184.216.34/big.bin"),
            progress("inProgress", 10, 5_000_000),
        ],
        ..Default::default()
    };
    let (backend, log, _) = backend_with(big).await;
    let error = backend
        .do_download("e1", Duration::from_secs(5), 1_000_000)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("larger than"), "{error}");
    assert!(methods(&log).contains(&"Browser.cancelDownload".to_owned()));

    let internal = Page {
        on_release: vec![
            begin("http://169.254.169.254/latest"),
            progress("completed", 1, 1),
        ],
        ..Default::default()
    };
    let (backend, log, _) = backend_with(internal).await;
    let error = backend
        .do_download("e1", Duration::from_secs(5), 1_000_000)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("non-public"), "{error}");
    assert!(methods(&log).contains(&"Browser.cancelDownload".to_owned()));
}

#[tokio::test]
async fn a_click_that_starts_no_download_times_out_cleanly() {
    let (backend, log, root) = backend_with(Page::default()).await;
    let error = backend
        .do_download("e1", Duration::from_millis(300), 1024)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no download"), "{error}");
    assert_eq!(
        methods(&log).last().map(String::as_str),
        Some("Browser.setDownloadBehavior")
    );
    assert_eq!(std::fs::read_dir(&root).map(|d| d.count()).unwrap_or(0), 0);
}

#[tokio::test]
async fn pdf_is_decoded_and_size_capped() {
    let (backend, _, _) = backend_with(Page::default()).await;
    let bytes = backend.do_pdf(1024).await.unwrap();
    assert!(bytes.starts_with(b"%PDF"));
    assert!(backend.do_pdf(4).await.is_err());
}
