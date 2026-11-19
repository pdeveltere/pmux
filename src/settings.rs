//! User settings, in `~/.config/pmux/settings.toml`. pmux → Settings… (⌘,) opens the file in an editor
//! tab; saving it applies the changes.
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Keyboard shortcut hints on the right of the status bar.
    pub shortcut_hints: bool,
}

/// Written when the file doesn't exist yet: every setting, commented out at its default.
const TEMPLATE: &str = "# pmux settings. Save to apply.

# Show keyboard shortcut hints in the status bar.
# shortcut_hints = false
";

pub fn path() -> PathBuf {
    crate::config::home().join(".config/pmux/settings.toml")
}

/// The saved settings (defaults when there is no file), or why the file can't be used.
pub fn load() -> Result<Settings, String> {
    match fs::read_to_string(path()) {
        Ok(s) => toml::from_str(&s).map_err(|e| format!("settings.toml: {}", e.message())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(format!("settings.toml: {e}")),
    }
}

/// The settings file, created from the template first if missing.
pub fn ensure() -> PathBuf {
    let p = path();
    if !p.exists() {
        let _ = fs::create_dir_all(p.parent().unwrap());
        let _ = fs::write(&p, TEMPLATE);
    }
    p
}
