//! Desktop entries: which unit ids are apps a person opens from a launcher,
//! and which ones the session starts by itself (autostart).
//!
//! The extension decides what goes under Apps from open windows. This index
//! gives the daemon a close guess for clients that cannot see windows (the
//! CLI), and names processes that left their launcher scope (see `apps`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Desktop {
    /// Desktop file id (file name without `.desktop`) -> shown in launchers.
    entries: HashMap<String, bool>,
    /// Lowercased ids, for lookups by process name.
    lower: HashMap<String, String>,
    autostart: HashSet<String>,
}

impl Desktop {
    /// `data_dirs` hold `applications/`, `autostart_dirs` are the autostart
    /// folders themselves. Both go from highest precedence to lowest: the
    /// first file with a given id wins, as in the XDG specs.
    pub fn scan(data_dirs: &[PathBuf], autostart_dirs: &[PathBuf]) -> Self {
        let mut d = Self::default();
        for dir in data_dirs {
            for (id, text) in desktop_files(&dir.join("applications")) {
                if d.entries.contains_key(&id) {
                    continue;
                }
                let shown = !key_is_true(&text, "NoDisplay") && !key_is_true(&text, "Hidden");
                d.lower.entry(id.to_ascii_lowercase()).or_insert(id.clone());
                d.entries.insert(id, shown);
            }
        }
        let mut seen = HashSet::new();
        for dir in autostart_dirs {
            for (id, text) in desktop_files(dir) {
                if !seen.insert(id.clone()) {
                    continue;
                }
                let disabled = key_is_true(&text, "Hidden")
                    || key_value(&text, "X-GNOME-Autostart-enabled")
                        .is_some_and(|v| v.eq_ignore_ascii_case("false"));
                if !disabled {
                    d.autostart.insert(id);
                }
            }
        }
        d
    }

    /// The XDG data and config folders of the current session, plus the
    /// Flatpak and Snap export folders a session normally adds.
    pub fn from_env(home: &Path) -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let split = |v: String| -> Vec<PathBuf> {
            v.split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect()
        };
        let mut data = vec![
            var("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share")),
        ];
        data.extend(split(
            var("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
        ));
        data.extend([
            home.join(".local/share/flatpak/exports/share"),
            PathBuf::from("/var/lib/flatpak/exports/share"),
            PathBuf::from("/var/lib/snapd/desktop"),
        ]);
        let mut config = vec![
            var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config")),
        ];
        config.extend(split(
            var("XDG_CONFIG_DIRS").unwrap_or_else(|| "/etc/xdg".into()),
        ));
        dedup(&mut data);
        dedup(&mut config);
        let autostart: Vec<PathBuf> = config.iter().map(|c| c.join("autostart")).collect();
        Self::scan(&data, &autostart)
    }

    /// Desktop file id for a unit id: as is, lowercased, or a snap's
    /// `<snap>_<app>` entry.
    fn resolve(&self, id: &str) -> Option<&str> {
        if let Some((k, _)) = self.entries.get_key_value(id) {
            return Some(k);
        }
        if let Some(k) = self.lower.get(&id.to_ascii_lowercase()) {
            return Some(k);
        }
        let snap = format!("{id}_{id}");
        self.entries.get_key_value(&snap).map(|(k, _)| k.as_str())
    }

    /// An app a person opens from a launcher: it has a desktop entry shown in
    /// launchers, and the session does not start it by itself.
    pub fn launcher(&self, id: &str) -> bool {
        self.resolve(id)
            .is_some_and(|k| self.entries[k] && !self.autostart(k))
            && !self.autostart(id)
    }

    pub fn autostart(&self, id: &str) -> bool {
        self.autostart.contains(id)
    }

    /// Launcher entry named after a process: `sublime_text` ->
    /// `sublime_text`. `comm` is cut at 15 bytes, so a cut name matches the
    /// only entry it starts.
    pub fn for_process(&self, comm: &str) -> Option<String> {
        let shown = |k: &str| self.entries.get(k).copied().unwrap_or(false);
        let lower = comm.to_ascii_lowercase();
        if let Some(k) = self.lower.get(&lower).filter(|k| shown(k)) {
            return Some(k.clone());
        }
        if comm.len() < 15 {
            return None;
        }
        let mut found = self
            .lower
            .iter()
            .filter(|(l, k)| l.starts_with(&lower) && shown(k));
        match (found.next(), found.next()) {
            (Some((_, k)), None) => Some(k.clone()),
            _ => None,
        }
    }
}

