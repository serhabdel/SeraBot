//! Tests that drive the real mail client against [`FakeImap`], plus an adversarial
//! corpus for the HTML sanitizer and the header parsers.

use crate::mail::{
    build_draft_message, imap_delete, imap_flag, imap_folders, imap_list, imap_move,
    imap_save_draft, imap_search, parse_message_detail,
};
use crate::mail_test_support::{FakeImap, FakeMessage};
use crate::mail_ux;

fn inbox() -> Vec<FakeMessage> {
    vec![
        FakeMessage::new(1, "Plan", "Anna <anna@example.example>", "Let us plan.").seen(),
        FakeMessage::new(2, "Invoice 42", "Bob <bob@example.example>", "Please pay."),
        FakeMessage::new(3, "Lunch", "Carol <carol@example.example>", "Noon?"),
    ]
}

#[tokio::test]
async fn list_search_flag_move_and_delete_against_a_dovecot_like_server() {
    let server = FakeImap::start(&["Archive", "INBOX.Drafts"], inbox()).await;
    let creds = server.credentials();

    let listed = imap_list(&creds, "INBOX", 10).await.unwrap();
    let uids: Vec<u32> = listed.iter().map(|m| m.uid).collect();
    assert_eq!(uids, vec![3, 2, 1], "newest first");
    assert!(listed[2].flags.iter().any(|f| f == "Seen"));
    assert_eq!(listed[1].snippet, "Please pay.");

    let from_anna = imap_search(&creds, "INBOX", "from:anna", 10).await.unwrap();
    assert_eq!(from_anna.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![1]);
    let unread = imap_search(&creds, "INBOX", "is:unread", 10).await.unwrap();
    assert_eq!(unread.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![3, 2]);
    let words = imap_search(&creds, "INBOX", "invoice", 10).await.unwrap();
    assert_eq!(words.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![2]);
    assert!(
        server
            .commands()
            .iter()
            .any(|c| c.contains("UID SEARCH FROM \"anna\"")),
        "{:?}",
        server.commands()
    );

    imap_flag(&creds, "INBOX", 2, Some(true), Some(true))
        .await
        .unwrap();
    let flags = server.messages("INBOX")[1].flags.clone();
    assert!(flags.contains(&"Seen".to_owned()) && flags.contains(&"Flagged".to_owned()));
    imap_flag(&creds, "INBOX", 2, Some(false), None)
        .await
        .unwrap();
    assert!(
        !server.messages("INBOX")[1]
            .flags
            .contains(&"Seen".to_owned())
    );

    imap_move(&creds, "INBOX", 3, "Archive").await.unwrap();
    assert_eq!(server.messages("INBOX").len(), 2);
    assert_eq!(server.messages("Archive").len(), 1);
    assert!(imap_move(&creds, "INBOX", 2, "Nowhere").await.is_err());

    imap_delete(&creds, "INBOX", 1).await.unwrap();
    assert_eq!(
        server
            .messages("INBOX")
            .iter()
            .map(|m| m.uid)
            .collect::<Vec<_>>(),
        vec![2]
    );
}

#[tokio::test]
async fn folders_are_found_and_drafts_are_appended_then_replaced() {
    let server = FakeImap::start(&["INBOX.Drafts", "INBOX.Sent"], Vec::new()).await;
    let creds = server.credentials();

    let folders = imap_folders(&creds).await.unwrap();
    assert_eq!(folders, vec!["INBOX", "INBOX.Drafts", "INBOX.Sent"]);
    let drafts = mail_ux::pick_folder(&folders, "drafts").unwrap().to_owned();
    assert_eq!(drafts, "INBOX.Drafts");

    let to = vec!["bob@example.example".to_owned()];
    let raw = build_draft_message("me@example.example", &to, &[], "First", "Hello", None).unwrap();
    let first = imap_save_draft(&creds, &drafts, &raw, None)
        .await
        .unwrap()
        .expect("APPENDUID");
    assert_eq!(server.messages(&drafts).len(), 1);
    assert!(
        server.messages(&drafts)[0]
            .flags
            .contains(&"Draft".to_owned())
    );

    let raw = build_draft_message(
        "me@example.example",
        &to,
        &[],
        "Second",
        "Hello again",
        None,
    )
    .unwrap();
    let second = imap_save_draft(&creds, &drafts, &raw, Some(first))
        .await
        .unwrap()
        .expect("APPENDUID");
    assert_ne!(first, second);
    let stored = server.messages(&drafts);
    assert_eq!(stored.len(), 1, "the old autosave is replaced, not kept");
    assert!(stored[0].raw.contains("Subject: Second"));

    assert!(
        imap_save_draft(&creds, "Missing", &raw, None)
            .await
            .is_err(),
        "a server that refuses the draft is an error"
    );
}

#[test]
fn a_draft_without_recipients_has_no_to_header_and_cannot_inject_headers() {
    let raw = build_draft_message(
        "me@example.example",
        &[],
        &[],
        "Hi\r\nBcc: evil@example.example",
        "body",
        None,
    )
    .unwrap();
    let text = String::from_utf8(raw).unwrap();
    let head = text.split("\r\n\r\n").next().unwrap();
    assert!(
        !head
            .lines()
            .any(|l| l.to_ascii_lowercase().starts_with("to:")),
        "{head}"
    );
    assert!(
        !head
            .lines()
            .any(|l| l.to_ascii_lowercase().starts_with("bcc:")),
        "{head}"
    );
    assert!(head.lines().any(|l| l.starts_with("Subject: ")));
}

