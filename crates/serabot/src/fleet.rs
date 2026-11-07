//! Per-user bot fleets, backed by the `bots` table.
//!
//! The repository's `bots/` + `prompts/` directories are **seed templates**
//! only: `config::load_bots` reads them once at startup, and
//! [`FleetRegistry::seed`] copies them into a user's rows the first time the
//! user needs a fleet (at account creation, or lazily on the first read).
//! After that the database is authoritative — enable/disable, prompt edits,
//! and scaffolds are scoped writes on the owner's rows.
//!
//! Every method takes the acting user's id. A fleet is loaded, validated, and
//! cached per user; the cache is invalidated by the API after a bot write.
//! Validation is the `orchestration::validate_bot_fleet` rule applied to that
//! user's rows: exactly one enabled orchestrator (named `main`), and every
//! delegate resolves inside the same user's fleet. A user with an invalid
//! fleet fails closed (their turns and the `live` flag error or report false)
//! without affecting any other user.
//!
//! The admin console overlay (`AdminOverlay.bot_configs`) is applied only
//! when the acting user is the bootstrap admin; it is never copied into the
//! seed templates, so members never inherit the admin's tool or skill
//! overrides.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::config::{BotConfig, Config, ConfiguredBot, LoadedBot, apply_bot_config_override};
use crate::error::{Result, SeraError};
use crate::orchestration::{BotFleetMember, safe_bot_label, validate_bot_fleet};
use crate::skills::SkillCatalog;
use crate::store::{Store, StoredBot};

/// A user's loaded fleet, keyed by bot id.
pub type Fleet = HashMap<String, LoadedBot>;

/// Replace a user's bot rows with the shipped templates.
///
/// This is the explicit opt-in behind `serabot fleet reseed`: the database is
/// authoritative after seeding, so a binary upgrade never rewrites a fleet on
/// its own. Every existing row for the user is dropped first, which means
/// prompt edits, tool allowlists, and scaffolded bots are lost — the command
/// prints the plan before writing and supports `--dry-run`.
///
/// Returns the number of templates written.
pub fn reseed_user(store: &Store, user_id: &str, templates: &[LoadedBot]) -> Result<usize> {
    for row in store.list_bots(user_id)? {
        store.delete_bot(user_id, &row.name)?;
    }
    for template in templates {
        store.upsert_bot(
            user_id,
            &template.config.name,
            &serde_json::to_value(&template.config)?,
            &template.prompt,
            template.config.enabled,
        )?;
    }
    Ok(templates.len())
}

/// The per-user fleet registry shared by the runtime, the skill tools, and
/// the delegation/team tools. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct FleetRegistry {
    config: Arc<Config>,
    store: Store,
    catalog: Arc<SkillCatalog>,
    templates: Arc<Vec<LoadedBot>>,
    admin_user_id: String,
    cache: Arc<RwLock<HashMap<String, Arc<Fleet>>>>,
}

