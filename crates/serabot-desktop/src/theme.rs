//! The console's status language, in one place.
//!
//! Colour never carries a state alone: every status is a tone (colour), an
//! icon and a word, so it reads the same for colour-blind owners and in
//! high-contrast modes. The CSS classes (`chip ok|warn|danger|muted`) and the
//! icon names live here so pages do not each invent their own.

/// How a status chip looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Danger,
    Muted,
}

impl Tone {
    /// The chip CSS class (`app.css` defines the colours as tokens).
    pub fn chip_class(self) -> &'static str {
        match self {
            Tone::Ok => "chip status-chip ok",
            Tone::Warn => "chip status-chip warn",
            Tone::Danger => "chip status-chip danger",
            Tone::Muted => "chip status-chip muted",
        }
    }

    /// The icon that backs the colour up. `None` for a neutral chip.
    pub fn icon(self) -> Option<&'static str> {
        match self {
            Tone::Ok => Some("check"),
            Tone::Warn => Some("alert"),
            Tone::Danger => Some("alert-circle"),
            Tone::Muted => None,
        }
    }
}

/// A three-step limit status: fine, close to the limit, or at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Near,
    AtLimit,
}

impl Level {
    /// From the server's own flags (the daily task quota ships both).
    pub fn from_flags(near_limit: bool, at_limit: bool) -> Self {
        if at_limit {
            Level::AtLimit
        } else if near_limit {
            Level::Near
        } else {
            Level::Ok
        }
    }

    /// From a fill percentage: `near_at` and above is near, 100 is at limit.
    pub fn from_percent(percent: u32, near_at: u32) -> Self {
        if percent >= 100 {
            Level::AtLimit
        } else if percent >= near_at {
            Level::Near
        } else {
            Level::Ok
        }
    }

    /// From a count of problems: none is fine, any is worth a look.
    pub fn from_problem_count(count: u64) -> Self {
        if count == 0 { Level::Ok } else { Level::Near }
    }

    /// `Some(self)` only when the level needs attention; used where an "OK"
    /// chip on every meter would be noise.
    pub fn flagged(self) -> Option<Self> {
        (self != Level::Ok).then_some(self)
    }

    pub fn tone(self) -> Tone {
        match self {
            Level::Ok => Tone::Ok,
            Level::Near => Tone::Warn,
            Level::AtLimit => Tone::Danger,
        }
    }

    /// The word shown beside the icon.
    pub fn label(self) -> &'static str {
        match self {
            Level::Ok => "OK",
            Level::Near => "Near limit",
            Level::AtLimit => "At limit",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_win_in_order_at_limit_then_near() {
        assert_eq!(Level::from_flags(false, false), Level::Ok);
        assert_eq!(Level::from_flags(true, false), Level::Near);
        assert_eq!(Level::from_flags(true, true), Level::AtLimit);
        assert_eq!(Level::from_flags(false, true), Level::AtLimit);
    }

    #[test]
    fn percent_thresholds() {
        assert_eq!(Level::from_percent(79, 80), Level::Ok);
        assert_eq!(Level::from_percent(80, 80), Level::Near);
        assert_eq!(Level::from_percent(99, 80), Level::Near);
        assert_eq!(Level::from_percent(100, 80), Level::AtLimit);
        assert_eq!(Level::from_percent(89, 90), Level::Ok);
        assert_eq!(Level::from_percent(90, 90), Level::Near);
    }

    #[test]
    fn problem_count_flags_any_problem() {
        assert_eq!(Level::from_problem_count(0), Level::Ok);
        assert_eq!(Level::from_problem_count(3), Level::Near);
    }

    #[test]
    fn flagged_hides_ok() {
        assert_eq!(Level::Ok.flagged(), None);
        assert_eq!(Level::Near.flagged(), Some(Level::Near));
        assert_eq!(Level::AtLimit.flagged(), Some(Level::AtLimit));
    }

    #[test]
    fn every_status_has_colour_icon_and_word() {
        for level in [Level::Ok, Level::Near, Level::AtLimit] {
            let tone = level.tone();
            assert!(tone.icon().is_some(), "{level:?} needs an icon");
            assert!(!level.label().is_empty());
            assert!(tone.chip_class().starts_with("chip status-chip "));
        }
        let classes = [Tone::Ok, Tone::Warn, Tone::Danger]
            .map(|tone| tone.chip_class())
            .to_vec();
        let mut unique = classes.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), classes.len());
        let icons = [Tone::Ok, Tone::Warn, Tone::Danger].map(|tone| tone.icon());
        assert_ne!(icons[0], icons[1]);
        assert_ne!(icons[1], icons[2]);
    }
}
