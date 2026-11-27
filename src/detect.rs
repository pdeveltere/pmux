//! Agent state from the screen: per-agent rule files (`assets/agents/*.toml`) matched against
//! regions of the visible screen and the window title. The highest-priority matching rule wins.
//! The approach (manifests, priorities, regions) follows Herdr's detector; the rules are ours.
//!
//! A rule holds when every condition it lists holds:
//!   contains = [..]    all of these substrings (case-insensitive)
//!   regex = [..]       any of these matches the region text
//!   line_regex = [..]  any of these matches some line of the region
//!   any / all / not    nested conditions: at least one / every one / none
use regex::Regex;
use serde::Deserialize;
use std::sync::OnceLock;

/// What the screen says the agent is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Working,
    /// Waiting on a person: a permission prompt, a question, a form.
    Blocked,
    Idle,
}

impl Screen {
    pub fn as_str(self) -> &'static str {
        match self {
            Screen::Working => "working",
            Screen::Blocked => "blocked",
            Screen::Idle => "idle",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Screen::Working, Screen::Blocked, Screen::Idle].into_iter().find(|x| x.as_str() == s)
    }
}

const MANIFESTS: &[&str] = &[include_str!("../assets/agents/claude.toml"), include_str!("../assets/agents/codex.toml")];

#[derive(Deserialize)]
struct RawManifest {
    id: String,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
struct RawRule {
    id: String,
    state: String,
    priority: i32,
    region: String,
    #[serde(flatten)]
    cond: RawCond,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawCond {
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
    #[serde(default)]
    any: Vec<RawCond>,
    #[serde(default)]
    all: Vec<RawCond>,
    #[serde(default)]
    not: Vec<RawCond>,
}

struct Cond {
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
    any: Vec<Cond>,
    all: Vec<Cond>,
    not: Vec<Cond>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Region {
    OscTitle,
    /// The whole visible screen.
    Screen,
    /// The last n non-empty lines.
    Bottom(usize),
    /// The first n non-empty lines.
    Top(usize),
    /// Below the last horizontal rule (a line of ─).
    AfterLastRule,
    /// Between the last two horizontal rules (Claude's prompt box).
    PromptBox,
}

struct Rule {
    id: String,
    /// `None`: the screen shows something that says nothing about state (keep the last one).
    state: Option<Screen>,
    priority: i32,
    region: Region,
    cond: Cond,
}

pub struct Manifest {
    pub agent: String,
    rules: Vec<Rule>,
}

fn region(s: &str) -> Result<Region, String> {
    let n = |p: &str| s.strip_prefix(p).and_then(|r| r.strip_suffix(')')).and_then(|n| n.parse().ok());
    Ok(match s {
        "osc_title" => Region::OscTitle,
        "screen" => Region::Screen,
        "after_last_rule" => Region::AfterLastRule,
        "prompt_box" => Region::PromptBox,
        _ if s.starts_with("bottom(") => Region::Bottom(n("bottom(").ok_or_else(|| format!("bad region {s}"))?),
        _ if s.starts_with("top(") => Region::Top(n("top(").ok_or_else(|| format!("bad region {s}"))?),
        _ => return Err(format!("unknown region {s}")),
    })
}

fn compile(c: RawCond) -> Result<Cond, String> {
    let re = |v: Vec<String>| v.iter().map(|r| Regex::new(r).map_err(|e| e.to_string())).collect::<Result<Vec<_>, _>>();
    let sub = |v: Vec<RawCond>| v.into_iter().map(compile).collect::<Result<Vec<_>, _>>();
    Ok(Cond {
        contains: c.contains.into_iter().map(|s| s.to_lowercase()).collect(),
        regex: re(c.regex)?,
        line_regex: re(c.line_regex)?,
        any: sub(c.any)?,
        all: sub(c.all)?,
        not: sub(c.not)?,
    })
}

pub fn parse(src: &str) -> Result<Manifest, String> {
    let m: RawManifest = toml::from_str(src).map_err(|e| e.to_string())?;
    let mut rules = Vec::new();
    for r in m.rules {
        let state = match r.state.as_str() {
            "unknown" => None,
            s => Some(Screen::parse(s).ok_or_else(|| format!("{}: unknown state {s}", r.id))?),
        };
        let region = region(&r.region).map_err(|e| format!("{}: {e}", r.id))?;
        rules.push(Rule {
            state,
            priority: r.priority,
            region,
            cond: compile(r.cond).map_err(|e| format!("{}: {e}", r.id))?,
            id: r.id,
        });
    }
    rules.sort_by_key(|r| -r.priority);
    Ok(Manifest { agent: m.id, rules })
}

/// The built-in manifests, compiled once.
fn manifests() -> &'static [Manifest] {
    static M: OnceLock<Vec<Manifest>> = OnceLock::new();
    M.get_or_init(|| {
        MANIFESTS.iter().filter_map(|s| parse(s).map_err(|e| eprintln!("pmux: agent manifest: {e}")).ok()).collect()
    })
}

/// The manifest for an agent (as named by `pty::agent_of`), if there is one.
pub fn manifest(agent: &str) -> Option<&'static Manifest> {
    manifests().iter().find(|m| m.agent == agent)
}

fn is_rule(l: &str) -> bool {
    let t = l.trim();
    t.chars().count() >= 8 && t.chars().all(|c| c == '─' || c == '━')
}

impl Cond {
    fn holds(&self, text: &str, lower: &str) -> bool {
        self.contains.iter().all(|c| lower.contains(c.as_str()))
            && (self.regex.is_empty() || self.regex.iter().any(|r| r.is_match(text)))
            && (self.line_regex.is_empty() || text.lines().any(|l| self.line_regex.iter().any(|r| r.is_match(l))))
            && (self.any.is_empty() || self.any.iter().any(|c| c.holds(text, lower)))
            && self.all.iter().all(|c| c.holds(text, lower))
            && !self.not.iter().any(|c| c.holds(text, lower))
    }
}

impl Manifest {
    /// The screen's verdict: `None` when no rule matches, or the best match says nothing
    /// about state. Returns the rule's id too (for tests and debugging).
    pub fn detect(&self, lines: &[String], title: &str) -> Option<(Screen, &str)> {
        let rules: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| is_rule(l)).map(|(i, _)| i).collect();
        let non_empty = || lines.iter().filter(|l| !l.trim().is_empty());
        let text = |r: Region| -> String {
            match r {
                Region::OscTitle => title.to_string(),
                Region::Screen => lines.join("\n"),
                Region::Bottom(n) => {
                    let v: Vec<&String> = non_empty().collect();
                    v[v.len().saturating_sub(n)..].iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
                }
                Region::Top(n) => non_empty().take(n).map(std::string::String::as_str).collect::<Vec<_>>().join("\n"),
                Region::AfterLastRule => match rules.last() {
                    Some(&i) => lines[i + 1..].join("\n"),
                    None => String::new(),
                },
                Region::PromptBox => match rules.as_slice() {
                    [.., a, b] => lines[a + 1..*b].join("\n"),
                    _ => String::new(),
                },
            }
        };
        let mut cache: Vec<(Region, String, String)> = Vec::new();
        for r in &self.rules {
            let i = match cache.iter().position(|(reg, ..)| *reg == r.region) {
                Some(i) => i,
                None => {
                    let t = text(r.region);
                    let l = t.to_lowercase();
                    cache.push((r.region, t, l));
                    cache.len() - 1
                }
            };
            let (_, t, l) = &cache[i];
            if r.cond.holds(t, l) {
                return r.state.map(|s| (s, r.id.as_str()));
            }
        }
        None
    }
}

