//! Conformance R-13: every failure rex or axkit reports carries a `hint:`
//! (CONVENTIONS §6). Checked on the source: each `Failure::new(...)` outside
//! the tests is followed by `.with_hint(`, unless a `// hint:` comment on
//! its line says where the hint is added.

// The helpers below are test code too; clippy only exempts `#[test]` fns.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = fs::read_to_string(&path).unwrap();
            // The tests build failures to look at, not to report.
            let text = text.split("#[cfg(test)]").next().unwrap_or_default().to_owned();
            out.push((path.display().to_string(), text));
        }
    }
}

/// The end of the call that opens at `open` (the index of its `(`).
fn call_end(text: &str, open: usize) -> usize {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in text[open..].char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    return open + i + 1;
                }
            }
            _ => {}
        }
    }
    panic!("unclosed Failure::new call")
}

#[test]
fn every_reported_failure_has_a_hint() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    sources(&root.join("../axkit/src"), &mut files);
    let mut bare = Vec::new();
    for (path, text) in &files {
        for (at, _) in text.match_indices("Failure::new(") {
            let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
            let line = &text[line_start..line_end];
            if line.trim_start().starts_with("//") || line.contains("// hint:") || text[..at].ends_with("fn ") {
                continue;
            }
            let end = call_end(text, at + "Failure::new".len());
            if !text[end..].trim_start().starts_with(".with_hint(") {
                bare.push(format!("{path}:{}: {}", text[..at].lines().count(), line.trim()));
            }
        }
    }
    assert!(files.len() > 10, "found the sources: {}", files.len());
    assert!(bare.is_empty(), "failures without a hint:\n{}", bare.join("\n"));
}
