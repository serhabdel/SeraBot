mod api;
pub mod state;
pub mod worker;
pub use api::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub epoch: u64,
    pub conversation: String,
}

impl Scope {
    pub fn accepts(&self, response: &Self) -> bool {
        self == response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_accepts_tls_and_exact_loopback_only() {
        for endpoint in [
            "https://bot.example",
            "http://localhost:9000",
            "http://127.0.0.1:9000/",
            "http://[::1]:9000",
        ] {
            assert!(validate_endpoint(endpoint).is_ok(), "{endpoint}");
        }
        for endpoint in [
            "http://bot.example",
            "http://localhost.evil.test",
            "http://127.0.0.1.evil.test",
            "http://127.1",
            "http://2130706433",
            "http://0x7f000001",
            "http://127.0.0.2",
            "http://localhost.",
            "http://[::ffff:127.0.0.1]",
            "https://user:password@bot.example",
            "https://@bot.example",
            "https://bot.example/?token=x",
            "https://bot.example/#x",
            "https://bot.example/base",
            "http://localhost\\@evil.test",
            " http://localhost",
            "http://local\nhost",
            "http://%6cocalhost",
            "file:///tmp/x",
        ] {
            assert!(validate_endpoint(endpoint).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn a_server_timeout_does_not_prove_a_mutation_was_rejected() {
        assert!(ApiError::Http(408).unknown_outcome());
        assert!(ApiError::Http(504).unknown_outcome());
        assert!(!ApiError::Http(401).unknown_outcome());
        assert!(!ApiError::Http(403).unknown_outcome());
        assert!(!ApiError::Http(429).unknown_outcome());
    }

    #[test]
    fn identifiers_tokens_and_composer_are_bounded() {
        assert!(valid_id("windows-owner:1_test"));
        for id in ["", "a/b", "a b", "..", "é", &"a".repeat(129)] {
            assert!(!valid_id(id));
        }
        assert!(valid_token(&"a".repeat(32)));
        assert!(valid_token(&"a".repeat(512)));
        for token in [
            "a".repeat(31),
            "a".repeat(513),
            format!("{}\r\n", "a".repeat(32)),
        ] {
            assert!(!valid_token(&token));
        }
        assert!(valid_text(&"é".repeat(8000)));
        assert!(!valid_text(&"a".repeat(8001)));
        assert!(!valid_text(" \n"));
    }

    #[test]
    fn selection_requires_catalog_membership_and_egress_approval() {
        let catalog: Catalog = serde_json::from_str(r#"{"providers":[{"id":"blocked","models":["private"],"data_egress_approved":false},{"id":"allowed","models":["small","large"],"data_egress_approved":true}],"selection":{"provider":"blocked","model":"private"}}"#).unwrap();
        assert_eq!(catalog.providers.len(), 2, "blocked profiles stay visible");
        assert!(!catalog.allows(&catalog.selection));
        assert!(catalog.allows(&Selection {
            provider: "allowed".into(),
            model: "small".into()
        }));
        assert!(!catalog.allows(&Selection {
            provider: "allowed".into(),
            model: "private".into()
        }));
        assert!(!catalog.allows(&Selection {
            provider: "invented".into(),
            model: "small".into()
        }));
    }

    #[test]
    fn stale_results_cannot_cross_sessions_or_conversations() {
        let old = Scope {
            epoch: 1,
            conversation: "windows-owner".into(),
        };
        assert!(old.accepts(&old));
        assert!(!old.accepts(&Scope {
            epoch: 2,
            conversation: old.conversation.clone()
        }));
        assert!(!old.accepts(&Scope {
            epoch: 1,
            conversation: "new".into()
        }));
        assert!(!old.accepts(&Scope {
            epoch: 3,
            conversation: old.conversation.clone()
        }));
    }
}