// ------------------------------------------------------- sanitizer corpus ----

fn html_message(html: &str) -> String {
    format!(
        "From: a@example.example\r\nSubject: s\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n"
    )
}

/// Things that must never survive in sanitized HTML.
const FORBIDDEN: &[&str] = &[
    "<script",
    "javascript:",
    "vbscript:",
    "onerror",
    "onload",
    "onclick",
    "onmouseover",
    "<iframe",
    "<object",
    "<embed",
    "<form",
    "<input",
    "<meta",
    "<link",
    "<base",
    "<svg",
    "<math",
    "<style",
    "<template",
    "<noscript",
    "expression(",
    "data:text/html",
    "srcdoc",
    "evil.example",
];

#[test]
fn hostile_html_is_neutralised() {
    let payloads = [
        "<script>alert(1)</script><p>hi</p>",
        "<img src=x onerror=alert(1)>",
        "<a href=\"javascript:alert(1)\">x</a>",
        "<a href=\"JaVa\tScRiPt:alert(1)\">x</a>",
        "<a href=\"&#106;avascript:alert(1)\">x</a>",
        "<a href=\" javascript:alert(1)\">x</a>",
        "<a href=\"vbscript:msgbox(1)\">x</a>",
        "<svg><script>alert(1)</script></svg>",
        "<svg onload=alert(1)></svg>",
        "<iframe src=\"https://evil.example\"></iframe>",
        "<object data=\"https://evil.example/x\"></object><embed src=\"https://evil.example/y\">",
        "<form action=\"https://evil.example\"><input name=x><button>go</button></form>",
        "<meta http-equiv=\"refresh\" content=\"0;url=https://evil.example\">",
        "<link rel=\"stylesheet\" href=\"https://evil.example/x.css\">",
        "<base href=\"https://evil.example/\"><a href=\"x\">y</a>",
        "<p style=\"background:url(https://evil.example/t.gif)\">x</p>",
        "<p style=\"width:expression(alert(1))\">x</p>",
        "<p style=\"background:u\\72l(https://evil.example/t.gif)\">x</p>",
        "<img src=\"https://evil.example/pixel.gif\">",
        "<img src=\"data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==\">",
        "<a href=\"data:text/html,<script>alert(1)</script>\">x</a>",
        "<math><mi xlink:href=\"javascript:alert(1)\">x</mi></math>",
        "<div onclick=\"x()\" onmouseover=\"y()\">z</div>",
        "<template><script>alert(1)</script></template>",
        "<noscript><img src=x onerror=alert(1)></noscript>",
        "<p>text<!--[if IE]><script>alert(1)</script><![endif]--></p>",
        "<body onload=alert(1)><p>x</p></body>",
        "<div srcdoc=\"<script>alert(1)</script>\">x</div>",
        "<<script>script>alert(1)<</script>/script>",
    ];
    for payload in payloads {
        let detail = parse_message_detail(1, Vec::new(), &html_message(payload), false);
        let html = detail.body_html.unwrap_or_default().to_ascii_lowercase();
        for bad in FORBIDDEN {
            assert!(
                !html.contains(bad),
                "payload {payload:?} left {bad:?} in {html:?}"
            );
        }
    }
}

#[test]
fn remote_images_are_blocked_until_allowed_and_cid_images_stay_inline() {
    let raw = html_message("<p>hi</p><img src=\"https://evil.example/pixel.gif\">");
    let blocked = parse_message_detail(1, Vec::new(), &raw, false);
    assert!(blocked.has_remote_images);
    assert!(
        !blocked
            .body_html
            .unwrap_or_default()
            .contains("evil.example")
    );
    let allowed = parse_message_detail(1, Vec::new(), &raw, true);
    assert!(
        allowed
            .body_html
            .unwrap_or_default()
            .contains("evil.example")
    );
}

// ------------------------------------------------------- header corpus ----

#[test]
fn awkward_headers_stay_bounded_and_safe() {
    let refs: String = (0..120)
        .map(|i| format!("<r{i}@x>"))
        .collect::<Vec<_>>()
        .join("\r\n ");
    let long_to = format!("{}@example.example", "a".repeat(3000));
    let raw = format!(
        "From: a@example.example\r\nTo: {long_to}\r\nSubject: s\r\nMessage-ID: <first@x>\r\nMessage-ID: <second@x>\r\nReferences: {refs}\r\nList-Unsubscribe: <javascript:alert(1)>, <http://plain.example/u>\r\n\r\nbody\r\n"
    );
    let detail = parse_message_detail(1, Vec::new(), &raw, false);
    assert_eq!(detail.message_id, "<first@x>", "the first Message-ID wins");
    assert_eq!(detail.references.len(), 50, "References is capped");
    assert_eq!(detail.references.last().unwrap(), "<r119@x>", "newest kept");
    assert!(detail.to.chars().count() <= 1000);
    assert!(
        detail.unsubscribe.is_none(),
        "only mailto: and https: unsubscribe targets are kept"
    );
}
