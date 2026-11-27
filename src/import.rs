//! `pmux import herdr`: open Herdr's live terminals in pmux, one tab per Herdr tab.
//!
//! Runs in the CLI process (it shells out to `hc`), then hands the window a list of terminals to
//! open. Claude Code sessions come back with `claude --resume <id>`: the id is in
//! `~/.claude/sessions/<pid>.json` for the pid Herdr reports as the terminal's foreground.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

/// One terminal to open in the window.
#[derive(Serialize, Deserialize)]
pub struct Item {
    pub cwd: PathBuf,
    /// Terminals with the same group share a tab (Herdr's tab id).
    pub group: String,
    /// Command to type into the new shell (`None`: just a shell).
    pub cmd: Option<String>,
    pub title: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Plan {
    pub items: Vec<Item>,
    /// Press Enter on the commands (else they wait, typed, at the prompt).
    pub run: bool,
}

fn hc(args: &[&str]) -> Result<Value, String> {
    let out = Command::new("hc").args(args).output().map_err(|e| format!("hc: {e} (is Herdr installed?)"))?;
    if !out.status.success() {
        return Err(format!("hc {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("hc {}: {e}", args.join(" ")))
}

/// Quote an argument for the shell if it needs it.
fn quote(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', r"'\''"))
    }
}

fn base(a: &str) -> &str {
    a.rsplit('/').next().unwrap_or(a).trim_start_matches('-')
}

pub fn session_id(pid: i64) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let s = std::fs::read_to_string(format!("{home}/.claude/sessions/{pid}.json")).ok()?;
    let v: Value = serde_json::from_str(&s).ok()?;
    v["sessionId"].as_str().map(String::from)
}

/// `claude <its flags> --resume <id>`, dropping whatever resume/continue flags it started with.
pub fn claude_cmd(argv: &[String], sid: Option<String>) -> String {
    // Flags that take no value; any other flag's next word is its value.
    const BOOL: &[&str] = &[
        "--dangerously-skip-permissions",
        "--verbose",
        "--debug",
        "-d",
        "--ide",
        "--chrome",
        "--no-chrome",
        "-c",
        "--continue",
        "--fork-session",
    ];
    let mut out = vec!["claude".to_string()];
    let (mut skip, mut value) = (false, false);
    for a in argv.iter().skip(1) {
        if std::mem::take(&mut skip) {
            continue;
        }
        if std::mem::take(&mut value) && !a.starts_with('-') {
            out.push(quote(a));
            continue;
        }
        match a.as_str() {
            "-r" | "--resume" | "--session-id" => skip = true,
            "-c" | "--continue" | "--fork-session" => {}
            a if a.starts_with("--resume=") || a.starts_with("--session-id=") => {}
            // A first message: the session already has it.
            a if !a.starts_with('-') => {}
            a => {
                value = !BOOL.contains(&a) && !a.contains('=');
                out.push(quote(a));
            }
        }
    }
    match sid {
        Some(id) => out.extend(["--resume".into(), id]),
        None => out.push("--continue".into()),
    }
    out.join(" ")
}

fn item(t: &Value) -> Option<(String, u32, Item)> {
    let fg = &t["foreground"];
    let cwd = fg["cwd"].as_str().or(t["cwd"].as_str())?;
    let argv: Vec<String> = fg["argv"]
        .as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let prog = argv.first().map(|a| base(a)).unwrap_or("");
    let claude = t["status"]["app"].as_str() == Some("claude-code") || prog == "claude";
    let cmd = if claude {
        Some(claude_cmd(&argv, fg["pid"].as_i64().and_then(session_id)))
    } else if argv.is_empty() || ["zsh", "bash", "fish", "sh", "nu"].contains(&prog) {
        None
    } else {
        Some(argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "))
    };
    let labels = &t["labels"];
    let space = labels["herdr.space"].as_str().unwrap_or("").to_string();
    let tab = labels["herdr.tab"].as_str().unwrap_or(t["address"].as_str()?).to_string();
    // "space name / 3": order tabs within a space by that number.
    let n = labels["where"].as_str().and_then(|w| w.rsplit(" / ").next()?.parse().ok()).unwrap_or(0);
    let title = if claude { t["title"].as_str().map(String::from) } else { None };
    Some((space, n, Item { cwd: PathBuf::from(cwd), group: tab, cmd, title }))
}

/// Gather Herdr's live terminals into a plan for the window.
pub fn plan(run: bool) -> Result<Plan, String> {
    let ls = hc(&["ls"])?;
    // `hc get` each, in parallel: one process per terminal.
    let handles: Vec<_> = ls["terminals"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["address"].as_str().map(String::from))
        .map(|a| std::thread::spawn(move || hc(&["get", &a])))
        .collect();
    let mut got = Vec::new();
    for h in handles {
        if let Ok(Ok(t)) = h.join() {
            if t["state"].as_str() == Some("live") {
                got.extend(item(&t));
            }
        }
    }
    // Spaces in the order they first appear, tabs by number, terminals of a tab together.
    let mut spaces: Vec<String> = Vec::new();
    for (s, _, _) in &got {
        if !spaces.contains(s) {
            spaces.push(s.clone());
        }
    }
    got.sort_by_key(|(s, n, it)| (spaces.iter().position(|x| x == s), *n, it.group.clone()));
    Ok(Plan { items: got.into_iter().map(|(_, _, it)| it).collect(), run })
}

pub fn main(args: &[String]) -> i32 {
    let run = args.iter().any(|a| a == "--run");
    if args.first().map(String::as_str) != Some("herdr") {
        eprintln!("usage: pmux import herdr [--run] [--dry-run]");
        return 1;
    }
    match plan(run) {
        Ok(p) if p.items.is_empty() => {
            eprintln!("pmux: Herdr has no live terminals");
            1
        }
        Ok(p) if args.iter().any(|a| a == "--dry-run") => {
            for it in &p.items {
                println!(
                    "{:<34} {:<50} {}",
                    &it.group[..it.group.len().min(34)],
                    it.cwd.display(),
                    it.cmd.as_deref().unwrap_or("(shell)")
                );
            }
            0
        }
        Ok(p) => crate::api::client(vec!["import".into(), serde_json::to_string(&p).unwrap()]),
        Err(e) => {
            eprintln!("pmux: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::claude_cmd;

    #[test]
    fn claude_resume_keeps_flags() {
        let argv: Vec<String> =
            ["claude", "--dangerously-skip-permissions", "-c", "--model", "opus", "fix it"].map(String::from).to_vec();
        assert_eq!(
            claude_cmd(&argv, Some("abc".into())),
            "claude --dangerously-skip-permissions --model opus --resume abc"
        );
        assert_eq!(claude_cmd(&argv[..1], None), "claude --continue");
    }
}
