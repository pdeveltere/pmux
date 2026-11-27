//! Syntax highlighting with tree-sitter: language by file name, highlight queries, and spans per
//! line for the editor to color. Configurations compile once per language, on first use.
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Capture names we color, most specific first where it matters. A capture like
/// `function.method.call` takes the longest of these that prefixes it.
pub const NAMES: &[&str] = &[
    "attribute",
    "boolean",
    "comment",
    "comment.doc",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "module",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.escape",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
    "text.title",
    "text.literal",
    "text.uri",
    "text.reference",
    "text.emphasis",
    "text.strong",
    "markup.heading",
    "markup.raw",
    "markup.link",
    "markup.italic",
    "markup.bold",
];

/// A colored run on one line: chars `start..end`, index into `NAMES`.
pub type Span = (usize, usize, u8);

/// Files bigger than this aren't highlighted (each edit re-highlights the whole buffer, in the background).
const MAX_BYTES: usize = 2 << 20;

/// The language for a file, by extension or name.
pub fn lang_for(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    if matches!(name, "Cargo.lock" | "uv.lock" | "poetry.lock") {
        return Some("toml");
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or(name);
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "sh" | "bash" | "zsh" | ".bashrc" | ".zshrc" | ".profile" | ".envrc" => "bash",
        "md" | "markdown" => "markdown",
        _ => return None,
    })
}

/// Names that injections (markdown code fences, HTML scripts, …) use for our languages.
fn canonical(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "rust" | "rs" => "rust",
        "toml" => "toml",
        "json" => "json",
        "javascript" | "js" | "jsx" => "javascript",
        "typescript" | "ts" => "typescript",
        "tsx" => "tsx",
        "bash" | "sh" | "shell" | "zsh" | "console" => "bash",
        "markdown" | "md" => "markdown",
        "markdown_inline" => "markdown_inline",
        _ => return None,
    })
}

fn build(name: &'static str) -> Option<HighlightConfiguration> {
    let ts_js = || format!("{}\n{}", tree_sitter_typescript::HIGHLIGHTS_QUERY, tree_sitter_javascript::HIGHLIGHT_QUERY);
    let (lang, hl, inj): (tree_sitter::Language, String, &str) = match name {
        "rust" => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY.into(),
            tree_sitter_rust::INJECTIONS_QUERY,
        ),
        "toml" => (tree_sitter_toml_ng::LANGUAGE.into(), tree_sitter_toml_ng::HIGHLIGHTS_QUERY.into(), ""),
        "json" => (tree_sitter_json::LANGUAGE.into(), tree_sitter_json::HIGHLIGHTS_QUERY.into(), ""),
        "javascript" => (
            tree_sitter_javascript::LANGUAGE.into(),
            format!("{}\n{}", tree_sitter_javascript::JSX_HIGHLIGHT_QUERY, tree_sitter_javascript::HIGHLIGHT_QUERY),
            tree_sitter_javascript::INJECTIONS_QUERY,
        ),
        "typescript" => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), ts_js(), ""),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            format!("{}\n{}", tree_sitter_javascript::JSX_HIGHLIGHT_QUERY, ts_js()),
            "",
        ),
        "bash" => (tree_sitter_bash::LANGUAGE.into(), tree_sitter_bash::HIGHLIGHT_QUERY.into(), ""),
        "markdown" => (
            tree_sitter_md::LANGUAGE.into(),
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.into(),
            tree_sitter_md::INJECTION_QUERY_BLOCK,
        ),
        "markdown_inline" => (
            tree_sitter_md::INLINE_LANGUAGE.into(),
            tree_sitter_md::HIGHLIGHT_QUERY_INLINE.into(),
            tree_sitter_md::INJECTION_QUERY_INLINE,
        ),
        _ => return None,
    };
    let mut c = HighlightConfiguration::new(lang, name, &hl, inj, "")
        .map_err(|e| eprintln!("pmux: {name} highlights: {e}"))
        .ok()?;
    c.configure(NAMES);
    Some(c)
}

/// The compiled configuration for a language (built once, kept for the process).
fn config(name: &str) -> Option<&'static HighlightConfiguration> {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Option<&'static HighlightConfiguration>>>> = OnceLock::new();
    let name = canonical(name)?;
    let mut m = CACHE.get_or_init(Default::default).lock().ok()?;
    *m.entry(name).or_insert_with(|| build(name).map(|c| &*Box::leak(Box::new(c))))
}

