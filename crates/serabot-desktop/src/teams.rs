//! Team-room helpers for the console.
//!
//! The wire types and the client calls live in [`crate::api`]. This module
//! holds the small, visual-free helpers the UI needs: membership maths, the
//! list of bots that can still join, and the fixed class names from the
//! design system (§3.4 of `docs/desktop-redesign.md`).

use crate::{BotSummary, Team, TeamMember, TeamMode};

/// Fixed design-system class names used by the team surfaces.
pub const AVATAR_STACK_CLASS: &str = "avatar-stack";
pub const TEAM_CHIP_CLASS: &str = "team-chip";
pub const TEAM_CHIP_REMOVE_CLASS: &str = "team-chip remove";
pub const ROSTER_CLASS: &str = "roster";
pub const ROSTER_ROW_CLASS: &str = "roster-row";

/// True when the conversation id is a room, the only kind with a team.
pub fn is_room(conversation: &str) -> bool {
    crate::is_room_conversation(conversation)
}

/// The bot ids of a team, in the order the server returned them.
pub fn member_ids(team: &Team) -> Vec<String> {
    team.members
        .iter()
        .map(|member| member.bot.clone())
        .collect()
}

/// The member entry for one bot id.
pub fn member<'a>(team: &'a Team, bot: &str) -> Option<&'a TeamMember> {
    team.members.iter().find(|member| member.bot == bot)
}

/// Enabled bots that are not in the team yet, in fleet order.
pub fn available_bots(bots: &[BotSummary], team: &Team) -> Vec<String> {
    bots.iter()
        .filter(|bot| bot.enabled)
        .filter(|bot| member(team, &bot.id).is_none())
        .map(|bot| bot.id.clone())
        .collect()
}

/// The member list after adding one bot. `None` when the bot cannot join:
/// it is already a member, it is not a valid bot id, or the team is full.
pub fn with_member(team: &Team, bot: &str) -> Option<Vec<String>> {
    if !crate::valid_team_member(bot) || member(team, bot).is_some() {
        return None;
    }
    let mut members = member_ids(team);
    if members.len() >= crate::MAX_TEAM_MEMBERS {
        return None;
    }
    members.push(bot.to_owned());
    Some(members)
}

/// The member list after removing one bot. `None` when the bot is not a
/// member or when the team would become empty.
pub fn without_member(team: &Team, bot: &str) -> Option<Vec<String>> {
    member(team, bot)?;
    let members: Vec<String> = member_ids(team)
        .into_iter()
        .filter(|member| member != bot)
        .collect();
    if members.is_empty() {
        return None;
    }
    Some(members)
}

/// The team mode as the owner sees it.
pub fn mode_label(mode: TeamMode) -> &'static str {
    mode.label()
}

/// The members that are live right now.
pub fn live_members(team: &Team) -> Vec<&TeamMember> {
    team.members.iter().filter(|member| member.live).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BotSummary;

    fn team_member(bot: &str, live: bool) -> TeamMember {
        TeamMember {
            bot: bot.into(),
            role: "specialist".into(),
            enabled: true,
            live,
        }
    }

    fn team() -> Team {
        Team {
            conversation_id: "room:review".into(),
            mode: TeamMode::Selected,
            members: vec![team_member("analyst", true), team_member("maker", false)],
        }
    }

    fn bot(id: &str, enabled: bool) -> BotSummary {
        BotSummary {
            id: id.into(),
            role: "specialist".into(),
            enabled,
            live: true,
            display_name: Some(id.into()),
            description: None,
            avatar: None,
            model: None,
            tools: Vec::new(),
            skills: Vec::new(),
            require_approval: Vec::new(),
        }
    }

    #[test]
    fn membership_maths_keep_the_server_order() {
        let team = team();
        assert_eq!(
            member_ids(&team),
            vec!["analyst".to_string(), "maker".to_string()]
        );
        assert!(member(&team, "maker").is_some());
        assert!(member(&team, "watcher").is_none());
        assert_eq!(live_members(&team).len(), 1);
        assert_eq!(mode_label(team.mode), "Selected");
    }

    #[test]
    fn add_and_remove_ignore_no_ops_and_invalid_bots() {
        let team = team();
        assert_eq!(
            with_member(&team, "watcher"),
            Some(vec![
                "analyst".to_string(),
                "maker".to_string(),
                "watcher".to_string()
            ])
        );
        assert_eq!(with_member(&team, "analyst"), None);
        assert_eq!(with_member(&team, "main"), None);
        assert_eq!(with_member(&team, "Analyst"), None);
        assert_eq!(
            without_member(&team, "maker"),
            Some(vec!["analyst".to_string()])
        );
        assert_eq!(without_member(&team, "watcher"), None);
        let solo = Team {
            conversation_id: "room:solo".into(),
            mode: TeamMode::All,
            members: vec![team_member("analyst", true)],
        };
        assert_eq!(without_member(&solo, "analyst"), None);
    }

    #[test]
    fn available_bots_skip_disabled_and_current_members() {
        let team = team();
        let bots = vec![
            bot("analyst", true),
            bot("maker", true),
            bot("watcher", true),
            bot("idle", false),
        ];
        assert_eq!(available_bots(&bots, &team), vec!["watcher".to_string()]);
    }

    #[test]
    fn only_rooms_carry_a_team() {
        assert!(is_room("room:review"));
        assert!(!is_room("windows-owner"));
        assert!(!is_room("grp:ops"));
        assert!(!is_room("dir:analyst"));
    }
}