impl FleetRegistry {
    /// Build the registry from the validated seed templates. Fails fast when
    /// the templates do not form a valid fleet or reference an unknown
    /// shipped skill: a broken template set must stop startup, not surface
    /// per user later.
    pub fn new(
        config: Arc<Config>,
        store: Store,
        templates: Vec<LoadedBot>,
        catalog: Arc<SkillCatalog>,
    ) -> Result<Self> {
        Self::validate_fleet(&templates)?;
        catalog.validate_bot_skills(&templates)?;
        let admin_user_id = store.admin_user_id()?;
        Ok(Self {
            config,
            store,
            catalog,
            templates: Arc::new(templates),
            admin_user_id,
            cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// The repository templates this registry seeds from. Read-only.
    pub fn templates(&self) -> &[LoadedBot] {
        &self.templates
    }

    /// The shipped skill catalog (without any user's rows).
    pub fn catalog(&self) -> &SkillCatalog {
        &self.catalog
    }

    /// The fleet-structure rule, applied to a candidate set of rows. Public so
    /// the API can reject an enable/disable/delete that would leave the
    /// owner's fleet invalid before writing it.
    pub fn validate_fleet(bots: &[LoadedBot]) -> Result<()> {
        let members = bots
            .iter()
            .map(|bot| BotFleetMember {
                name: bot.config.name.clone(),
                enabled: bot.config.enabled,
                role: bot.config.role,
                delegates: bot.config.delegates.clone(),
            })
            .collect::<Vec<_>>();
        validate_bot_fleet(&members)
    }

    /// Insert the default fleet for a user who has none, and repair the
    /// empty prompts migration 0010 leaves on pre-existing rows (SQL cannot
    /// read the prompt files). Idempotent; returns the number of rows
    /// inserted (0 when the user already had a fleet).
    pub fn seed(&self, user_id: &str) -> Result<usize> {
        let existing = self.store.list_bots(user_id)?;
        if existing.is_empty() {
            for template in self.templates.iter() {
                self.store.upsert_bot(
                    user_id,
                    &template.config.name,
                    &serde_json::to_value(&template.config)?,
                    &template.prompt,
                    template.config.enabled,
                )?;
            }
            if !self.templates.is_empty() {
                self.invalidate(user_id);
            }
            return Ok(self.templates.len());
        }
        let mut repaired = 0;
        for row in existing {
            if !row.prompt.is_empty() {
                continue;
            }
            if let Some(template) = self
                .templates
                .iter()
                .find(|template| template.config.name == row.name)
            {
                self.store
                    .set_bot_prompt(user_id, &row.name, &template.prompt)?;
                repaired += 1;
            }
        }
        if repaired > 0 {
            self.invalidate(user_id);
        }
        Ok(0)
    }

    /// The acting user's rows, seeding the default fleet when the user has
    /// none. Rows are returned even when the fleet is structurally invalid so
    /// the console can show what needs fixing.
    #[allow(dead_code)]
    pub fn rows(&self, user_id: &str) -> Result<Vec<StoredBot>> {
        self.seed(user_id)?;
        self.store.list_bots(user_id)
    }

    /// One row of the acting user, seeding first. Another user's bot is
    /// indistinguishable from a missing one.
    pub fn row(&self, user_id: &str, name: &str) -> Result<Option<StoredBot>> {
        self.seed(user_id)?;
        self.store.get_bot(user_id, name)
    }

    /// The acting user's fleet rows as `LoadedBot`s, without validating the
    /// fleet. Used to build a candidate set for an update/delete check.
    pub fn candidates(&self, user_id: &str) -> Result<Vec<LoadedBot>> {
        let rows = self.rows(user_id)?;
        self.rows_to_bots(rows, user_id)
    }

    /// The acting user's validated fleet. Cached until [`Self::invalidate`].
    /// A fleet that fails validation is not cached and every caller fails
    /// closed with the validation error.
    pub fn load(&self, user_id: &str) -> Result<Arc<Fleet>> {
        if let Some(fleet) = self
            .cache
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(user_id)
        {
            return Ok(fleet.clone());
        }
        let bots = self.candidates(user_id)?;
        Self::validate_fleet(&bots)?;
        self.catalog
            .effective(&self.store, user_id)?
            .validate_bot_skills(&bots)?;
        let fleet: Fleet = bots
            .into_iter()
            .map(|bot| (bot.config.name.clone(), bot))
            .collect();
        let fleet = Arc::new(fleet);
        self.cache
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(user_id.to_owned(), fleet.clone());
        Ok(fleet)
    }

    /// Console-facing summaries of the acting user's rows. No prompts.
    pub fn configured(&self, user_id: &str) -> Result<Vec<ConfiguredBot>> {
        let rows = self.rows(user_id)?;
        let bots = self.rows_to_bots(rows, user_id)?;
        Ok(bots
            .into_iter()
            .map(|bot| ConfiguredBot {
                id: bot.config.name,
                role: bot.config.role.as_str().to_owned(),
                enabled: bot.config.enabled,
                model: bot.config.model,
                tools: bot.config.tools,
                skills: bot.config.skills,
                require_approval: bot.config.require_approval,
            })
            .collect())
    }

    /// The acting user's effective skill ids: shipped catalog ∪ own rows.
    pub fn effective_skill_ids(&self, user_id: &str) -> Result<Vec<String>> {
        Ok(self.catalog.effective(&self.store, user_id)?.skill_ids())
    }

    /// Drop the cached fleet after a bot write so the next read sees the new
    /// rows.
    pub fn invalidate(&self, user_id: &str) {
        self.cache
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(user_id);
    }

    fn rows_to_bots(&self, rows: Vec<StoredBot>, user_id: &str) -> Result<Vec<LoadedBot>> {
        let mut bots = Vec::with_capacity(rows.len());
        for row in rows {
            if !safe_bot_label(&row.name) {
                return Err(SeraError::Config(format!(
                    "bot '{}' has an invalid id",
                    row.name
                )));
            }
            let mut config: BotConfig = serde_json::from_value(row.config).map_err(|_| {
                SeraError::Config(format!("bot '{}' has an invalid stored config", row.name))
            })?;
            config.name = row.name.clone();
            config.enabled = row.enabled;
            // The console overlay is the admin's alone. Seed templates are
            // raw, so members never inherit it.
            if user_id == self.admin_user_id {
                apply_bot_config_override(&self.config, &mut config);
            }
            bots.push(LoadedBot {
                config,
                prompt: row.prompt,
            });
        }
        Ok(bots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::BotConfigOverlay;
    use crate::config::BotConfig;
    use crate::orchestration::BotRole;

    fn config() -> Config {
        let root = std::env::temp_dir().join(format!("serabot-fleet-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            base_dir: root.canonicalize().unwrap(),
            ..Config::default()
        }
    }

    fn bot(name: &str, role: BotRole, delegates: Vec<String>, enabled: bool) -> LoadedBot {
        LoadedBot {
            config: BotConfig {
                name: name.into(),
                trigger: "command".into(),
                model: None,
                system_prompt: String::new(),
                skills: vec![],
                tools: vec![],
                require_approval: vec![],
                enabled,
                temperature: None,
                role,
                delegates,
            },
            prompt: format!("{name} prompt"),
        }
    }

    fn templates() -> Vec<LoadedBot> {
        vec![
            bot("main", BotRole::Orchestrator, vec!["analyst".into()], true),
            bot("analyst", BotRole::Specialist, vec![], true),
        ]
    }

    fn registry(store: &Store, config: Config) -> FleetRegistry {
        FleetRegistry::new(
            Arc::new(config),
            store.clone(),
            templates(),
            Arc::new(SkillCatalog::load(&Config::default()).unwrap()),
        )
        .unwrap()
    }

    fn member(store: &Store) -> String {
        store
            .create_user("microsoft:fleet-member", "f@example.example", "F")
            .unwrap()
            .id
    }

    /// `serabot fleet reseed` replaces the rows wholesale: an edited prompt, a
    /// narrowed tool list, and a scaffolded bot the templates do not know all
    /// disappear, and the shipped templates are written back.
    #[test]
    fn reseed_replaces_the_rows_with_the_shipped_templates() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let registry = registry(&store, config());
        registry.seed(&admin).unwrap();

        // Customize one row and add a bot the templates do not contain.
        store
            .set_bot_prompt(&admin, "main", "edited by the operator")
            .unwrap();
        store
            .upsert_bot(
                &admin,
                "custom",
                &serde_json::to_value(bot("custom", BotRole::Specialist, vec![], false).config)
                    .unwrap(),
                "custom prompt",
                false,
            )
            .unwrap();

        let written = reseed_user(&store, &admin, &templates()).unwrap();
        assert_eq!(written, 2);

        let rows = store.list_bots(&admin).unwrap();
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["analyst", "main"]);
        let main = rows.iter().find(|row| row.name == "main").unwrap();
        assert_eq!(main.prompt, "main prompt");
        assert!(rows.iter().all(|row| row.name != "custom"));
    }

    #[test]
    fn seeds_each_user_from_templates_once() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let registry = registry(&store, config());
        assert_eq!(registry.seed(&admin).unwrap(), 2);
        assert_eq!(registry.seed(&admin).unwrap(), 0);
        let member = member(&store);
        assert_eq!(registry.seed(&member).unwrap(), 2);
        assert_eq!(store.count_bots(&admin).unwrap(), 2);
        assert_eq!(store.count_bots(&member).unwrap(), 2);
        let admin_main = registry.row(&admin, "main").unwrap().unwrap();
        assert_eq!(admin_main.prompt, "main prompt");
        let member_main = registry.row(&member, "main").unwrap().unwrap();
        assert_eq!(member_main.prompt, "main prompt");
        // Loading seeds lazily too, for accounts created before phase 3 or
        // through a path that did not run the post-login hook.
        let other = store
            .create_user("microsoft:fleet-other", "o@example.example", "O")
            .unwrap()
            .id;
        assert!(registry.load(&other).unwrap().contains_key("main"));
        assert_eq!(store.count_bots(&other).unwrap(), 2);
    }

    #[test]
    fn repairs_empty_prompts_left_by_the_migration() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        store
            .upsert_bot(
                &admin,
                "main",
                &serde_json::to_value(&templates()[0].config).unwrap(),
                "",
                true,
            )
            .unwrap();
        let registry = registry(&store, config());
        assert_eq!(registry.seed(&admin).unwrap(), 0);
        assert_eq!(
            registry.row(&admin, "main").unwrap().unwrap().prompt,
            "main prompt"
        );
    }

    #[test]
    fn fleet_validation_is_per_user() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let member = member(&store);
        let registry = registry(&store, config());
        assert!(registry.load(&admin).is_ok());
        assert!(registry.load(&member).is_ok());

        // Disabling the member's orchestrator breaks only the member's fleet.
        let member_main = store.get_bot(&member, "main").unwrap().unwrap();
        assert!(
            store
                .update_bot(
                    &member,
                    "main",
                    &member_main.config,
                    &member_main.prompt,
                    false
                )
                .unwrap()
        );
        registry.invalidate(&member);
        let error = registry.load(&member).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("exactly one enabled orchestrator"),
            "{error}"
        );
        assert!(registry.load(&admin).is_ok());

        // A delegate that resolves only outside the user's fleet is invalid.
        assert!(
            store
                .update_bot(
                    &member,
                    "main",
                    &member_main.config,
                    &member_main.prompt,
                    true
                )
                .unwrap()
        );
        assert!(store.delete_bot(&member, "analyst").unwrap());
        registry.invalidate(&member);
        let error = registry.load(&member).unwrap_err();
        assert!(error.to_string().contains("delegate analyst"), "{error}");
        assert!(registry.load(&admin).is_ok());
    }

