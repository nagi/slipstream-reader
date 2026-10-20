//! Where the reader keeps things, by the XDG rules: settings under `~/.config/slipstream`, what
//! it remembers under `~/.local/state/slipstream`, and books it has fetched under
//! `~/.cache/slipstream`. The same folders Slipstream itself uses, so on a Slipstream desktop the
//! reader's files sit with the rest.

use std::path::PathBuf;

/// `$NAME` when it is set to an absolute path, else `~/fallback`.
fn xdg_home(name: &str, fallback: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn config_dir() -> PathBuf {
    xdg_home("XDG_CONFIG_HOME", ".config").join("slipstream")
}

pub fn state_dir() -> PathBuf {
    xdg_home("XDG_STATE_HOME", ".local/state").join("slipstream")
}

pub fn cache_dir() -> PathBuf {
    xdg_home("XDG_CACHE_HOME", ".cache").join("slipstream")
}
