/// Collapses whitespace, shortens `$HOME` to `~` and cuts to `max` characters.
pub fn short_command(cmd: &str, home: &str, max: usize) -> String {
    let collapsed = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = if !home.is_empty() && home != "/" {
        collapsed.replace(home, "~")
    } else {
        collapsed
    };
    truncate(&text, max)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i >= 3 {
        format!("{v:.1} {}", UNITS[i])
    } else {
        format!("{v:.0} {}", UNITS[i])
    }
}

pub fn duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d{}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_and_units() {
        assert_eq!(
            short_command("node   /home/u/.claude/x.js", "/home/u", 80),
            "node ~/.claude/x.js"
        );
        assert_eq!(truncate("abcdefgh", 6), "abc...");
        assert_eq!(bytes(512 * 1024 * 1024), "512 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert_eq!(duration(3700), "1h01m");
    }
}
