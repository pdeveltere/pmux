//! The sidebar's workspaces (one folder each), persisted as TOML in `~/.config/pmux/workspaces.toml`.
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The whole config file.
#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    /// Workspace folders, in sidebar order (`~`-relative where possible).
    #[serde(default)]
    pub workspaces: Vec<String>,
    /// The old format: named groups of project folders. Read once and flattened into `workspaces`.
    #[serde(default, skip_serializing)]
    workspace: Vec<OldWs>,
}

#[derive(Deserialize)]
struct OldWs {
    #[serde(default)]
    projects: Vec<String>,
}

impl Config {
    pub fn new(workspaces: Vec<String>) -> Self {
        Config { workspaces, workspace: vec![] }
    }

    /// Flatten old-format groups: each project becomes a workspace.
    fn migrate(&mut self) {
        let old = std::mem::take(&mut self.workspace);
        for p in old.into_iter().flat_map(|w| w.projects) {
            if !self.workspaces.contains(&p) {
                self.workspaces.push(p);
            }
        }
    }
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
    let mut c: Config = toml::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?;
    if !c.workspace.is_empty() {
        // Keep the old file around once, then write the new format.
        let _ = fs::copy(&p, p.with_extension("toml.old"));
        c.migrate();
        save(&c);
    }
    Ok(Some(c))
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

#[cfg(test)]
mod tests {
    #[test]
    fn migrates_old_groups() {
        let mut c: super::Config = toml::from_str("[[workspace]]\nname = \"a\"\nprojects = [\"~/x\", \"~/y\"]\n[[workspace]]\nname = \"b\"\nprojects = [\"~/y\", \"/z\"]\n").unwrap();
        c.migrate();
        assert_eq!(c.workspaces, ["~/x", "~/y", "/z"]);
        assert_eq!(toml::to_string(&c).unwrap(), "workspaces = [\"~/x\", \"~/y\", \"/z\"]\n");
    }
}
