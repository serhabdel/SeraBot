//! `/v1/whatsapp-link` (0.58): each member sees and changes only their own
//! link; pairing is refused while the server has linking turned off.

use super::*;

const ADMIN_TOKEN: &str = "test-token-that-is-long-enough-123";

#[tokio::test]
async fn whatsapp_link_routes_are_per_member_and_refuse_pair_when_disabled() {
    // v0.59 enables [whatsapp_link] by default in Config; an ApiState without
    // a wired WhatsappLink handle (e.g. when enabled = false) reports
    // enabled: false and refuses /pair with 422.
    assert!(crate::config::Config::default().whatsapp_link.enabled);

    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("subject-wa", "wa@example.example", "Synthetic Member")
        .unwrap();
    let member_token = mint_for(&store, &member.id, "wa-member");

    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/whatsapp-link",
            ADMIN_TOKEN,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["enabled"], false);
    assert_eq!(body["status"], "signed_out");
    assert!(body["risk"].as_str().unwrap().contains("spare number"));

    let response = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/whatsapp-link/pair",
            ADMIN_TOKEN,
            Some(json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // The allowed chats are normalised and belong to the caller only.
    let response = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/whatsapp-link/allowlist",
            ADMIN_TOKEN,
            Some(json!({"chats": ["+33 6 12 34 56 78"]})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["allowlist"], json!(["33612345678@s.whatsapp.net"]));

    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/whatsapp-link",
            &member_token,
            None,
        ))
        .await
        .unwrap();
    let body = read_json(response).await;
    assert_eq!(body["allowlist"], json!([]));

    // Groups cannot be allowed.
    let response = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/whatsapp-link/allowlist",
            ADMIN_TOKEN,
            Some(json!({"chats": ["120363000000@g.us"]})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let response = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            "/v1/whatsapp-link",
            ADMIN_TOKEN,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let admin = store.admin_user_id().unwrap();
    assert!(store.whatsapp_link_account(&admin).unwrap().is_none());
}