/// `pmux detect <agent> [title] < screen.txt`: which rule matches a screen.
pub fn cli(args: &[String]) -> i32 {
    use std::io::Read;
    let Some(m) = args.first().and_then(|a| manifest(a)) else {
        eprintln!(
            "usage: pmux detect <agent> [title] < screen.txt   (agents: {})",
            manifests().iter().map(|m| m.agent.as_str()).collect::<Vec<_>>().join(", ")
        );
        return 1;
    };
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    let lines: Vec<String> = s.lines().map(String::from).collect();
    match m.detect(&lines, args.get(1).map_or("", String::as_str)) {
        Some((st, rule)) => println!("{} ({rule})", st.as_str()),
        None => println!("no rule matched"),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn manifests_compile() {
        for src in MANIFESTS {
            parse(src).unwrap();
        }
        assert!(manifest("claude").is_some() && manifest("codex").is_some());
    }

    #[test]
    fn claude_states() {
        let m = manifest("claude").unwrap();
        let rule = "─".repeat(40);
        let idle = screen(&format!("⏺ Done.\n\n{rule}\n❯ \n{rule}\n  ? for shortcuts"));
        assert_eq!(m.detect(&idle, "✳ Fix bug").map(|x| x.0), Some(Screen::Idle));

        let working = screen(&format!(
            "✻ Pondering… (12s · ↓ 1.2k tokens · esc to interrupt)\n\n{rule}\n❯ \n{rule}\n  ⏵⏵ accept edits on"
        ));
        assert_eq!(m.detect(&working, "✳ Fix bug").map(|x| x.0), Some(Screen::Working));
        assert_eq!(m.detect(&idle, "⠂ Fix bug").map(|x| x.0), Some(Screen::Working));

        let perm = screen(&format!(
            "⏺ Bash(rm -rf target)\n{rule}\n Bash command\n\n   rm -rf target\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. No, and tell Claude what to do differently (esc)\n\n Esc to cancel · Tab to amend"
        ));
        assert_eq!(m.detect(&perm, "✳ Fix bug").map(|x| x.0), Some(Screen::Blocked));

        // A question about permissions in the transcript, with the empty prompt below it, is not a prompt.
        let talk = screen(&format!("⏺ Do you want to proceed? yes or no\n\n{rule}\n❯ \n{rule}\n  ? for shortcuts"));
        assert_eq!(m.detect(&talk, "✳ Fix bug").map(|x| x.0), Some(Screen::Idle));
    }

    #[test]
    fn codex_states() {
        let m = manifest("codex").unwrap();
        let working = screen("• Working (12s • esc to interrupt)\n\n› Ask Codex to do anything");
        assert_eq!(m.detect(&working, "").map(|x| x.0), Some(Screen::Working));
        let blocked = screen("Would you like to run the following command?\n\n  $ rm -rf target\n\n› 1. Yes, proceed (y)\n  2. No (esc)\n\nPress enter to confirm or esc to cancel");
        assert_eq!(m.detect(&blocked, "").map(|x| x.0), Some(Screen::Blocked));
        let idle = screen("• Done.\n\n› Ask Codex to do anything\n\n  100% context left");
        assert_eq!(m.detect(&idle, "").map(|x| x.0), Some(Screen::Idle));
    }
}
