mod api;
pub mod creds;
pub mod events;
pub mod session;
pub mod state;
pub mod updater;
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
    fn server_hint_keeps_a_valid_origin_and_drops_invalid_values() {
        assert_eq!(
            resolve_server_hint(Some("https://owner.example"), None),
            "https://owner.example"
        );
        assert_eq!(
            resolve_server_hint(
                Some("  https://owner.example  "),
                Some("http://evil.example")
            ),
            "https://owner.example"
        );
        assert_eq!(
            resolve_server_hint(None, Some("https://compiled.example")),
            "https://compiled.example"
        );
        assert_eq!(
            resolve_server_hint(
                Some("http://owner.example"),
                Some("https://compiled.example")
            ),
            "https://compiled.example"
        );
        assert_eq!(resolve_server_hint(Some(""), Some("")), "");
        assert_eq!(
            resolve_server_hint(Some("https://user:secret@owner.example"), Some("not a url")),
            ""
        );
        assert_eq!(OWNER_TOKEN_SECRET_NAME, "SERABOT_OWNER_TOKEN");
        assert!(!valid_token(OWNER_TOKEN_SECRET_NAME));
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
    fn bot_and_conversation_inputs_stay_on_the_safe_subset() {
        assert!(valid_bot_id("notes"));
        assert!(valid_bot_id("com"));
        assert!(!valid_bot_id("main"));
        assert!(!valid_bot_id("con"));
        assert!(!valid_bot_id("com1"));
        assert!(!valid_bot_id("lpt9"));
        assert!(!valid_bot_id("Notes"));
        assert!(!valid_bot_id("../notes"));
        assert!(!valid_bot_id(""));
        let mut seen = std::collections::HashSet::new();
        for tool in SPECIALIST_TOOLS {
            assert!(seen.insert(*tool), "{tool}");
            assert_ne!(*tool, "delegate");
        }
        for tool in DEFAULT_SPECIALIST_TOOLS {
            assert!(SPECIALIST_TOOLS.contains(tool));
        }
        assert!(validate_tool_list(&["skill_list".into(), "rag_search".into()]).is_ok());
        assert!(validate_tool_list(&[]).is_ok());
        assert!(validate_tool_list(&["delegate".into()]).is_err());
        assert!(validate_tool_list(&["../secret".into()]).is_err());
        assert!(validate_tool_list(&vec!["skill_list".into(); 33]).is_err());
        assert_eq!(
            parse_skill_list("ask, notes").unwrap(),
            vec!["ask".to_string(), "notes".to_string()]
        );
        assert!(parse_skill_list("").unwrap().is_empty());
        assert!(parse_skill_list("Ask").is_err());
        assert!(parse_skill_list("../ask").is_err());
        assert!(parse_skill_list("ask ask").is_err());
        assert!(valid_conversation_parent(ConversationKind::Section, None).is_ok());
        assert!(valid_conversation_parent(ConversationKind::Section, Some("sec:finance")).is_err());
        assert!(valid_conversation_parent(ConversationKind::Group, None).is_err());
        assert!(valid_conversation_parent(ConversationKind::Group, Some("sec:finance")).is_ok());
        assert!(valid_conversation_parent(ConversationKind::Group, Some("grp:ops")).is_err());
        assert!(valid_conversation_parent(ConversationKind::Direct, None).is_ok());
        assert!(valid_conversation_parent(ConversationKind::Direct, Some("grp:ops")).is_ok());
        assert!(valid_conversation_parent(ConversationKind::Room, Some("dir:analyst")).is_err());
        assert_eq!(
            conversation_node_id(ConversationKind::Group, "ops").as_deref(),
            Some("grp:ops")
        );
        assert!(conversation_node_id(ConversationKind::Section, "Finance").is_none());
        assert!(valid_bot_description("line\nok\t"));
        assert!(!valid_bot_description("a\u{0}b"));
        assert!(!valid_bot_description(&"é".repeat(2001)));
    }

    #[test]
    fn skill_inputs_stay_on_the_safe_subset() {
        assert!(valid_new_skill_id("notes-pack"));
        assert!(valid_new_skill_id("main"));
        assert!(valid_new_skill_id("a"));
        assert!(!valid_new_skill_id(""));
        assert!(!valid_new_skill_id("Notes"));
        assert!(!valid_new_skill_id("../notes"));
        assert!(!valid_new_skill_id("con"));
        assert!(!valid_new_skill_id("aux"));
        assert!(!valid_new_skill_id("com1"));
        assert!(!valid_new_skill_id("lpt9"));
        assert!(!valid_new_skill_id(&"a".repeat(65)));
        assert!(valid_skill_description(""));
        assert!(valid_skill_description(" Short summary "));
        assert!(!valid_skill_description("say \"hi\""));
        assert!(!valid_skill_description("a\\b"));
        assert!(!valid_skill_description("line\nbreak"));
        assert!(!valid_skill_description(&"a".repeat(501)));
        assert!(valid_skill_body(""));
        assert!(valid_skill_body("# Notes\n\nSteps.\t"));
        assert!(!valid_skill_body("a\u{0}b"));
        assert!(!valid_skill_body(&"a".repeat(96_001)));
    }

    #[test]
    fn conversation_memory_and_secret_inputs_stay_bounded() {
        assert!(valid_conversation_title("Operations review"));
        assert!(!valid_conversation_title("   "));
        assert!(!valid_conversation_title("line\nbreak"));
        assert!(!valid_conversation_title(&"a".repeat(121)));
        assert!(valid_memory_kind("fact"));
        assert!(valid_memory_kind("preference"));
        assert!(!valid_memory_kind("secret"));
        assert!(!valid_memory_kind("Fact"));
        assert!(valid_memory_content("line one\nline two\t"));
        assert!(!valid_memory_content(" \n "));
        assert!(!valid_memory_content("a\u{0}b"));
        assert!(!valid_memory_content(&"é".repeat(4_001)));
        assert!(valid_memory_source("operator:desktop"));
        assert!(!valid_memory_source(""));
        assert!(!valid_memory_source(&"a".repeat(513)));
        assert!(valid_memory_id("mem_1-2"));
        assert!(!valid_memory_id(""));
        assert!(!valid_memory_id("a/b"));
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
