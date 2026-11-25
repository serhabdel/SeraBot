//! Search and ordering for the model picker (std only).
//!
//! * [`fuzzy_score`]: forgiving subsequence matching ("son5" finds "claude-sonnet-5").
//! * Favorites and recent choices: small capped lists of `provider|model` values.

/// Models a person can star.
pub const MAX_FAVORITES: usize = 20;
/// Models remembered as recently chosen.
pub const MAX_RECENT: usize = 6;

/// How well `needle` matches `haystack`, or `None` when it does not match at all.
///
/// Every character of `needle` must appear in `haystack` in order (case-insensitive,
/// spaces in the needle are ignored). Higher is better: a match at the start of the
/// text or of a word, and runs of adjacent characters, score more; gaps cost a little.
pub fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    let needle: Vec<char> = needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.chars().flat_map(char::to_lowercase).collect();
    if needle.len() > hay.len() {
        return None;
    }
    // A plain substring is always the best kind of match.
    if let Some(at) = find_sub(&hay, &needle) {
        let boundary = at == 0 || !hay[at - 1].is_alphanumeric();
        return Some(1000 + if boundary { 200 } else { 0 } - at as i32);
    }
    let mut score = 0i32;
    let mut pos = 0usize;
    let mut prev: Option<usize> = None;
    for &want in &needle {
        let found = hay[pos..].iter().position(|c| *c == want)? + pos;
        score += 10;
        if found == 0 || !hay[found - 1].is_alphanumeric() {
            score += 15;
        }
        match prev {
            Some(p) if found == p + 1 => score += 12,
            Some(p) => score -= (found - p - 1).min(8) as i32,
            None => score -= found.min(10) as i32,
        }
        prev = Some(found);
        pos = found + 1;
    }
    Some(score)
}

fn find_sub(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// The best score of `needle` over several fields, each with a weight (title counts
/// more than the provider name).
pub fn best_score(needle: &str, fields: &[(&str, i32)]) -> Option<i32> {
    fields
        .iter()
        .filter_map(|(text, weight)| fuzzy_score(needle, text).map(|s| s * weight))
        .max()
}

/// Star or unstar `value`. Returns `true` when it is now a favorite. A full list
/// refuses new stars (`false`, unchanged).
pub fn toggle_favorite(list: &mut Vec<String>, value: &str) -> bool {
    if let Some(at) = list.iter().position(|v| v == value) {
        list.remove(at);
        return false;
    }
    if list.len() >= MAX_FAVORITES || value.trim().is_empty() {
        return false;
    }
    list.push(value.to_owned());
    true
}

/// Note a choice: most recent first, no duplicates, at most [`MAX_RECENT`].
pub fn touch_recent(list: &mut Vec<String>, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    list.retain(|v| v != value);
    list.insert(0, value.to_owned());
    list.truncate(MAX_RECENT);
}

/// Drop blanks and duplicates and apply the caps (used when loading saved prefs).
pub fn sanitize_values(list: &mut Vec<String>, cap: usize) {
    let mut seen: Vec<String> = Vec::new();
    list.retain(|v| {
        let keep = !v.trim().is_empty() && v.len() <= 200 && !seen.contains(v);
        if keep {
            seen.push(v.clone());
        }
        keep
    });
    list.truncate(cap);
}

/// Where a model sits when the search box is empty: favorites first (in the order
/// they were starred), then recents, then everything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standing {
    Favorite(usize),
    Recent(usize),
    Other,
}

