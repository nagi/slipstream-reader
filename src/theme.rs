//! The desktop's colours, which the reader follows: the Omarchy theme Slipstream is drawn in,
//! when it follows one, and otherwise Slipstream's own dark or light. The page is the theme's
//! background and the text its foreground, and they change the moment the theme does.

use gtk4::gio::{self, prelude::*};
use slipstream_config::{ColourScheme, Settings};

use crate::palette::Rgb;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub page: Rgb,
    pub text: Rgb,
    /// The ring's colour on the desktop: the chapter you're in.
    pub accent: Rgb,
}

/// Slipstream's own colours, as its Settings window is drawn without a theme.
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

/// The colours the desktop is drawn in now.
pub fn current() -> Theme {
    let settings = slipstream_config::read(&slipstream_config::path()).unwrap_or_default();
    of(&settings)
}

fn of(settings: &Settings) -> Theme {
    let theme = settings.appearance.theme();
    match settings.palette(theme.as_ref()) {
        Some(palette) => Theme {
            page: palette.ground,
            text: palette.ink,
            accent: palette.accent,
        },
        None => match settings.appearance.colour_scheme {
            ColourScheme::Dark => OWN_DARK,
            ColourScheme::Light => OWN_LIGHT,
        },
    }
}

/// Calls `changed` whenever the colours may have: Omarchy setting a theme, or the settings
/// turning following it on or off. The monitors stop when they're dropped.
pub fn watch(changed: impl Fn() + Clone + 'static) -> Vec<gio::FileMonitor> {
    let mut monitors = Vec::new();
    // `omarchy theme set` moves the new theme into place and then writes its name, so the
    // folder is watched rather than a file in it.
    let folder = gio::File::for_path(slipstream_config::theme::current_dir());
    if let Ok(monitor) = folder.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
    {
        let changed = changed.clone();
        monitor.connect_changed(move |_, _, _, _| changed());
        monitors.push(monitor);
    }
    let settings = gio::File::for_path(slipstream_config::path());
    if let Ok(monitor) = settings.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
        monitor.connect_changed(move |_, _, _, _| changed());
        monitors.push(monitor);
    }
    monitors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_theme_the_reader_is_in_slipstreams_own_dark() {
        let mut settings = Settings::default();
        settings.appearance.follow_omarchy_theme = false;
        settings.appearance.colour_scheme = ColourScheme::Dark;
        assert_eq!(of(&settings), OWN_DARK);
        settings.appearance.colour_scheme = ColourScheme::Light;
        assert_eq!(of(&settings), OWN_LIGHT);
    }
}
