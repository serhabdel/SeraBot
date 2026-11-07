//! Composio client: the acting user's own key and linked apps.
//!
//! The key travels once in a `PUT` body and is never read back. App names
//! and descriptions are server data shown as plain text; the sign-in address
//! must be a plain `https` URL.

use super::*;

const MAX_APPS: usize = 64;
const MAX_ACCOUNTS: usize = 100;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ComposioApp {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ComposioAccount {
    pub id: String,
    pub toolkit: String,
    #[serde(default)]
    pub status: String,
}

impl ComposioAccount {
    pub fn is_active(&self) -> bool {
        self.status.eq_ignore_ascii_case("active")
    }
}

#[derive(Deserialize)]
struct Apps {
    #[serde(default)]
    toolkits: Vec<ComposioApp>,
}

#[derive(Deserialize)]
struct Accounts {
    #[serde(default)]
    accounts: Vec<ComposioAccount>,
}

#[derive(Deserialize)]
struct LinkAck {
    url: String,
}

#[derive(Deserialize)]
struct KeyAck {
    #[serde(default)]
    connected: bool,
}

#[derive(Serialize)]
struct KeyBody<'a> {
    key: &'a str,
}

#[derive(Serialize)]
struct ConnectBody<'a> {
    toolkit: &'a str,
}

fn plain_id(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn check_app(app: &ComposioApp) -> bool {
    plain_id(&app.slug, 64) && bounded_text(&app.name, 120) && app.description.chars().count() <= 400
}

impl ApiClient {
    /// `PUT /v1/connectors/composio/key`: the server checks the key with
    /// Composio, seals it, and never returns it.
    pub fn composio_set_key(&self, key: &str) -> Result<(), ApiError> {
        if key.len() < 8 || key.len() > 512 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ApiError::Validation("That does not look like a key."));
        }
        let ack: KeyAck = self.panel_json(
            Method::PUT,
            &["v1", "connectors", "composio", "key"],
            Some(&KeyBody { key }),
        )?;
        if ack.connected {
            Ok(())
        } else {
            Err(ApiError::InvalidResponse)
        }
    }

    /// `DELETE /v1/connectors/composio`: forget the key.
    pub fn composio_remove_key(&self) -> Result<(), ApiError> {
        let ack: KeyAck =
            self.panel_json::<_, ()>(Method::DELETE, &["v1", "connectors", "composio"], None)?;
        if ack.connected {
            Err(ApiError::InvalidResponse)
        } else {
            Ok(())
        }
    }

    /// `GET /v1/connectors/composio/toolkits?q=`: apps matching a name.
    pub fn composio_apps(&self, query: &str) -> Result<Vec<ComposioApp>, ApiError> {
        let query: String = query.trim().chars().take(80).collect();
        let bytes = self.panel_exchange_query::<()>(
            Method::GET,
            &["v1", "connectors", "composio", "toolkits"],
            &[("q", query.as_str())],
            None,
        )?;
        let apps: Apps = serde_json::from_slice(&bytes).map_err(|_| ApiError::InvalidResponse)?;
        if apps.toolkits.len() > MAX_APPS || !apps.toolkits.iter().all(check_app) {
            return Err(ApiError::InvalidResponse);
        }
        Ok(apps.toolkits)
    }

    /// `GET /v1/connectors/composio/accounts`: this user's linked apps.
    pub fn composio_accounts(&self) -> Result<Vec<ComposioAccount>, ApiError> {
        let list: Accounts = self.panel_json::<_, ()>(
            Method::GET,
            &["v1", "connectors", "composio", "accounts"],
            None,
        )?;
        if list.accounts.len() > MAX_ACCOUNTS
            || !list.accounts.iter().all(|a| {
                plain_id(&a.id, 128) && plain_id(&a.toolkit, 64) && a.status.len() <= 32
            })
        {
            return Err(ApiError::InvalidResponse);
        }
        Ok(list.accounts)
    }

    /// `POST /v1/connectors/composio/connect`: Composio's hosted sign-in page
    /// for one app. Only an `https` address is accepted back.
    pub fn composio_connect(&self, toolkit: &str) -> Result<String, ApiError> {
        if !plain_id(toolkit, 64) {
            return Err(ApiError::Validation("That app is not valid."));
        }
        let ack: LinkAck = self.panel_json(
            Method::POST,
            &["v1", "connectors", "composio", "connect"],
            Some(&ConnectBody { toolkit }),
        )?;
        check_consent_url(&ack.url)?;
        Ok(ack.url)
    }

    /// `DELETE /v1/connectors/composio/accounts/{id}`: unlink one app.
    pub fn composio_remove_account(&self, id: &str) -> Result<(), ApiError> {
        if !plain_id(id, 128) {
            return Err(ApiError::Validation("That app link is not valid."));
        }
        let _: serde_json::Value = self.panel_json::<_, ()>(
            Method::DELETE,
            &["v1", "connectors", "composio", "accounts", id],
            None,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_plain_words() {
        assert!(plain_id("gmail", 64));
        assert!(plain_id("google_drive", 64));
        assert!(!plain_id("", 64));
        assert!(!plain_id("a/b", 64));
        assert!(!plain_id("a b", 64));
    }

    #[test]
    fn apps_are_bounded() {
        let ok = ComposioApp {
            slug: "gmail".into(),
            name: "Gmail".into(),
            description: "Mail".into(),
        };
        assert!(check_app(&ok));
        let long = ComposioApp {
            description: "x".repeat(401),
            ..ok.clone()
        };
        assert!(!check_app(&long));
        let bad = ComposioApp {
            slug: "../x".into(),
            ..ok
        };
        assert!(!check_app(&bad));
    }

    #[test]
    fn active_status_is_case_insensitive() {
        let account = ComposioAccount {
            id: "ca_1".into(),
            toolkit: "gmail".into(),
            status: "ACTIVE".into(),
        };
        assert!(account.is_active());
    }
}
