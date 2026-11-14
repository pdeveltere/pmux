use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub workspace: Vec<WsCfg>,
}

#[derive(Serialize, Deserialize)]
pub struct WsCfg {
    pub name: String,
    #[serde(default)]
    pub projects: Vec<String>,
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn path() -> PathBuf {
    home().join(".config/pmux/workspaces.toml")
}

pub fn load() -> Option<Config> {
    toml::from_str(&fs::read_to_string(path()).ok()?).ok()
}

pub fn save(c: &Config) {
    let p = path();
    let _ = fs::create_dir_all(p.parent().unwrap());
    if let Ok(s) = toml::to_string_pretty(c) {
        let _ = fs::write(p, s);
    }
}

pub fn expand(s: &str) -> PathBuf {
    if s == "~" {
        home()
    } else if let Some(r) = s.strip_prefix("~/") {
        home().join(r)
    } else {
        PathBuf::from(s)
    }
}

pub fn tilde(p: &Path) -> String {
    match p.strip_prefix(home()) {
        Ok(r) => format!("~/{}", r.display()),
        Err(_) => p.display().to_string(),
    }
}
