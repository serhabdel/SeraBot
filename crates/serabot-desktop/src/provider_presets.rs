//! Provider presets for the "Add provider" sheet.
//!
//! Data only: no I/O, no UI. The `kind` values are the server's own provider
//! kinds (`crates/serabot/src/providers.rs`), so a preset can be posted to
//! `POST /v1/admin/providers` unchanged.

/// One ready-made provider profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderPreset {
    /// Stable id of the preset. Used to look the preset up again.
    pub id: &'static str,
    /// Name shown in the picker.
    pub label: &'static str,
    /// Base URL the server talks to. Empty for the custom preset.
    pub base_url: &'static str,
    /// Server provider kind (`openai`, `mistral`, `groq`, `openrouter`,
    /// `openai-compatible`).
    pub kind: &'static str,
    /// Model ids offered as a starting list. The owner can change them.
    pub models: &'static [&'static str],
    /// Name of the secret in the encrypted store that holds the API key.
    pub secret_name: &'static str,
    /// One short line for the owner.
    pub note: &'static str,
}

const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        base_url: "https://api.openai.com/v1",
        kind: "openai",
        models: &["gpt-4o-mini", "gpt-4o"],
        secret_name: "openai_api_key",
        note: "OpenAI chat and the /models catalog.",
    },
    ProviderPreset {
        id: "mistral",
        label: "Mistral",
        base_url: "https://api.mistral.ai/v1",
        kind: "mistral",
        models: &["mistral-large-latest", "mistral-small-latest"],
        secret_name: "mistral_api_key",
        note: "Mistral chat; catalog GET https://api.mistral.ai/v1/models.",
    },
    ProviderPreset {
        id: "groq",
        label: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        kind: "groq",
        models: &["llama-3.3-70b-versatile", "llama-3.1-8b-instant"],
        secret_name: "groq_api_key",
        note: "Groq OpenAI-compatible chat.",
    },
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        kind: "openrouter",
        models: &[],
        secret_name: "openrouter_api_key",
        note: "OpenRouter OpenAI-compatible chat. Model ids come from your account.",
    },
    ProviderPreset {
        id: "ollama",
        label: "Ollama / local (OpenAI-compatible)",
        base_url: "http://127.0.0.1:11434/v1",
        kind: "openai-compatible",
        models: &[],
        secret_name: "ollama_api_key",
        note: "Local Ollama. Usually no key is needed; leave the secret empty.",
    },
    ProviderPreset {
        id: "custom",
        label: "Custom (OpenAI-compatible)",
        base_url: "",
        kind: "openai-compatible",
        models: &[],
        secret_name: "custom_api_key",
        note: "Any other OpenAI-compatible base URL. Fill in the URL, models and key.",
    },
];

/// The preset catalog, in picker order.
pub fn presets() -> &'static [ProviderPreset] {
    PRESETS
}

/// One preset by id.
pub fn preset_for(id: &str) -> Option<&'static ProviderPreset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

/// The preset that leaves the base URL to the owner.
pub fn custom_preset() -> &'static ProviderPreset {
    preset_for("custom").expect("the custom preset is always present")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_preset_is_complete_and_unique() {
        let mut ids = HashSet::new();
        for preset in presets() {
            assert!(!preset.id.is_empty(), "{:?}", preset.id);
            assert!(!preset.label.is_empty(), "{:?}", preset.id);
            assert!(!preset.secret_name.is_empty(), "{:?}", preset.id);
            assert!(!preset.note.is_empty(), "{:?}", preset.id);
            assert!(ids.insert(preset.id), "duplicate preset id");
            assert!(!preset.kind.is_empty(), "{:?}", preset.id);
            assert!(
                preset
                    .kind
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
                "{:?}",
                preset.id
            );
        }
    }

    #[test]
    fn the_custom_preset_has_no_base_url() {
        let custom = custom_preset();
        assert_eq!(custom.id, "custom");
        assert!(custom.base_url.is_empty());
        for preset in presets() {
            if preset.id != "custom" {
                assert!(
                    preset.base_url.starts_with("https://")
                        || preset.base_url.starts_with("http://127.0.0.1"),
                    "{:?}",
                    preset.id
                );
            }
        }
    }

    #[test]
    fn presets_match_the_server_provider_kinds() {
        for preset in presets() {
            assert!(
                matches!(
                    preset.kind,
                    "openai" | "mistral" | "groq" | "openrouter" | "openai-compatible"
                ),
                "{:?}",
                preset.id
            );
        }
        assert_eq!(
            preset_for("openai").unwrap().base_url,
            "https://api.openai.com/v1"
        );
        assert_eq!(
            preset_for("mistral").unwrap().base_url,
            "https://api.mistral.ai/v1"
        );
        assert_eq!(
            preset_for("groq").unwrap().base_url,
            "https://api.groq.com/openai/v1"
        );
        assert_eq!(
            preset_for("openrouter").unwrap().base_url,
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            preset_for("ollama").unwrap().base_url,
            "http://127.0.0.1:11434/v1"
        );
        assert!(preset_for("nope").is_none());
    }
}
