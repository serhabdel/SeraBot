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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::{BotConfig, Config, ConfiguredBot, LoadedBot, apply_bot_config_override};
use crate::error::{Result, SeraError};
use crate::orchestration::{BotFleetMember, safe_bot_label, validate_bot_fleet};
use crate::skills::SkillCatalog;
use crate::store::{Store, StoredBot};
use crate::util::sha256_hex;

/// A user's loaded fleet, keyed by bot id.
pub type Fleet = HashMap<String, LoadedBot>;

/// Digest of a prompt text. CRLF is stripped first, so a Windows checkout
/// hashes the same as the committed LF file.
fn prompt_digest(text: &str) -> String {
    sha256_hex(text.replace('\r', ""))
}

/// Prompt digests shipped before offering records existed (the 0.18
/// templates), by bot. A row without a record whose prompt is one of these is
/// an unedited shipped prompt and may be upgraded; any other text is the
/// owner's and is kept.
const SHIPPED_BEFORE_OFFERINGS: &[(&str, &str)] = &[
    (
        "main",
        "efd3545f42d1951f1a9565f85124ce2b23830fa809babf198138ac7dd92259b3",
    ),
    (
        "analyst",
        "8c6a7ff1dfaeecc9f8bd1f68ab13f62e06e0e79bbc1a8d3cf96c3916e80f5080",
    ),
    (
        "maker",
        "4380e832859c3b0c2c450128034c0fb5fd7ab68730a5d5e205e1164f5b6062a9",
    ),
    (
        "legal",
        "48cabdfe757d189aacfc5372f9f538b667157054591d7182c6c57921811505d4",
    ),
    (
        "watcher",
        "bf942e84859332c591d6dd26514f596e2cf31830580a4f4cb2b41b8f1bbe5506",
    ),
];

/// What the shipped template for one bot offered the last time it was seeded
/// for a user. Entries stay in the record after the owner removes them, so a
/// later upgrade only adds what is new since this offering.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct BotOffering {
    #[serde(default)]
    tools: BTreeSet<String>,
    #[serde(default)]
    skills: BTreeSet<String>,
    #[serde(default)]
    delegates: BTreeSet<String>,
    /// Prompt digest (see `prompt_digest`) of the text last offered.
    #[serde(default)]
    prompt_sha256: String,
}

impl BotOffering {
    fn of(template: &LoadedBot) -> Self {
        Self {
            tools: template.config.tools.iter().cloned().collect(),
            skills: template.config.skills.iter().cloned().collect(),
            delegates: template.config.delegates.iter().cloned().collect(),
            prompt_sha256: prompt_digest(&template.prompt),
        }
    }
}

/// One user's offering records, by bot name. Stored in `kv` under the user id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Offerings {
    #[serde(default)]
    bots: BTreeMap<String, BotOffering>,
}

