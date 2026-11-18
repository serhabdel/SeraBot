//! `/v1/whatsapp-link`: link a member's spare WhatsApp number as a device,
//! see its state, set the allowed chats, sign out. Every route acts on the
//! caller's own link only. Design: docs/specs/whatsapp-linked-device.md.

use super::*;

#[cfg(feature = "whatsapp-link")]
fn link(state: &ApiState) -> Result<&Arc<crate::whatsapp_link::client::WhatsappLink>> {
    state
        .whatsapp_link
        .as_ref()
        .filter(|link| link.enabled())
        .ok_or_else(|| {
            SeraError::Validation(
                "WhatsApp linking is turned off on this server ([whatsapp_link] enabled)".into(),
            )
        })
}

#[cfg(not(feature = "whatsapp-link"))]
fn link(_state: &ApiState) -> Result<&'static ()> {
    Err(SeraError::Validation(
        "this server was built without WhatsApp linking".into(),
    ))
}

/// The caller's link: status, allowed chats and, while pairing, the QR
/// payload or pairing code to show.
pub(super) async fn whatsapp_link_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let account = state.store.whatsapp_link_account(&user.id)?;
    #[cfg_attr(not(feature = "whatsapp-link"), allow(unused_mut))]
    let mut body = json!({
        "enabled": link(&state).is_ok(),
        "status": account.as_ref().map(|a| a.status.clone()).unwrap_or_else(|| "signed_out".into()),
        "phone": account.as_ref().and_then(|a| a.phone.clone()),
        "allowlist": account.as_ref().map(|a| a.allowlist.clone()).unwrap_or_default(),
        "risk": "Unofficial WhatsApp client: the number can be banned. Use a spare number.",
    });
    #[cfg(feature = "whatsapp-link")]
    if let Ok(link) = link(&state) {
        body["connected"] = json!(link.is_live(&user.id));
        body["pairing"] = serde_json::to_value(link.pairing_view(&user.id))?;
    }
    Ok(Json(body))
}

#[derive(Deserialize)]
pub(super) struct PairRequest {
    /// With a number, WhatsApp also offers a pairing code for it.
    phone: Option<String>,
}

/// Start linking: a QR code (and a pairing code when a number is given)
/// appears in the status within a few seconds.
pub(super) async fn whatsapp_link_pair(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<PairRequest>,
) -> Result<Json<Value>> {
    #[cfg(feature = "whatsapp-link")]
    {
        let link = link(&state)?.clone();
        link.start(
            &user.id,
            request.phone.as_deref().filter(|p| !p.trim().is_empty()),
        )
        .await?;
        audit_auth(
            &state,
            &user.id,
            "connector.whatsapp_link_pairing",
            json!({"connector": "whatsapp_link", "pair_code": request.phone.is_some()}),
        )?;
        Ok(Json(json!({"status": "pairing"})))
    }
    #[cfg(not(feature = "whatsapp-link"))]
    {
        let _ = (&user, &request.phone);
        link(&state)?;
        unreachable!()
    }
}

#[derive(Deserialize)]
pub(super) struct AllowlistRequest {
    chats: Vec<String>,
}

/// Replace the chats whose messages reach the bot (phone numbers).
pub(super) async fn whatsapp_link_allowlist(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<AllowlistRequest>,
) -> Result<Json<Value>> {
    let list = state
        .store
        .set_whatsapp_link_allowlist(&user.id, &request.chats)?;
    audit_auth(
        &state,
        &user.id,
        "connector.whatsapp_link_allowlist",
        json!({"connector": "whatsapp_link", "count": list.len()}),
    )?;
    Ok(Json(json!({"allowlist": list})))
}

/// Sign out on WhatsApp's side and delete the session.
pub(super) async fn whatsapp_link_sign_out(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    #[cfg(feature = "whatsapp-link")]
    if let Some(link) = state.whatsapp_link.as_ref() {
        link.sign_out(&user.id).await?;
    }
    state.store.delete_whatsapp_link(&user.id)?;
    audit_auth(
        &state,
        &user.id,
        "connector.whatsapp_link_signed_out",
        json!({"connector": "whatsapp_link"}),
    )?;
    Ok(Json(json!({"status": "signed_out"})))
}
