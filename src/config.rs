//! The sidebar's workspaces and projects, persisted as TOML in `~/.config/pmux/workspaces.toml`.
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The whole config file.
#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub workspace: Vec<WsCfg>,
}

/// One workspace: a name and its project directories (`~`-relative where possible).
#[derive(Serialize, Deserialize)]
pub struct WsCfg {
    pub name: String,
    #[serde(default)]
    pub projects: Vec<String>,
}

/// `$HOME`, or `/` when unset.
pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn path() -> PathBuf {
    home().join(".config/pmux/workspaces.toml")
}

/// The saved config: `Ok(None)` when there is no file yet, `Err` when it can't be read or parsed
/// (the caller must not overwrite it then: that would drop the user's workspaces).
pub fn load() -> Result<Option<Config>, String> {
    let p = path();
    let s = match fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    toml::from_str(&s).map(Some).map_err(|e| format!("{}: {e}", p.display()))
}

/// Write the config atomically (temp file + rename), so a crash never leaves it half written.
pub fn save(c: &Config) {
    let p = path();
    let _ = fs::create_dir_all(p.parent().unwrap());
    if let Ok(s) = toml::to_string_pretty(c) {
        let tmp = p.with_extension("toml.tmp");
        if fs::write(&tmp, s).is_ok() {
            let _ = fs::rename(&tmp, &p);
        }
    }
}

/// `~` / `~/x` → absolute path under `$HOME`.
pub fn expand(s: &str) -> PathBuf {
    if s == "~" {
        home()
    } else if let Some(r) = s.strip_prefix("~/") {
        home().join(r)
    } else {
        PathBuf::from(s)
    }
}

/// Inverse of `expand`: an absolute path under `$HOME` as `~/x`.
pub fn tilde(p: &Path) -> String {
    match p.strip_prefix(home()) {
        Ok(r) => format!("~/{}", r.display()),
        Err(_) => p.display().to_string(),
    }
}
