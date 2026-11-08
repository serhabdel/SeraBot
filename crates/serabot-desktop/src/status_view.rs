//! Plain-word formatting for the Usage page's health cards (0.21).
//!
//! Pure functions, so the numbers and wording are tested without a window.

use crate::ServerHealth;

/// `1536` -> `1.5 KB`. One decimal below 10, none above, so the card stays
/// short and calm.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// `used` of `total` as a whole percent, 0..=100 (0 when `total` is 0).
pub fn percent(used: u64, total: u64) -> u32 {
    if total == 0 {
        return 0;
    }
    ((used as u128 * 100 / total as u128) as u32).min(100)
}

/// A bar width in percent: `bytes` against the largest value, with a small
/// floor so a non-empty row stays visible.
pub fn bar_percent(bytes: u64, max: u64) -> u32 {
    if bytes == 0 || max == 0 {
        return 0;
    }
    percent(bytes, max).max(3)
}

/// `90` -> `1 min`, `7300` -> `2 h 1 min`, `200000` -> `2 days 7 h`.
pub fn format_uptime(secs: u64) -> String {
    let minutes = secs / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    if days > 0 {
        let unit = if days == 1 { "day" } else { "days" };
        format!("{days} {unit} {} h", hours % 24)
    } else if hours > 0 {
        format!("{hours} h {} min", minutes % 60)
    } else {
        format!("{} min", minutes.max(1))
    }
}

/// `12.34` -> `12%`.
pub fn format_percent(value: f32) -> String {
    format!("{:.0}%", value.clamp(0.0, 100.0))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Ok,
    Watch,
}

/// One plain sentence about the server and whether it needs a look.
pub fn health(status: &ServerHealth) -> (Health, &'static str) {
    if let Some(disk) = &status.disk
        && percent(disk.used_bytes, disk.total_bytes) >= 90
    {
        return (Health::Watch, "Low on disk space");
    }
    if percent(
        status.host.memory_used_bytes,
        status.host.memory_total_bytes,
    ) >= 90
    {
        return (Health::Watch, "Memory is tight");
    }
    (Health::Ok, "OK")
}

/// A member's name for the table: display name, else email.
pub fn member_label(display_name: &str, email: &str) -> String {
    let name = display_name.trim();
    if name.is_empty() {
        email.trim().to_owned()
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HealthDisk, HealthHost};

    #[test]
    fn bytes_read_in_plain_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(900), "900 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(15 * 1024 * 1024), "15 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert!(format_bytes(u64::MAX).ends_with("TB"));
    }

    #[test]
    fn percent_and_bars_are_clamped() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(50, 100), 50);
        assert_eq!(percent(500, 100), 100);
        assert_eq!(bar_percent(0, 100), 0);
        assert_eq!(bar_percent(1, 1000), 3);
        assert_eq!(bar_percent(100, 100), 100);
        assert_eq!(bar_percent(5, 0), 0);
    }

    #[test]
    fn uptime_is_short_and_plain() {
        assert_eq!(format_uptime(5), "1 min");
        assert_eq!(format_uptime(7300), "2 h 1 min");
        assert_eq!(format_uptime(86400 + 3600 * 5), "1 day 5 h");
        assert_eq!(format_uptime(200_000), "2 days 7 h");
        assert_eq!(format_percent(12.34), "12%");
        assert_eq!(format_percent(150.0), "100%");
    }

    #[test]
    fn health_flags_a_full_disk_or_tight_memory() {
        let mut status = ServerHealth::default();
        assert_eq!(health(&status), (Health::Ok, "OK"));
        status.disk = Some(HealthDisk {
            total_bytes: 100,
            used_bytes: 95,
            free_bytes: 5,
        });
        assert_eq!(health(&status), (Health::Watch, "Low on disk space"));
        status.disk = None;
        status.host = HealthHost {
            memory_total_bytes: 100,
            memory_used_bytes: 92,
            ..HealthHost::default()
        };
        assert_eq!(health(&status), (Health::Watch, "Memory is tight"));
    }

    #[test]
    fn member_label_falls_back_to_email() {
        assert_eq!(member_label("Ana", "a@x.example"), "Ana");
        assert_eq!(member_label("  ", "a@x.example"), "a@x.example");
    }
}