/// Run the highlighter; the explicit `'a` lets the 'static configs shorten to the source's lifetime.
fn run<'a>(
    hl: &'a mut Highlighter,
    cfg: &'a HighlightConfiguration,
    src: &'a str,
) -> Result<impl Iterator<Item = Result<HighlightEvent, tree_sitter_highlight::Error>> + 'a, tree_sitter_highlight::Error>
{
    hl.highlight(cfg, src.as_bytes(), None, None, |n| -> Option<&'a HighlightConfiguration> { config(n) })
}

/// Highlight `lines` (joined with '\n') as `lang`: one list of spans per line.
#[cfg(test)]
pub fn highlight(lang: &str, lines: &[String]) -> Vec<Vec<Span>> {
    highlight_src(lang, &lines.join("\n"))
}

/// Highlight `src` as `lang`: one list of spans per line. Takes ~0.3 ms per KB: run it off the
/// UI thread.
pub fn highlight_src(lang: &str, src: &str) -> Vec<Vec<Span>> {
    let mut out = vec![Vec::new(); src.split('\n').count()];
    let Some(cfg) = config(lang) else { return out };
    if src.len() > MAX_BYTES {
        return out;
    }
    let mut hl = Highlighter::new();
    let Ok(events) = run(&mut hl, cfg, src) else { return out };
    // Byte offset → (line, char column), walked forward as events arrive in order.
    let mut stack: Vec<u8> = Vec::new();
    let (mut line, mut col, mut at) = (0usize, 0usize, 0usize);
    for ev in events {
        let Ok(ev) = ev else { break };
        match ev {
            HighlightEvent::HighlightStart(h) => stack.push(h.0 as u8),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                debug_assert!(start >= at);
                // Skip to `start` (no color), then color `start..end`, splitting at newlines.
                for (i, c) in src[at..end].char_indices() {
                    let colored = at + i >= start;
                    if c == '\n' {
                        line += 1;
                        col = 0;
                        continue;
                    }
                    if let (true, Some(&k)) = (colored, stack.last()) {
                        match out[line].last_mut() {
                            Some(s) if s.1 == col && s.2 == k => s.1 += 1,
                            _ => out[line].push((col, col + 1, k)),
                        }
                    }
                    col += 1;
                }
                at = end;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_at(spans: &[Span], col: usize) -> Option<&'static str> {
        spans.iter().find(|s| s.0 <= col && col < s.1).map(|s| NAMES[s.2 as usize])
    }

    #[test]
    fn every_language_compiles() {
        for l in ["rust", "toml", "json", "javascript", "typescript", "tsx", "bash", "markdown", "markdown_inline"] {
            assert!(config(l).is_some(), "{l}");
        }
    }

    #[test]
    fn rust_spans() {
        let lines: Vec<String> =
            ["fn main() {", "    let s = \"hé\"; // note", "}"].iter().map(|s| s.to_string()).collect();
        let h = highlight("rust", &lines);
        assert_eq!(name_at(&h[0], 0), Some("keyword"));
        assert_eq!(name_at(&h[0], 3).map(|n| n.starts_with("function")), Some(true));
        assert_eq!(name_at(&h[1], 4), Some("keyword"));
        assert_eq!(name_at(&h[1], 13), Some("string"), "char columns, not bytes");
        assert_eq!(name_at(&h[1], 19), Some("comment"));
    }

    #[test]
    fn markdown_injects_code() {
        let lines: Vec<String> =
            ["# Title", "", "```rust", "let x = 1;", "```"].iter().map(|s| s.to_string()).collect();
        let h = highlight("markdown", &lines);
        assert!(name_at(&h[0], 2).is_some());
        assert_eq!(name_at(&h[3], 0), Some("keyword"));
    }

    #[test]
    fn detects_languages() {
        assert_eq!(lang_for(Path::new("a/b.rs")), Some("rust"));
        assert_eq!(lang_for(Path::new("Cargo.lock")), Some("toml"));
        assert_eq!(lang_for(Path::new(".zshrc")), Some("bash"));
        assert_eq!(lang_for(Path::new("x.unknown")), None);
    }
}