/// Append the entries a template gained since its last offering. An entry the
/// baseline already covers is never added again, so a removal stays removed.
fn add_gained(current: &mut Vec<String>, template: &[String], baseline: &BTreeSet<String>) {
    for entry in template {
        if !baseline.contains(entry) && !current.contains(entry) {
            current.push(entry.clone());
        }
    }
}

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
    let mut offerings = Offerings::default();
    for template in templates {
        store.upsert_bot(
            user_id,
            &template.config.name,
            &serde_json::to_value(&template.config)?,
            &template.prompt,
            template.config.enabled,
        )?;
        offerings
            .bots
            .insert(template.config.name.clone(), BotOffering::of(template));
    }
    store.set_fleet_offerings(user_id, &serde_json::to_string(&offerings)?)?;
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
    /// One lock per user, so concurrent seeds for one user run in turn.
    user_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Users whose upgrade already ran in this process.
    upgraded: Arc<Mutex<HashSet<String>>>,
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
            user_locks: Arc::new(Mutex::new(HashMap::new())),
            upgraded: Arc::new(Mutex::new(HashSet::new())),
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

    /// Bring the user's fleet up to the shipped templates. Idempotent; returns
    /// the number of bot rows inserted.
    ///
    /// The work runs once per user per process: the first call holds that
    /// user's lock and upgrades, and later calls return at once until
    /// [`Self::invalidate`] clears the mark. Per template, against the user's
    /// offering record:
    /// - no row and no record: insert it, so a new default bot reaches an
    ///   existing user. A row the owner deleted after an offering stays gone.
    /// - a row: add the tools, skills, and delegates the template gained since
    ///   the baseline, never re-adding one the owner removed. Replace the
    ///   prompt only while the row still holds a shipped text (the recorded
    ///   digest, an empty prompt, or a pre-record 0.18 text); an owner's edit
    ///   is kept.
    ///
    /// Nothing is ever deleted, and the record is written only when it
    /// changes. Empty prompts left by migration 0010 are repaired here too,
    /// since SQL cannot read the prompt files. An unreadable record is logged
    /// and replaced by defaults; a row whose config cannot be parsed is logged
    /// and skipped. Neither fails the fleet.
    pub fn seed(&self, user_id: &str) -> Result<usize> {
        let user_lock = self.user_lock(user_id);
        let _guard = user_lock.lock().unwrap_or_else(|error| error.into_inner());
        let done = self
            .upgraded
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(user_id);
        if done {
            return Ok(0);
        }
        let inserted = self.upgrade(user_id)?;
        self.upgraded
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(user_id.to_owned());
        Ok(inserted)
    }

    fn user_lock(&self, user_id: &str) -> Arc<Mutex<()>> {
        self.user_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(user_id.to_owned())
            .or_default()
            .clone()
    }

    fn upgrade(&self, user_id: &str) -> Result<usize> {
        let rows = self.store.list_bots(user_id)?;
        let mut offerings: Offerings = match self.store.fleet_offerings(user_id)? {
            Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|error| {
                warn!(%error, "fleet offering record is unreadable; using defaults");
                Offerings::default()
            }),
            None => Offerings::default(),
        };
        let original = offerings.clone();
        let mut inserted = 0;
        let mut rows_changed = false;
        for template in self.templates.iter() {
            let name = template.config.name.as_str();
            let record = offerings.bots.get(name).cloned();
            let Some(row) = rows.iter().find(|row| row.name == name) else {
                if record.is_none() {
                    self.store.upsert_bot(
                        user_id,
                        name,
                        &serde_json::to_value(&template.config)?,
                        &template.prompt,
                        template.config.enabled,
                    )?;
                    offerings
                        .bots
                        .insert(name.to_owned(), BotOffering::of(template));
                    inserted += 1;
                }
                continue;
            };
            let mut config: BotConfig = match serde_json::from_value(row.config.clone()) {
                Ok(config) => config,
                Err(error) => {
                    warn!(bot = name, %error, "bot has an invalid stored config; not upgrading it");
                    continue;
                }
            };
            // Without a record the row itself is the baseline, so nothing it
            // already holds is treated as new. The cost: a tool, skill, or
            // delegate the owner removed before the first upgrade may be added
            // back once; after this run the record keeps it removed.
            let baseline = record.unwrap_or_else(|| BotOffering {
                tools: config.tools.iter().cloned().collect(),
                skills: config.skills.iter().cloned().collect(),
                delegates: config.delegates.iter().cloned().collect(),
                prompt_sha256: String::new(),
            });
            let row_digest = prompt_digest(&row.prompt);
            let prompt_is_shipped = row.prompt.is_empty()
                || baseline.prompt_sha256 == row_digest
                || SHIPPED_BEFORE_OFFERINGS.contains(&(name, row_digest.as_str()));
            let prompt_changed =
                prompt_is_shipped && prompt_digest(&row.prompt) != prompt_digest(&template.prompt);
            let counts = |config: &BotConfig| {
                (
                    config.tools.len(),
                    config.skills.len(),
                    config.delegates.len(),
                )
            };
            let before = counts(&config);
            add_gained(&mut config.tools, &template.config.tools, &baseline.tools);
            add_gained(
                &mut config.skills,
                &template.config.skills,
                &baseline.skills,
            );
            add_gained(
                &mut config.delegates,
                &template.config.delegates,
                &baseline.delegates,
            );
            if counts(&config) != before || prompt_changed {
                let prompt = if prompt_changed {
                    template.prompt.as_str()
                } else {
                    row.prompt.as_str()
                };
                self.store.update_bot(
                    user_id,
                    name,
                    &serde_json::to_value(&config)?,
                    prompt,
                    row.enabled,
                )?;
                rows_changed = true;
            }
            // The record keeps everything offered before, plus this template.
            let mut next = BotOffering::of(template);
            next.tools.extend(baseline.tools);
            next.skills.extend(baseline.skills);
            next.delegates.extend(baseline.delegates);
            offerings.bots.insert(name.to_owned(), next);
        }
        if offerings != original {
            self.store
                .set_fleet_offerings(user_id, &serde_json::to_string(&offerings)?)?;
        }
        if inserted > 0 || rows_changed {
            self.evict(user_id);
        }
        Ok(inserted)
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
        self.evict(user_id);
        self.upgraded
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(user_id);
    }

    /// Drop the cached fleet only; the upgrade mark stays set.
    fn evict(&self, user_id: &str) {
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
        // `analyst` is a template the row-only user never had: it is inserted.
        assert_eq!(registry.seed(&admin).unwrap(), 1);
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
        assert!(templates.len() >= 5, "the shipped fleet has five bots");
        let catalog = Arc::new(SkillCatalog::load(&config).unwrap());
        let store = Store::open_in_memory(384).unwrap();
        let registry =
            FleetRegistry::new(Arc::new(config), store.clone(), templates, catalog).unwrap();
        let admin = store.admin_user_id().unwrap();
        assert_eq!(registry.seed(&admin).unwrap(), 5);
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

    /// A `main` + `analyst` fleet whose `main` offers the given tools and
    /// prompt, for simulating successive template releases.
    fn versioned(tools: &[&str], prompt: &str) -> Vec<LoadedBot> {
        let mut main = bot("main", BotRole::Orchestrator, vec!["analyst".into()], true);
        main.config.tools = tools.iter().map(|tool| (*tool).to_owned()).collect();
        main.prompt = prompt.into();
        vec![main, bot("analyst", BotRole::Specialist, vec![], true)]
    }

    fn registry_with(store: &Store, templates: Vec<LoadedBot>) -> FleetRegistry {
        FleetRegistry::new(
            Arc::new(config()),
            store.clone(),
            templates,
            Arc::new(SkillCatalog::load(&Config::default()).unwrap()),
        )
        .unwrap()
    }

    fn config_of(store: &Store, user_id: &str, name: &str) -> BotConfig {
        let row = store.get_bot(user_id, name).unwrap().unwrap();
        serde_json::from_value(row.config).unwrap()
    }

    /// A pre-0.19 `main` row with no offering record gains the local folder
    /// tools; a text the registry does not recognize is the owner's and stays.
    #[test]
    fn an_existing_row_without_a_record_gains_the_new_tools() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let old = versioned(&["delegate", "memory_save"], "old prompt");
        store
            .upsert_bot(
                &admin,
                "main",
                &serde_json::to_value(&old[0].config).unwrap(),
                "old prompt",
                true,
            )
            .unwrap();
        let registry = registry_with(
            &store,
            versioned(
                &[
                    "delegate",
                    "memory_save",
                    "local_list",
                    "local_read",
                    "local_write",
                ],
                "new prompt",
            ),
        );
        assert_eq!(registry.seed(&admin).unwrap(), 1);
        assert_eq!(
            config_of(&store, &admin, "main").tools,
            vec![
                "delegate",
                "memory_save",
                "local_list",
                "local_read",
                "local_write"
            ]
        );
        assert_eq!(
            store.get_bot(&admin, "main").unwrap().unwrap().prompt,
            "old prompt"
        );
    }

    /// A tool the owner removed after an offering is not added back, while a
    /// tool the template gains later is.
    #[test]
    fn a_tool_the_owner_removed_stays_removed() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        registry_with(&store, versioned(&["delegate", "memory_save"], "v1"))
            .seed(&admin)
            .unwrap();
        let mut config = config_of(&store, &admin, "main");
        config.tools.retain(|tool| tool != "memory_save");
        let row = store.get_bot(&admin, "main").unwrap().unwrap();
        store
            .update_bot(
                &admin,
                "main",
                &serde_json::to_value(&config).unwrap(),
                &row.prompt,
                true,
            )
            .unwrap();

        let next = registry_with(
            &store,
            versioned(&["delegate", "memory_save", "local_list"], "v1"),
        );
        next.seed(&admin).unwrap();
        assert_eq!(
            config_of(&store, &admin, "main").tools,
            vec!["delegate", "local_list"]
        );
    }

    /// An unedited shipped prompt is replaced by the new one; an owner's edit
    /// is kept, and one user's upgrade never touches another user's rows.
    #[test]
    fn an_unedited_prompt_upgrades_and_an_edited_one_is_kept() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let member = member(&store);
        registry_with(&store, versioned(&["delegate"], "v1 prompt"))
            .seed(&admin)
            .unwrap();
        let v1 = registry_with(&store, versioned(&["delegate"], "v1 prompt"));
        v1.seed(&member).unwrap();
        store
            .set_bot_prompt(&member, "main", "the member's own wording")
            .unwrap();

        registry_with(&store, versioned(&["delegate"], "v2 prompt"))
            .seed(&admin)
            .unwrap();
        assert_eq!(
            store.get_bot(&admin, "main").unwrap().unwrap().prompt,
            "v2 prompt"
        );
        assert_eq!(
            store.get_bot(&member, "main").unwrap().unwrap().prompt,
            "the member's own wording"
        );
    }

    /// A template bot the user lacks is inserted, but a bot the owner deleted
    /// after its offering is not brought back.
    #[test]
    fn a_new_template_bot_reaches_existing_users_and_a_deleted_one_stays_deleted() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        registry_with(&store, versioned(&[], "v1"))
            .seed(&admin)
            .unwrap();
        store.delete_bot(&admin, "analyst").unwrap();

        let mut templates = versioned(&[], "v1");
        templates.push(bot("watcher", BotRole::Specialist, vec![], true));
        let next = registry_with(&store, templates);
        assert_eq!(next.seed(&admin).unwrap(), 1);
        assert!(store.get_bot(&admin, "watcher").unwrap().is_some());
        assert!(store.get_bot(&admin, "analyst").unwrap().is_none());
    }

    /// Once upgraded, a second run writes nothing: same rows, same timestamps,
    /// same record.
    #[test]
    fn running_the_upgrade_twice_is_a_no_op() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let old = versioned(&["delegate"], "old prompt");
        store
            .upsert_bot(
                &admin,
                "main",
                &serde_json::to_value(&old[0].config).unwrap(),
                "old prompt",
                true,
            )
            .unwrap();
        let registry = registry_with(&store, versioned(&["delegate", "local_list"], "new"));
        assert_eq!(registry.seed(&admin).unwrap(), 1);
        let rows = store.list_bots(&admin).unwrap();
        let record = store.fleet_offerings(&admin).unwrap();
        assert!(record.is_some());

        assert_eq!(registry.seed(&admin).unwrap(), 0);
        let again = store.list_bots(&admin).unwrap();
        let names = |rows: &[StoredBot]| {
            rows.iter()
                .map(|row| {
                    (
                        row.name.clone(),
                        row.config.to_string(),
                        row.prompt.clone(),
                        row.updated_at,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&rows), names(&again));
        assert_eq!(store.fleet_offerings(&admin).unwrap(), record);
    }

    /// The shipped `main` template gains the local folder tools and the
    /// telegram document tool; a pre-0.19 row (no record, 0.18 tools, and a
    /// prompt the registry does not know) keeps its text.
    #[test]
    fn repository_templates_upgrade_a_pre_0_19_main_row() {
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
        let shipped = templates
            .iter()
            .find(|bot| bot.config.name == "main")
            .unwrap()
            .config
            .clone();
        let old_tools: Vec<String> = shipped
            .tools
            .iter()
            .filter(|tool| !tool.starts_with("local_") && tool.as_str() != "telegram_send_document")
            .cloned()
            .collect();
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let old = BotConfig {
            tools: old_tools,
            ..shipped
        };
        store
            .upsert_bot(
                &admin,
                "main",
                &serde_json::to_value(&old).unwrap(),
                "a pre-0.19 prompt",
                true,
            )
            .unwrap();
        let catalog = Arc::new(SkillCatalog::load(&config).unwrap());
        let registry =
            FleetRegistry::new(Arc::new(config), store.clone(), templates, catalog).unwrap();
        registry.seed(&admin).unwrap();
        let tools = config_of(&store, &admin, "main").tools;
        for tool in [
            "local_list",
            "local_read",
            "local_write",
            "telegram_send_document",
        ] {
            assert!(tools.iter().any(|name| name == tool), "{tool}");
        }
        assert_eq!(
            store.get_bot(&admin, "main").unwrap().unwrap().prompt,
            "a pre-0.19 prompt"
        );
    }

    /// A template bot added to `main.delegates` reaches an existing user: the
    /// bot row is inserted and `main` gains the delegate.
    #[test]
    fn a_new_template_bot_joins_an_existing_users_main_delegates() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        registry_with(&store, versioned(&[], "v1"))
            .seed(&admin)
            .unwrap();
        assert_eq!(config_of(&store, &admin, "main").delegates, vec!["analyst"]);

        let mut templates = versioned(&[], "v1");
        templates[0].config.delegates.push("watcher".into());
        templates.push(bot("watcher", BotRole::Specialist, vec![], true));
        assert_eq!(registry_with(&store, templates).seed(&admin).unwrap(), 1);
        assert!(store.get_bot(&admin, "watcher").unwrap().is_some());
        assert_eq!(
            config_of(&store, &admin, "main").delegates,
            vec!["analyst", "watcher"]
        );
    }

    /// The 0.18 main prompt, copied byte for byte from `release/0.19.0`, is
    /// recognised as shipped with or without CRLF line endings and is replaced.
    #[test]
    fn the_shipped_0_18_main_prompt_is_replaced_with_or_without_crlf() {
        const MAIN_0_18: &str = include_str!("fixtures/main_0_18_prompt.txt");
        for prompt in [MAIN_0_18.to_owned(), MAIN_0_18.replace('\n', "\r\n")] {
            let store = Store::open_in_memory(384).unwrap();
            let admin = store.admin_user_id().unwrap();
            let old = versioned(&["delegate"], "x");
            store
                .upsert_bot(
                    &admin,
                    "main",
                    &serde_json::to_value(&old[0].config).unwrap(),
                    &prompt,
                    true,
                )
                .unwrap();
            registry_with(&store, versioned(&["delegate"], "v2 prompt"))
                .seed(&admin)
                .unwrap();
            assert_eq!(
                store.get_bot(&admin, "main").unwrap().unwrap().prompt,
                "v2 prompt"
            );
        }
    }

    /// An unreadable offering record is logged and replaced by defaults; the
    /// fleet still upgrades and the record is valid JSON afterwards.
    #[test]
    fn an_unreadable_offering_record_does_not_fail_the_fleet() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        registry_with(&store, versioned(&["delegate"], "v1"))
            .seed(&admin)
            .unwrap();
        store.set_fleet_offerings(&admin, "not json {").unwrap();

        let next = registry_with(&store, versioned(&["delegate", "local_list"], "v1"));
        assert_eq!(next.seed(&admin).unwrap(), 0);
        assert_eq!(
            config_of(&store, &admin, "main").tools,
            vec!["delegate", "local_list"]
        );
        let record = store.fleet_offerings(&admin).unwrap().unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&record).is_ok());
    }

    /// A row whose stored config cannot be parsed is skipped with a warning:
    /// its text is left alone, other bots still upgrade, and seed succeeds.
    #[test]
    fn an_unparseable_row_is_skipped_and_the_rest_upgrade() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        store
            .upsert_bot(
                &admin,
                "main",
                &serde_json::json!({"not": "a bot config"}),
                "owner text",
                true,
            )
            .unwrap();
        let registry = registry_with(&store, versioned(&["delegate"], "v2"));
        assert_eq!(registry.seed(&admin).unwrap(), 1);
        let main = store.get_bot(&admin, "main").unwrap().unwrap();
        assert_eq!(main.config, serde_json::json!({"not": "a bot config"}));
        assert_eq!(main.prompt, "owner text");
        assert!(store.get_bot(&admin, "analyst").unwrap().is_some());
    }

    /// Concurrent seeds for one user serialize: exactly one of them inserts
    /// the missing template bot.
    #[test]
    fn concurrent_seeds_for_one_user_insert_each_bot_once() {
        let path =
            std::env::temp_dir().join(format!("serabot-fleet-race-{}.db", uuid::Uuid::new_v4()));
        let store = Store::open(&path, 384, 5000).unwrap();
        let admin = store.admin_user_id().unwrap();
        let registry = registry_with(&store, versioned(&[], "v1"));
        let total: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let registry = registry.clone();
                    let admin = admin.clone();
                    scope.spawn(move || registry.seed(&admin).unwrap())
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .sum()
        });
        assert_eq!(total, 2);
        assert_eq!(store.count_bots(&admin).unwrap(), 2);
    }

    /// The upgrade runs once per user per process: the mark is set after the
    /// first seed and cleared by `invalidate`.
    #[test]
    fn the_upgrade_runs_once_until_invalidated() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let registry = registry_with(&store, versioned(&[], "v1"));
        registry.seed(&admin).unwrap();
        assert!(registry.upgraded.lock().unwrap().contains(&admin));
        registry.invalidate(&admin);
        assert!(!registry.upgraded.lock().unwrap().contains(&admin));
        assert_eq!(registry.seed(&admin).unwrap(), 0);
        assert!(registry.upgraded.lock().unwrap().contains(&admin));
    }
}
