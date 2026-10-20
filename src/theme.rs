//! The colours the reader is drawn in: the Omarchy theme when there is one, else its own dark or
//! light. The page is the theme's background and the text its foreground, and they change the
//! moment the theme does.
//!
//! On a Slipstream desktop its settings still have their say: with "follow the Omarchy theme"
//! turned off, the reader takes Slipstream's dark or light instead. Without Omarchy (another
//! desktop, or Windows through WSL) it is the reader's own dark.
//!
//! Omarchy keeps the theme it is showing in `~/.local/state/omarchy/current`, its colours in
//! `theme/colors.toml`. That file is read the way Omarchy's own `omarchy-theme-color` reads it,
//! line by line rather than as TOML, with the same older names standing in for ones a theme
//! leaves out.

use std::{collections::HashMap, path::PathBuf};

use gtk4::gio::{self, prelude::*};

use crate::{dirs, palette::Rgb};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub page: Rgb,
    pub text: Rgb,
    /// The chapter you're in, highlights, and buttons that are on.
    pub accent: Rgb,
}

/// The reader's own colours, Slipstream's, for when there's no theme to follow.
const OWN_DARK: Theme = Theme {
    page: [0x17, 0x19, 0x1f],
    text: [0xe6, 0xe8, 0xee],
    accent: [0x42, 0xd3, 0xff],
};
const OWN_LIGHT: Theme = Theme {
    page: [0xf7, 0xf1, 0xe3],
    text: [0x1e, 0x24, 0x30],
    accent: [0x1d, 0x7f, 0xb0],
};
/// A theme's accent when it names none and no blue either.
const OWN_CYAN: Rgb = [0x42, 0xd3, 0xff];

/// Where Omarchy keeps the theme it is showing.
fn omarchy_dir() -> PathBuf {
    dirs::home().join(".local/state/omarchy/current")
}

/// Slipstream's settings file, when there is a Slipstream.
fn settings_path() -> PathBuf {
    dirs::config_dir().join("settings.toml")
}

/// The colours to draw in now.
pub fn current() -> Theme {
    let settings = std::fs::read_to_string(settings_path()).unwrap_or_default();
    let (follow, light) = appearance(&settings);
    let omarchy = follow
        .then(|| std::fs::read_to_string(omarchy_dir().join("theme/colors.toml")).ok())
        .flatten()
        .and_then(|text| parse(&text));
    omarchy.unwrap_or(if light { OWN_LIGHT } else { OWN_DARK })
}

/// From Slipstream's settings: whether to follow the Omarchy theme (yes, unless it says not),
/// and whether its own scheme is light (no, unless it says so).
fn appearance(settings: &str) -> (bool, bool) {
    let mut section = "";
    let (mut follow, mut light) = (true, false);
    for line in settings.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line;
            continue;
        }
        if section != "[appearance]" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "follow-omarchy-theme" => follow = value != "false",
            "colour-scheme" => light = value == "light",
            _ => {}
        }
    }
    (follow, light)
}

/// A `colors.toml`'s page, text and accent. `None` when it names no background or foreground.
fn parse(text: &str) -> Option<Theme> {
    let raw = raw_values(text);
    let colour = |names: &[&str]| names.iter().find_map(|name| rgb(raw.get(*name)?));
    Some(Theme {
        page: colour(&["background", "bg", "color0"])?,
        text: colour(&["foreground", "fg", "color7"])?,
        accent: colour(&["accent", "blue", "color4"]).unwrap_or(OWN_CYAN),
    })
}

/// `key = "value"` lines, keys stripped of quotes and spaces, as Omarchy reads them.
fn raw_values(text: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key: String = key
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | ' ' | '\t'))
            .collect();
        let plain = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
        if key.is_empty() || !key.chars().all(plain) {
            continue;
        }
        let quote = |c: char| c == '"' || c == '\'';
        let value = match value.find(quote) {
            Some(open) => {
                let rest = &value[open + 1..];
                &rest[..rest.find(quote).unwrap_or(rest.len())]
            }
            None => value.trim(),
        };
        values.insert(key, value.to_string());
    }
    values
}

/// `#rrggbb` (or with an alpha after it), with or without the `#`.
fn rgb(text: &str) -> Option<Rgb> {
    let digits = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if !matches!(digits.len(), 6 | 8) || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// Calls `changed` whenever the colours may have: Omarchy setting a theme, or Slipstream's
/// settings changing. The monitors stop when they're dropped.
pub fn watch(changed: impl Fn() + Clone + 'static) -> Vec<gio::FileMonitor> {
    let mut monitors = Vec::new();
    // `omarchy theme set` moves the new theme into place and then writes its name, so the
    // folder is watched rather than a file in it.
    let folder = gio::File::for_path(omarchy_dir());
    if let Ok(monitor) =
        folder.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
    {
        let changed = changed.clone();
        monitor.connect_changed(move |_, _, _, _| changed());
        monitors.push(monitor);
    }
    let settings = gio::File::for_path(settings_path());
    if let Ok(monitor) = settings.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
    {
        monitor.connect_changed(move |_, _, _, _| changed());
        monitors.push(monitor);
    }
    monitors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_gives_page_text_and_accent() {
        let theme =
            parse("accent = \"#7aa2f7\"\nbackground = \"#1a1b26\"\nforeground = \"#a9b1d6\"\n")
                .unwrap();
        assert_eq!(theme.page, [0x1a, 0x1b, 0x26]);
        assert_eq!(theme.text, [0xa9, 0xb1, 0xd6]);
        assert_eq!(theme.accent, [0x7a, 0xa2, 0xf7]);
    }

    #[test]
    fn older_themes_names_stand_in() {
        let theme = parse("bg='#000000'\nfg = '#ffffff'\ncolor4 = \"#0000ff\"").unwrap();
        assert_eq!(theme.page, [0, 0, 0]);
        assert_eq!(theme.accent, [0, 0, 255]);
        assert!(parse("accent = \"#ffffff\"").is_none());
    }

    #[test]
    fn slipstreams_settings_have_their_say() {
        assert_eq!(appearance(""), (true, false));
        let settings = "[appearance]\ncolour-scheme = \"light\"\nfollow-omarchy-theme = false\n\
                        [wallpaper]\nfollow-omarchy-theme = true\n";
        assert_eq!(appearance(settings), (false, true));
    }
}
