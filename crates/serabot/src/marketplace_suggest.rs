//! `suggest_marketplace_app` (v0.51, M8): a read-only tool that lets a bot point
//! the member at a marketplace app that would help, as an install card in the
//! chat.
//!
//! The tool installs nothing, signs nothing in and stores nothing. It reads the
//! catalog and the caller's own installs, and returns up to three cards. Only an
//! app that can run is suggested: it has an `http` server on a real host (never
//! a `*.invalid` placeholder), needs no local program, and is not already
//! installed and enabled for the caller. The member installs from the
//! Integrations screen, where sign-in or the API key is asked for.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::marketplace::{App, AuthKind, PluginManifest, Server};
use crate::result_cards::{ResultCard, install_card};
use crate::store::Store;
use crate::util::truncate_chars;

pub const TOOL: &str = "suggest_marketplace_app";
/// Most cards one call shows.
const MAX_SUGGESTIONS: usize = 3;
const MAX_QUERY_CHARS: usize = 200;

pub struct SuggestMarketplaceApp {
    store: Store,
    catalog: Arc<Vec<PluginManifest>>,
}

impl SuggestMarketplaceApp {
    pub fn new(store: Store, catalog: Vec<PluginManifest>) -> Self {
        Self {
            store,
            catalog: Arc::new(catalog),
        }
    }
}

/// Register the tool.
pub fn register(
    registry: &mut crate::gateway::ToolRegistry,
    store: Store,
    catalog: Vec<PluginManifest>,
) -> Result<()> {
    registry.register(SuggestMarketplaceApp::new(store, catalog))
}

/// What the member must do after installing: `sign_in`, `api_key` or `none`.
/// `None` when the app cannot run in this build (a v1 link-out sign-in, a local
/// program, a placeholder host).
fn needs_of(app: &App) -> Option<&'static str> {
    let Server::Http { url } = &app.server else {
        return None;
    };
    let host = url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    if host.ends_with(".invalid") || host.is_empty() {
        return None;
    }
    match app.auth.first().map(|auth| auth.kind) {
        None | Some(AuthKind::None) => Some("none"),
        Some(AuthKind::ApiKey) => Some("api_key"),
        Some(AuthKind::Oauth) => Some("sign_in"),
        Some(AuthKind::OauthLink) => None,
    }
}

/// The plugin's first runnable app need, or `None` when no app can run.
fn runnable(manifest: &PluginManifest) -> Option<&'static str> {
    manifest.apps.iter().find_map(needs_of)
}

fn terms(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .map(str::to_owned)
        .collect()
}

/// How well a plugin matches the query terms: name and slug weigh most.
fn score(manifest: &PluginManifest, terms: &[String]) -> usize {
    let name = format!("{} {}", manifest.name, manifest.slug).to_lowercase();
    let tagline = format!("{} {}", manifest.tagline, manifest.category).to_lowercase();
    let description = manifest.description.to_lowercase();
    terms
        .iter()
        .map(|term| {
            let mut points = 0;
            if name.contains(term) {
                points += 4;
            }
            if tagline.contains(term) {
                points += 2;
            }
            if description.contains(term) {
                points += 1;
            }
            points
        })
        .sum()
}

#[async_trait]
impl Tool for SuggestMarketplaceApp {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL.into(),
            description: "Suggest a marketplace app that fits what the user is trying to do. It shows up to 3 install cards in the chat and installs nothing: the user installs from the card. Use it when a task needs an outside service and no connected tool covers it. Read only.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What the user needs, in a few words, for example 'design a poster' or 'track issues'"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    fn requires_approval(&self, _args: &Value) -> bool {
        false
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(|text| truncate_chars(text.trim(), MAX_QUERY_CHARS))
            .filter(|text| !text.is_empty())
            .ok_or_else(|| SeraError::Validation("query is required".into()))?;
        let terms = terms(&query);
        // The caller's own installs only: apps already on are not suggested.
        let installed: Vec<String> = self
            .store
            .marketplace_installs(&context.user_id)?
            .into_iter()
            .filter(|(_, enabled, _)| *enabled)
            .map(|(slug, _, _)| slug)
            .collect();
        let mut matches: Vec<(usize, &PluginManifest, &'static str)> = self
            .catalog
            .iter()
            .filter(|manifest| !installed.contains(&manifest.slug))
            .filter_map(|manifest| Some((score(manifest, &terms), manifest, runnable(manifest)?)))
            .filter(|(points, _, _)| *points > 0)
            .collect();
        matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.slug.cmp(&b.1.slug)));
        matches.truncate(MAX_SUGGESTIONS);
        if matches.is_empty() {
            return Ok(ToolOutput {
                content: "No installable marketplace app matches that. Do not suggest one.".into(),
                data: Some(json!({"suggested": []})),
                ..ToolOutput::default()
            });
        }
        let cards: Vec<ResultCard> = matches
            .iter()
            .map(|(_, manifest, needs)| {
                install_card(
                    &manifest.slug,
                    &manifest.name,
                    &manifest.tagline,
                    &manifest.category,
                    needs,
                )
            })
            .collect();
        let names: Vec<String> = matches
            .iter()
            .map(|(_, manifest, needs)| {
                format!("{} (needs {})", manifest.name, needs.replace('_', " "))
            })
            .collect();
        Ok(ToolOutput {
            content: format!(
                "Shown to the user as install cards: {}. Nothing was installed; the user installs from the card in Integrations. Say what the app would do, then continue without it.",
                names.join(", ")
            ),
            data: Some(json!({
                "suggested": matches.iter().map(|(_, m, _)| m.slug.clone()).collect::<Vec<_>>()
            })),
            cards,
            ..ToolOutput::default()
        })
    }
}

#[cfg(test)]
mod tests;