    #[test]
    fn admin_overlay_applies_only_to_the_admin() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let member = member(&store);
        let mut config = config();
        config.bot_config_overrides.insert(
            "main".into(),
            BotConfigOverlay {
                tools: Some(vec!["skill_list".into()]),
                ..BotConfigOverlay::default()
            },
        );
        let registry = registry(&store, config);
        let admin_main = registry.load(&admin).unwrap()["main"].clone();
        assert_eq!(admin_main.config.tools, vec!["skill_list"]);
        let member_main = registry.load(&member).unwrap()["main"].clone();
        assert!(member_main.config.tools.is_empty());
    }

    #[test]
    fn repository_templates_seed_a_valid_fleet() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("repository root")
            .to_path_buf();
        let config = Config {
            base_dir: root.canonicalize().unwrap(),
            ..Config::default()
        };
        let templates =
            crate::config::load_bots(&config, &config.fleet.bots_dir).expect("seed templates");
        assert!(templates.len() >= 12, "the shipped fleet has twelve bots");
        let catalog = Arc::new(SkillCatalog::load(&config).unwrap());
        let store = Store::open_in_memory(384).unwrap();
        let registry =
            FleetRegistry::new(Arc::new(config), store.clone(), templates, catalog).unwrap();
        let admin = store.admin_user_id().unwrap();
        assert_eq!(registry.seed(&admin).unwrap(), 12);
        let fleet = registry.load(&admin).unwrap();
        assert!(fleet["main"].config.enabled);
        assert_eq!(fleet["main"].config.role, BotRole::Orchestrator);
        assert!(!fleet["legal"].config.enabled);
        assert!(!fleet["main"].prompt.is_empty());
        assert!(fleet["main"].prompt.contains("main bot"));
    }

    #[test]
    fn another_users_bot_is_invisible() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let member = member(&store);
        let registry = registry(&store, config());
        store
            .upsert_bot(
                &admin,
                "admin-only",
                &serde_json::to_value(&templates()[1].config).unwrap(),
                "admin only",
                true,
            )
            .unwrap();
        assert!(registry.row(&admin, "admin-only").unwrap().is_some());
        assert!(registry.row(&member, "admin-only").unwrap().is_none());
        assert!(!registry.load(&member).unwrap().contains_key("admin-only"));
    }
}
