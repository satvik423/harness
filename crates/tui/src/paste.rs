//! Pasted text that names files becomes attachments (P4-T5).
//!
//! Terminals deliver a dragged file as pasted text: a plain path, a quoted
//! path, a path with backslash-escaped spaces, or a `file://` URL. If every
//! item in the paste names an existing file, the paste is a list of
//! attachments; otherwise it is ordinary text.

use std::path::{Path, PathBuf};

/// Splits pasted text into shell-like words (quotes and `\ ` escapes kept together).
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            (None, c) if c.is_whitespace() => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            (None, c) => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// `file:///a%20b` → `/a b`.
fn from_file_url(word: &str) -> Option<String> {
    let rest = word.strip_prefix("file://")?;
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = path.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// The files named by `pasted`, if it names only existing files (resolved
/// against `cwd`); `None` if it is ordinary text.
///
/// # Examples
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("a b.png"), b"x").unwrap();
/// let got = harness_tui::paste::attachments("'a b.png'", dir.path()).unwrap();
/// assert_eq!(got, [dir.path().join("a b.png")]);
/// assert!(harness_tui::paste::attachments("just some words", dir.path()).is_none());
/// ```
#[must_use]
pub fn attachments(pasted: &str, cwd: &Path) -> Option<Vec<PathBuf>> {
    let words = words(pasted);
    if words.is_empty() {
        return None;
    }
    let mut paths = Vec::with_capacity(words.len());
    for word in words {
        let word = from_file_url(&word).unwrap_or(word);
        let path = Path::new(&word);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        if !path.is_file() {
            return None;
        }
        paths.push(path);
    }
    Some(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_files_in_every_terminal_style_are_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("my file.txt"), "x").unwrap();
        std::fs::write(dir.path().join("b.rs"), "x").unwrap();
        let abs = dir.path().join("my file.txt");
        let quoted = format!("'{}'", abs.display());
        let escaped = abs.display().to_string().replace(' ', "\\ ");
        let url = format!("file://{}", abs.display().to_string().replace(' ', "%20"));
        for pasted in [quoted, escaped, url] {
            assert_eq!(
                attachments(&pasted, dir.path()),
                Some(vec![abs.clone()]),
                "{pasted}"
            );
        }
        let two = format!("\"{}\" b.rs", abs.display());
        assert_eq!(attachments(&two, dir.path()).unwrap().len(), 2);
    }

    #[test]
    fn text_or_missing_files_stay_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        assert!(attachments("a.txt and more words", dir.path()).is_none());
        assert!(attachments("missing.txt", dir.path()).is_none());
        assert!(attachments("   ", dir.path()).is_none());
        assert!(
            attachments(dir.path().to_str().unwrap(), dir.path()).is_none(),
            "folders are not files"
        );
    }
}
