use super::*;
use serde_json::json;

fn outline() -> Outline {
    let nodes = vec![
        json!({"nodeId":"1","ignored":false,"role":{"value":"RootWebArea"},"name":{"value":"P"},"childIds":["2","3","4","5"]}),
        json!({"nodeId":"2","parentId":"1","ignored":false,"role":{"value":"button"},"name":{"value":"Accept all"},"backendDOMNodeId":11,"childIds":[]}),
        json!({"nodeId":"3","parentId":"1","ignored":false,"role":{"value":"button"},"name":{"value":"Accept"},"backendDOMNodeId":12,"childIds":[]}),
        json!({"nodeId":"4","parentId":"1","ignored":false,"role":{"value":"link"},"name":{"value":"Pricing"},"backendDOMNodeId":13,"childIds":[]}),
        json!({"nodeId":"5","parentId":"1","ignored":false,"role":{"value":"StaticText"},"name":{"value":"Welcome to the shop"},"childIds":[]}),
    ];
    build_outline(&nodes, 10_000, 100)
}

#[test]
fn interactive_text_has_only_actionable_lines() {
    let o = outline();
    assert_eq!(
        o.interactive,
        "[e1] button \"Accept all\"\n[e2] button \"Accept\"\n[e3] link \"Pricing\""
    );
    assert!(o.text.contains("Welcome to the shop"));
    assert_eq!(
        build_outline(&[], 10, 10).interactive,
        "(no interactive elements)"
    );
}

#[test]
fn modes_parse() {
    assert_eq!(SnapshotMode::parse(None).unwrap(), SnapshotMode::Full);
    assert_eq!(
        SnapshotMode::parse(Some("Interactive")).unwrap(),
        SnapshotMode::Interactive
    );
    assert_eq!(
        SnapshotMode::parse(Some("text")).unwrap(),
        SnapshotMode::Text
    );
    assert!(SnapshotMode::parse(Some("x")).is_err());
}

#[test]
fn find_by_role_name_and_text() {
    let o = outline();
    let hits = find_in_outline(&o, Some("button"), Some("accept"), None, 10);
    // The exact name "Accept" ranks before "Accept all"... but only when the
    // wanted name equals it; "accept" is exact for e2.
    assert_eq!(hits[0].reference.as_deref(), Some("e2"));
    assert_eq!(hits[1].reference.as_deref(), Some("e1"));
    assert_eq!(find_in_outline(&o, Some("LINK"), None, None, 10).len(), 1);
    assert!(find_in_outline(&o, Some("slider"), None, None, 10).is_empty());
    let by_text = find_in_outline(&o, None, None, Some("welcome"), 10);
    assert_eq!(by_text.len(), 1);
    assert_eq!(by_text[0].reference, None);
    let link = find_in_outline(&o, None, None, Some("pricing"), 10);
    assert_eq!(link[0].reference.as_deref(), Some("e3"));
    assert_eq!(find_in_outline(&o, Some("button"), None, None, 1).len(), 1);
}
