//! Small helpers shared across modules.
use std::collections::HashSet;
use std::hash::Hash;
use std::path::Path;

/// Insert `key` if absent, remove it if present.
pub fn toggle<T: Eq + Hash>(set: &mut HashSet<T>, key: T) {
    if !set.remove(&key) {
        set.insert(key);
    }
}

/// A path's last component ("/" for the root).
pub fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(|| "/".into(), |n| n.to_string_lossy().into_owned())
}

/// Quote `s` for a POSIX shell, leaving plain words as they are.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./+@%:".contains(&b)) {
        return s.into();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(shell_quote("src/main.rs"), "src/main.rs");
        assert_eq!(shell_quote("it's here"), "'it'\\''s here'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(file_name(Path::new("/")), "/");
        let mut s = HashSet::new();
        toggle(&mut s, 1);
        assert!(s.contains(&1));
        toggle(&mut s, 1);
        assert!(s.is_empty());
    }
}
