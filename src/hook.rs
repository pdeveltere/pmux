//! Agent hooks: the agent tells pmux which session runs in a terminal, so the background pmux can
//! resume it after a restart, and where it works, so its tab follows it into a worktree. Claude's
//! `SessionStart` hook reports the session id; `CwdChanged` (a `cd`) and `PostToolUse` of
//! `EnterWorktree`/`ExitWorktree` (which don't fire `CwdChanged`) report the folder. State comes
//! from the screen (`detect`), not hooks.
//!
//!   pmux integration install claude     add the hook to ~/.claude/settings.json
//!   pmux integration uninstall claude   remove it
//!   pmux hook claude                    what the hook runs (reads the hook's JSON on stdin)
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;

/// Our hook command. It does nothing outside pmux (`$PMUX_BIN` is set only in pmux terminals).
const CLAUDE_CMD: &str = r#"[ -n "$PMUX_BIN" ] && "$PMUX_BIN" hook claude; exit 0"#;
/// (event, matcher) pairs our hook runs on.
const EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", Some("startup|resume|clear|compact|fork")),
    ("CwdChanged", None),
    ("PostToolUse", Some("EnterWorktree|ExitWorktree")),
];

fn claude_settings() -> PathBuf {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude"));
    dir.join("settings.json")
}

fn is_ours(h: &Value) -> bool {
    h["command"].as_str().is_some_and(|c| c.contains("PMUX_BIN") && c.contains("hook claude"))
}

/// Drop our hook from every event's list (and entries it leaves empty). True if it was there.
fn remove(settings: &mut Value) -> bool {
    let Some(events) = settings.pointer_mut("/hooks").and_then(Value::as_object_mut) else { return false };
    let mut found = false;
    for list in events.values_mut().filter_map(Value::as_array_mut) {
        let before = list.len();
        let mut had = false;
        for entry in list.iter_mut() {
            if let Some(hs) = entry["hooks"].as_array_mut() {
                let n = hs.len();
                hs.retain(|h| !is_ours(h));
                had |= hs.len() != n;
            }
        }
        if had {
            list.retain(|e| e["hooks"].as_array().is_none_or(|h| !h.is_empty()));
        }
        found |= had || list.len() != before;
    }
    found
}

fn install(settings: &mut Value) {
    remove(settings);
    if !settings.is_object() {
        *settings = json!({});
    }
    let hooks = settings.as_object_mut().unwrap().entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return };
    for (event, matcher) in EVENTS {
        if let Value::Array(list) = hooks.entry(*event).or_insert_with(|| json!([])) {
            let mut e = json!({ "hooks": [{ "type": "command", "command": CLAUDE_CMD, "timeout": 5 }] });
            if let Some(m) = matcher {
                e["matcher"] = json!(m);
            }
            list.push(e);
        }
    }
}

/// `pmux integration install|uninstall claude`
pub fn integration(args: &[String]) -> i32 {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let installing = match a.as_slice() {
        ["install", "claude"] => true,
        ["uninstall", "claude"] => false,
        _ => {
            eprintln!("usage: pmux integration install|uninstall claude");
            return 1;
        }
    };
    let path = claude_settings();
    let mut settings: Value = match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("pmux: {} isn't valid JSON ({e}); not touching it", path.display());
                return 1;
            }
        },
        Err(_) => json!({}),
    };
    let msg = if installing {
        install(&mut settings);
        "installed: Claude reports its session and folder to pmux (new Claude sessions)"
    } else if remove(&mut settings) {
        "removed the pmux hook"
    } else {
        println!("no pmux hook in {}", path.display());
        return 0;
    };
    let mut out = serde_json::to_string_pretty(&settings).unwrap();
    out.push('\n');
    if let Err(e) = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, out)) {
        eprintln!("pmux: {}: {e}", path.display());
        return 1;
    }
    println!("{msg} ({})", path.display());
    0
}

/// `pmux hook claude`: run by Claude's hook with its JSON on stdin. Always exits 0 and prints
/// nothing, so it can never get in Claude's way.
pub fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) != Some("claude") {
        return 0;
    }
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Ok(v) = serde_json::from_str::<Value>(&input) else { return 0 };
    // Subagents run their own sessions inside the same terminal: only the main one counts.
    if v.get("agent_id").is_some_and(|a| !a.is_null()) {
        return 0;
    }
    let args = match v["hook_event_name"].as_str() {
        Some("SessionStart") => v["session_id"].as_str().map(|sid| ["agent-session", "claude", sid]),
        Some("CwdChanged") => v["new_cwd"].as_str().or(v["cwd"].as_str()).map(|d| ["agent-cwd", "claude", d]),
        // After the tool ran, `cwd` is the folder it switched to.
        Some("PostToolUse") => v["cwd"].as_str().map(|d| ["agent-cwd", "claude", d]),
        _ => None,
    };
    let (Some(args), Ok(pane)) = (args, std::env::var("PMUX_PANE")) else { return 0 };
    // Straight to the background pmux: it keeps these, window open or not.
    let Ok(mut s) = std::os::unix::net::UnixStream::connect(crate::api::hold_sock()) else { return 0 };
    let req =
        crate::api::Request { caller: Some(pane), args: args.iter().map(std::string::ToString::to_string).collect() };
    let mut msg = serde_json::to_string(&req).unwrap();
    msg.push('\n');
    let _ = s.write_all(msg.as_bytes());
    let _ = s.read_to_string(&mut String::new());
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_is_idempotent_and_keeps_other_hooks() {
        let mut s = json!({"model": "opus", "hooks": {"PostToolUse": [{"matcher": "X", "hooks": [{"type": "command", "command": "other"}]}]}});
        install(&mut s);
        install(&mut s);
        assert_eq!(s["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(s["hooks"]["CwdChanged"].as_array().unwrap().len(), 1);
        assert_eq!(s["hooks"]["PostToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(s["hooks"]["PostToolUse"][0]["hooks"][0]["command"], "other");
        assert!(remove(&mut s));
        assert_eq!(s["hooks"]["SessionStart"].as_array().unwrap().len(), 0);
        assert_eq!(s["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(s["model"], "opus");
        let keys: Vec<&String> = s.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks"]);
    }
}