pub fn standing(value: &str, favorites: &[String], recent: &[String]) -> Standing {
    if let Some(i) = favorites.iter().position(|v| v == value) {
        Standing::Favorite(i)
    } else if let Some(i) = recent.iter().position(|v| v == value) {
        Standing::Recent(i)
    } else {
        Standing::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_beats_scattered_letters() {
        let exact = fuzzy_score("sonnet", "claude-sonnet-5").unwrap();
        let scattered = fuzzy_score("sonnet", "s-o-n-n-e-t-x").unwrap();
        assert!(exact > scattered, "{exact} vs {scattered}");
        assert!(fuzzy_score("sonnet", "Claude Sonnet 5").is_some());
    }

    #[test]
    fn subsequences_match_in_order_only() {
        assert!(fuzzy_score("son5", "claude-sonnet-5").is_some());
        assert!(fuzzy_score("c5s", "claude-sonnet-5").is_none());
        assert!(fuzzy_score("gpt4o", "gpt-4o-mini").is_some());
        assert!(fuzzy_score("xyz", "gpt-4o").is_none());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_score("  ", "anything"), Some(0));
        assert!(fuzzy_score("a long needle", "short").is_none());
    }

    #[test]
    fn word_starts_and_runs_rank_higher() {
        // Neither is a plain substring, so word starts decide.
        let start = fuzzy_score("gf", "gemini-flash").unwrap();
        let middle = fuzzy_score("gf", "tragedy-fx").unwrap();
        assert!(start > middle, "{start} vs {middle}");
        let prefix = fuzzy_score("cla", "claude").unwrap();
        let inside = fuzzy_score("cla", "oracle-claude").unwrap();
        assert!(prefix > inside);
    }

    #[test]
    fn fields_are_weighted() {
        let title = best_score("opus", &[("Claude Opus", 3), ("anthropic", 1)]).unwrap();
        let provider_only = best_score("opus", &[("Claude", 3), ("opus-hosting", 1)]).unwrap();
        assert!(title > provider_only);
        assert_eq!(best_score("zzz", &[("Claude", 3)]), None);
    }

    #[test]
    fn favorites_toggle_and_cap() {
        let mut favs = Vec::new();
        assert!(toggle_favorite(&mut favs, "a|m1"));
        assert!(toggle_favorite(&mut favs, "a|m2"));
        assert!(!toggle_favorite(&mut favs, "a|m1"));
        assert_eq!(favs, vec!["a|m2".to_owned()]);
        assert!(!toggle_favorite(&mut favs, "  "));
        for i in 0..MAX_FAVORITES {
            toggle_favorite(&mut favs, &format!("p|m{i}"));
        }
        assert_eq!(favs.len(), MAX_FAVORITES);
        assert!(!toggle_favorite(&mut favs, "p|one-too-many"));
        assert_eq!(favs.len(), MAX_FAVORITES);
    }

    #[test]
    fn recents_are_most_recent_first_and_capped() {
        let mut recent = Vec::new();
        for i in 0..10 {
            touch_recent(&mut recent, &format!("p|m{i}"));
        }
        assert_eq!(recent.len(), MAX_RECENT);
        assert_eq!(recent[0], "p|m9");
        touch_recent(&mut recent, "p|m7");
        assert_eq!(recent[0], "p|m7");
        assert_eq!(recent.iter().filter(|v| *v == "p|m7").count(), 1);
        touch_recent(&mut recent, "");
        assert_eq!(recent.len(), MAX_RECENT);
    }

    #[test]
    fn saved_lists_are_cleaned() {
        let mut list = vec![
            "a|1".to_owned(),
            String::new(),
            "a|1".to_owned(),
            "b|2".to_owned(),
            "x".repeat(300),
        ];
        sanitize_values(&mut list, 5);
        assert_eq!(list, vec!["a|1".to_owned(), "b|2".to_owned()]);
        let mut long: Vec<String> = (0..30).map(|i| format!("p|{i}")).collect();
        sanitize_values(&mut long, MAX_FAVORITES);
        assert_eq!(long.len(), MAX_FAVORITES);
    }

    #[test]
    fn standing_orders_favorites_then_recents() {
        let favs = vec!["a|1".to_owned(), "b|2".to_owned()];
        let recent = vec!["c|3".to_owned(), "a|1".to_owned()];
        assert_eq!(standing("b|2", &favs, &recent), Standing::Favorite(1));
        assert_eq!(standing("a|1", &favs, &recent), Standing::Favorite(0));
        assert_eq!(standing("c|3", &favs, &recent), Standing::Recent(0));
        assert_eq!(standing("d|4", &favs, &recent), Standing::Other);
        assert!(Standing::Favorite(5) < Standing::Recent(0));
        assert!(Standing::Recent(3) < Standing::Other);
    }
}
