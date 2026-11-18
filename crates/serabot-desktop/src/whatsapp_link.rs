//! Blocking calls behind the "Link a spare WhatsApp number" card (0.58).
//! Each one opens its own client from the session's credentials; the card
//! runs them off the UI thread.

use serde_json::Value;

use crate::api::ApiClient;
use crate::worker::Credentials;

fn client(credentials: &Credentials) -> Result<ApiClient, String> {
    ApiClient::new(credentials.endpoint.clone(), credentials.token.clone())
        .map_err(|error| error.to_string())
}

pub fn status(credentials: &Credentials) -> Result<Value, String> {
    client(credentials)?
        .whatsapp_link_status()
        .map_err(|error| error.to_string())
}

pub fn pair(credentials: &Credentials, phone: Option<&str>) -> Result<Value, String> {
    client(credentials)?
        .whatsapp_link_pair(phone)
        .map_err(|error| error.to_string())
}

pub fn set_allowlist(credentials: &Credentials, chats: &[String]) -> Result<Value, String> {
    client(credentials)?
        .whatsapp_link_allowlist(chats)
        .map_err(|error| error.to_string())
}

pub fn sign_out(credentials: &Credentials) -> Result<Value, String> {
    client(credentials)?
        .whatsapp_link_sign_out()
        .map_err(|error| error.to_string())
}
