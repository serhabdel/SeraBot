//! Standing grants list/revoke (0.30.0, B5): acting user only.

use super::*;

#[tokio::test]
async fn standing_grants_are_scoped_and_revoke_audits() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let alice = store
        .create_user("subject-sga", "sga@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-sgb", "sgb@example.example", "Synthetic B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "sg-alice");
    let bob_token = mint_for(&store, &bob.id, "sg-bob");

    let alice_action = store
        .create_action(
            &alice.id,
            "synthetic_tool",
            None,
            None,
            "{}",
            "alice-hash",
            Some("example.test"),
            None,
            "policy-v1",
        )
        .unwrap();
    let alice_approval = store
        .create_approval(
            &alice.id,
            &alice_action.id,
            "synthetic_tool",
            "synthetic",
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .unwrap();
    store
        .insert_grant(
            &alice.id,
            "synthetic_tool",
            "alice-hash",
            Some("example.test"),
            "policy-v1",
            &alice_approval.id,
        )
        .unwrap();

    let bob_action = store
        .create_action(
            &bob.id,
            "synthetic_tool",
            None,
            None,
            "{}",
            "bob-hash",
            None,
            None,
            "policy-v1",
        )
        .unwrap();
    let bob_approval = store
        .create_approval(
            &bob.id,
            &bob_action.id,
            "synthetic_tool",
            "synthetic",
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .unwrap();
    store
        .insert_grant(
            &bob.id,
            "synthetic_tool",
            "bob-hash",
            None,
            "policy-v1",
            &bob_approval.id,
        )
        .unwrap();

    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/standing-grants",
            &alice_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let grants = body["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["tool"], "synthetic_tool");
    assert_eq!(grants[0]["canonical_hash"], "alice-hash");
    assert!(grants[0]["active"].as_bool().unwrap());

    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/standing-grants",
            &bob_token,
            None,
        ))
        .await
        .unwrap();
    let body = read_json(response).await;
    assert_ne!(
        body["grants"].as_array().unwrap()[0]["canonical_hash"],
        "alice-hash"
    );

    let grant_id = grants[0]["id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            &format!("/v1/standing-grants/{grant_id}"),
            &bob_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            &format!("/v1/standing-grants/{grant_id}"),
            &alice_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        !read_json(response).await["grant"]["active"]
            .as_bool()
            .unwrap()
    );

    let audit = store.list_audit_for_user(&alice.id, 20).unwrap();
    assert!(audit.iter().any(|row| {
        row.event == "standing_grant.revoked" && row.tool.as_deref() == Some("synthetic_tool")
    }));
}
