//! Shipped bot templates, prompts, and skill packs compiled into the
//! `serabot` binary as a fallback so upgraded servers that only replace the
//! binary (without copying new `bots/`, `prompts/`, or `skills/` files into
//! `/etc/serabot`) still serve the full fleet and catalog.

pub struct BuiltinBot {
    pub name: &'static str,
    pub toml: &'static str,
    pub prompt: &'static str,
}

pub struct BuiltinSkill {
    pub id: &'static str,
    pub pack: Option<&'static str>,
    pub markdown: &'static str,
}

macro_rules! builtin_bot {
    ($name:literal) => {
        BuiltinBot {
            name: $name,
            toml: include_str!(concat!("../../../bots/", $name, ".toml")),
            prompt: include_str!(concat!("../../../prompts/", $name, ".txt")),
        }
    };
}

macro_rules! builtin_skill {
    ($id:literal) => {
        BuiltinSkill {
            id: $id,
            pack: None,
            markdown: include_str!(concat!("../../../skills/", $id, "/SKILL.md")),
        }
    };
    ($pack:literal, $id:literal) => {
        BuiltinSkill {
            id: $id,
            pack: Some($pack),
            markdown: include_str!(concat!(
                "../../../skills/packs/",
                $pack,
                "/",
                $id,
                "/SKILL.md"
            )),
        }
    };
}

pub const BUILTIN_BOTS: &[BuiltinBot] = &[
    builtin_bot!("analyst"),
    builtin_bot!("auditor"),
    builtin_bot!("comptable"),
    builtin_bot!("courier"),
    builtin_bot!("hostinger"),
    builtin_bot!("inbox"),
    builtin_bot!("jobfinder"),
    builtin_bot!("legal"),
    builtin_bot!("main"),
    builtin_bot!("mailbox"),
    builtin_bot!("maker"),
    builtin_bot!("marketer"),
    builtin_bot!("markets"),
    builtin_bot!("planner"),
    builtin_bot!("product"),
    builtin_bot!("researcher"),
    builtin_bot!("support"),
    builtin_bot!("translator"),
    builtin_bot!("tutor"),
    builtin_bot!("watcher"),
    builtin_bot!("whatsapp"),
    builtin_bot!("writer"),
];

pub const BUILTIN_SKILLS: &[BuiltinSkill] = &[
    builtin_skill!("ask"),
    builtin_skill!("audio"),
    builtin_skill!("audit-checklist"),
    builtin_skill!("bank-reconciliation"),
    builtin_skill!("brief"),
    builtin_skill!("campaign-plan"),
    builtin_skill!("contract-review"),
    builtin_skill!("copy"),
    builtin_skill!("deadlines"),
    builtin_skill!("deck"),
    builtin_skill!("design-brief"),
    builtin_skill!("elevenlabs"),
    builtin_skill!("email-triage"),
    builtin_skill!("equity-research"),
    builtin_skill!("financial-analysis"),
    builtin_skill!("firecrawl"),
    builtin_skill!("legal-rag"),
    builtin_skill!("ma-payroll"),
    builtin_skill!("ma-tax-2026"),
    builtin_skill!("mail"),
    builtin_skill!("messaging-etiquette"),
    builtin_skill!("monitoring-playbook"),
    builtin_skill!("onedrive"),
    builtin_skill!("onepager"),
    builtin_skill!("pcm-cgnc"),
    builtin_skill!("scheduler"),
    builtin_skill!("skill-creator"),
    builtin_skill!("social-extract"),
    builtin_skill!("social-posts"),
    builtin_skill!("synthesis"),
    builtin_skill!("timesheets"),
    builtin_skill!("transcript"),
    builtin_skill!("translate-it-fr"),
    builtin_skill!("triage"),
    builtin_skill!("write"),
    builtin_skill!("office", "design"),
    builtin_skill!("office", "docx"),
    builtin_skill!("office", "pptx"),
    builtin_skill!("office", "xlsx"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    #[test]
    fn builtin_bots_cover_every_shipped_manifest() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut on_disk = BTreeSet::new();
        for entry in std::fs::read_dir(root.join("bots")).expect("bots dir") {
            let path = entry.expect("bot entry").path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                let stem = path.file_stem().unwrap().to_str().unwrap().to_owned();
                assert!(
                    root.join("prompts").join(format!("{stem}.txt")).is_file(),
                    "missing prompt file for {stem}"
                );
                on_disk.insert(stem);
            }
        }
        let embedded: BTreeSet<String> = BUILTIN_BOTS.iter().map(|b| b.name.to_owned()).collect();
        assert_eq!(embedded.len(), BUILTIN_BOTS.len(), "duplicate builtin bot");
        assert_eq!(embedded, on_disk, "BUILTIN_BOTS out of sync with bots/");
        assert_eq!(BUILTIN_BOTS.len(), 22);
    }

    #[test]
    fn builtin_skills_cover_every_shipped_skill() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let skills_dir = root.join("skills");
        let mut on_disk = BTreeSet::new();
        for entry in std::fs::read_dir(&skills_dir).expect("skills dir") {
            let path = entry.expect("skill entry").path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().unwrap().to_str().unwrap();
            if name.starts_with('.') {
                continue;
            }
            if name == "packs" {
                for pack in std::fs::read_dir(&path).expect("packs dir") {
                    let pack_path = pack.expect("pack entry").path();
                    if !pack_path.is_dir() {
                        continue;
                    }
                    let pack_name = pack_path.file_name().unwrap().to_str().unwrap().to_owned();
                    for skill in std::fs::read_dir(&pack_path).expect("pack skills") {
                        let skill_path = skill.expect("skill").path();
                        if skill_path.join("SKILL.md").is_file() {
                            let id = skill_path.file_name().unwrap().to_str().unwrap().to_owned();
                            on_disk.insert((id, Some(pack_name.clone())));
                        }
                    }
                }
                continue;
            }
            if path.join("SKILL.md").is_file() {
                on_disk.insert((name.to_owned(), None));
            }
        }
        let embedded: BTreeSet<(String, Option<String>)> = BUILTIN_SKILLS
            .iter()
            .map(|s| (s.id.to_owned(), s.pack.map(str::to_owned)))
            .collect();
        assert_eq!(
            embedded.len(),
            BUILTIN_SKILLS.len(),
            "duplicate builtin skill"
        );
        assert_eq!(embedded, on_disk, "BUILTIN_SKILLS out of sync with skills/");
        assert_eq!(BUILTIN_SKILLS.len(), 39);
    }
}