fn dedup(dirs: &mut Vec<PathBuf>) {
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
}

fn desktop_files(dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_suffix(".desktop")?.to_string();
            let text = fs::read_to_string(e.path()).ok()?;
            Some((id, text))
        })
        .collect();
    out.sort();
    out
}

/// A key of the `[Desktop Entry]` group; other groups (actions) are ignored.
fn key_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim());
        }
    }
    None
}

fn key_is_true(text: &str, key: &str) -> bool {
    key_value(text, key).is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join(format!("{name}.desktop")),
            format!("[Desktop Entry]\nType=Application\nName={name}\n{body}"),
        )
        .unwrap();
    }

    fn fixture(name: &str) -> (PathBuf, Desktop) {
        let root =
            std::env::temp_dir().join(format!("hivetop-desktop-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let user = root.join("user");
        let system = root.join("system");
        let user_auto = root.join("config/autostart");
        let system_auto = root.join("xdg/autostart");
        write(&user.join("applications"), "todoist", "");
        write(&system.join("applications"), "brave-browser", "");
        write(&system.join("applications"), "sublime_text", "");
        write(&system.join("applications"), "solaar", "");
        write(&system.join("applications"), "org.flameshot.Flameshot", "");
        write(
            &system.join("applications"),
            "org.gnome.Evolution-alarm-notify",
            "NoDisplay=true",
        );
        write(
            &system.join("applications"),
            "com.example.LongNameViewer",
            "[Desktop Action new]\nNoDisplay=true",
        );
        write(&system.join("applications"), "firefox_firefox", "");
        write(&system_auto, "solaar", "");
        write(&system_auto, "update-notifier", "");
        write(&system_auto, "nm-applet", "");
        write(&user_auto, "Flameshot", "");
        write(&user_auto, "nm-applet", "Hidden=true");
        write(&user_auto, "Element", "X-GNOME-Autostart-enabled=false");
        let d = Desktop::scan(&[user, system], &[user_auto, system_auto]);
        (root, d)
    }

    #[test]
    fn launcher_apps_and_autostart() {
        let (root, d) = fixture("launcher");
        assert!(d.launcher("brave-browser"));
        assert!(d.launcher("todoist"), "user data dir");
        assert!(d.launcher("firefox"), "snap entry");
        assert!(d.launcher("org.flameshot.flameshot"), "case-insensitive");
        assert!(!d.launcher("solaar"), "autostart");
        assert!(!d.launcher("Flameshot"), "user autostart");
        assert!(!d.launcher("update-notifier"), "autostart without an entry");
        assert!(!d.launcher("org.gnome.Evolution-alarm-notify"), "NoDisplay");
        assert!(
            d.launcher("com.example.LongNameViewer"),
            "action group ignored"
        );
        assert!(!d.launcher("gnome-session-manager"), "no entry");
        assert!(!d.autostart("nm-applet"), "hidden by the user");
        assert!(!d.autostart("Element"), "disabled by the user");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn entry_for_a_process_name() {
        let (root, d) = fixture("process");
        assert_eq!(
            d.for_process("sublime_text").as_deref(),
            Some("sublime_text")
        );
        assert_eq!(
            d.for_process("com.example.Lon").as_deref(),
            Some("com.example.LongNameViewer"),
            "comm cut at 15 bytes"
        );
        assert_eq!(d.for_process("chrome_crashpad"), None);
        assert_eq!(
            d.for_process("org.gnome.Evolution-alarm-notify"),
            None,
            "hidden entry"
        );
        assert_eq!(d.for_process("sublime"), None, "short names match exactly");
        fs::remove_dir_all(root).unwrap();
    }
}
