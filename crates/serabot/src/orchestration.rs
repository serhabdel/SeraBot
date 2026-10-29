use serde::{Deserialize, Serialize};

use crate::error::{Result, SeraError};

pub const ORCHESTRATOR_BOT: &str = "main";
pub const MAX_HANDOFF_OBJECTIVE_LEN: usize = 2000;
pub const MAX_HANDOFF_MEMORIES: usize = 5;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BotRole {
    #[default]
    Specialist,
    Orchestrator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    Section,
    Group,
    Direct,
    Room,
    Legacy,
}

impl ConversationKind {
    pub fn accepts_turns(self) -> bool {
        matches!(
            self,
            Self::Direct | Self::Room | Self::Legacy
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationScope {
    pub kind: ConversationKind,
    pub slug: Option<String>,
    pub raw_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelClass {
    Routine,
    Synthesis,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Handoff {
    pub id: String,
    pub parent_task_id: String,
    pub conversation_id: String,
    pub from_bot: String,
    pub to_bot: String,
    #[serde(default)]
    pub skill: Option<String>,
    pub objective: String,
    #[serde(default)]
    pub memory_ids: Vec<String>,
    pub model_class: ModelClass,
}

#[derive(Debug, Clone)]
pub struct BotFleetMember {
    pub name: String,
    pub enabled: bool,
    pub role: BotRole,
    pub delegates: Vec<String>,
}

pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() || slug.len() > 32 {
        return Err(SeraError::Validation("invalid conversation slug".into()));
    }
    let mut chars = slug.chars();
    let Some(first) = chars.next() else {
        return Err(SeraError::Validation("invalid conversation slug".into()));
    };
    if !first.is_ascii_lowercase() {
        return Err(SeraError::Validation("invalid conversation slug".into()));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(SeraError::Validation("invalid conversation slug".into()));
    }
    Ok(())
}

pub fn validate_conversation_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':'))
    {
        return Err(SeraError::Validation("invalid conversation id".into()));
    }
    if let Some((prefix, slug)) = id.split_once(':') {
        match prefix {
            "sec" | "grp" | "dir" | "room" => validate_slug(slug)?,
            _ => {}
        }
    }
    Ok(())
}

pub fn parse_conversation_scope(id: &str) -> Result<ConversationScope> {
    validate_conversation_id(id)?;
    let (kind, slug) = if let Some((prefix, slug)) = id.split_once(':') {
        match prefix {
            "sec" => (ConversationKind::Section, Some(slug.to_owned())),
            "grp" => (ConversationKind::Group, Some(slug.to_owned())),
            "dir" => (ConversationKind::Direct, Some(slug.to_owned())),
            "room" => (ConversationKind::Room, Some(slug.to_owned())),
            _ => (ConversationKind::Legacy, None),
        }
    } else {
        (ConversationKind::Legacy, None)
    };
    if let Some(ref slug) = slug {
        validate_slug(slug)?;
    }
    Ok(ConversationScope {
        kind,
        slug,
        raw_id: id.to_owned(),
    })
}

pub fn validate_handoff(h: &Handoff, allowed_skills: &[String]) -> Result<()> {
    if h.from_bot != ORCHESTRATOR_BOT {
        return Err(SeraError::Validation(
            "handoff from_bot must be main".into(),
        ));
    }
    if h.to_bot.is_empty() || h.to_bot == ORCHESTRATOR_BOT {
        return Err(SeraError::Validation("invalid handoff to_bot".into()));
    }
    validate_conversation_id(&h.conversation_id)?;
    let scope = parse_conversation_scope(&h.conversation_id)?;
    if !scope.kind.accepts_turns() {
        return Err(SeraError::Validation(
            "handoff conversation cannot accept turns".into(),
        ));
    }
    if h.objective.is_empty() || h.objective.len() > MAX_HANDOFF_OBJECTIVE_LEN {
        return Err(SeraError::Validation("invalid handoff objective".into()));
    }
    if h.memory_ids.len() > MAX_HANDOFF_MEMORIES {
        return Err(SeraError::Validation("too many handoff memory_ids".into()));
    }
    if let Some(skill) = &h.skill {
        if skill.is_empty() || !allowed_skills.iter().any(|s| s == skill) {
            return Err(SeraError::Validation(
                "handoff skill not allowed for target bot".into(),
            ));
        }
    }
    Ok(())
}

pub fn validate_bot_fleet(members: &[BotFleetMember]) -> Result<()> {
    let enabled: Vec<&BotFleetMember> = members.iter().filter(|m| m.enabled).collect();
    let orchestrators: Vec<&BotFleetMember> = enabled
        .iter()
        .copied()
        .filter(|m| m.role == BotRole::Orchestrator)
        .collect();
    if orchestrators.len() != 1 {
        return Err(SeraError::Config(
            "exactly one enabled orchestrator bot is required".into(),
        ));
    }
    let orchestrator = orchestrators[0];
    if orchestrator.name != ORCHESTRATOR_BOT {
        return Err(SeraError::Config(
            "the enabled orchestrator must be named main".into(),
        ));
    }

    let enabled_names: std::collections::HashSet<&str> =
        enabled.iter().map(|m| m.name.as_str()).collect();

    for member in members {
        if member.role == BotRole::Specialist && !member.delegates.is_empty() {
            return Err(SeraError::Config(format!(
                "specialist {} must not declare delegates",
                member.name
            )));
        }
    }

    for delegate in &orchestrator.delegates {
        if !enabled_names.contains(delegate.as_str()) {
            return Err(SeraError::Config(format!(
                "orchestrator delegate {delegate} is not an enabled bot"
            )));
        }
        if delegate == ORCHESTRATOR_BOT {
            return Err(SeraError::Config(
                "orchestrator cannot delegate to itself".into(),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_slug() {
        assert!(validate_slug("Bad").is_err());
        assert!(validate_slug("").is_err());
        assert!(validate_slug(&"a".repeat(33)).is_err());
        assert!(validate_slug("valid-slug-1").is_ok());
    }

    #[test]
    fn section_cannot_accept_turns() {
        let scope = parse_conversation_scope("sec:finance").unwrap();
        assert_eq!(scope.kind, ConversationKind::Section);
        assert!(!scope.kind.accepts_turns());
        let handoff = Handoff {
            id: "h1".into(),
            parent_task_id: "t1".into(),
            conversation_id: "sec:finance".into(),
            from_bot: "main".into(),
            to_bot: "analyst".into(),
            skill: None,
            objective: "review".into(),
            memory_ids: vec![],
            model_class: ModelClass::Routine,
        };
        assert!(validate_handoff(&handoff, &[]).is_err());
    }

    #[test]
    fn legacy_owner_id_validates() {
        validate_conversation_id("owner").unwrap();
        let scope = parse_conversation_scope("owner").unwrap();
        assert_eq!(scope.kind, ConversationKind::Legacy);
        assert!(scope.kind.accepts_turns());
    }

    #[test]
    fn rejects_two_orchestrators() {
        let members = vec![
            BotFleetMember {
                name: "main".into(),
                enabled: true,
                role: BotRole::Orchestrator,
                delegates: vec!["analyst".into()],
            },
            BotFleetMember {
                name: "analyst".into(),
                enabled: true,
                role: BotRole::Orchestrator,
                delegates: vec![],
            },
        ];
        assert!(validate_bot_fleet(&members).is_err());
    }

    #[test]
    fn rejects_unknown_or_disabled_delegate() {
        let members = vec![
            BotFleetMember {
                name: "main".into(),
                enabled: true,
                role: BotRole::Orchestrator,
                delegates: vec!["legal".into()],
            },
            BotFleetMember {
                name: "legal".into(),
                enabled: false,
                role: BotRole::Specialist,
                delegates: vec![],
            },
        ];
        assert!(validate_bot_fleet(&members).is_err());

        let members = vec![BotFleetMember {
            name: "main".into(),
            enabled: true,
            role: BotRole::Orchestrator,
            delegates: vec!["missing".into()],
        }];
        assert!(validate_bot_fleet(&members).is_err());
    }

    #[test]
    fn rejects_specialist_with_delegates() {
        let members = vec![
            BotFleetMember {
                name: "main".into(),
                enabled: true,
                role: BotRole::Orchestrator,
                delegates: vec!["analyst".into()],
            },
            BotFleetMember {
                name: "analyst".into(),
                enabled: true,
                role: BotRole::Specialist,
                delegates: vec!["maker".into()],
            },
        ];
        assert!(validate_bot_fleet(&members).is_err());
    }

    #[test]
    fn accepts_valid_fleet() {
        let members = vec![
            BotFleetMember {
                name: "main".into(),
                enabled: true,
                role: BotRole::Orchestrator,
                delegates: vec!["analyst".into(), "maker".into()],
            },
            BotFleetMember {
                name: "analyst".into(),
                enabled: true,
                role: BotRole::Specialist,
                delegates: vec![],
            },
            BotFleetMember {
                name: "maker".into(),
                enabled: true,
                role: BotRole::Specialist,
                delegates: vec![],
            },
        ];
        validate_bot_fleet(&members).unwrap();
    }
}
