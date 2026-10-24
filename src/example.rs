//! The shell examples of a skill, pulled out for the tool's tests to run
//! (CONVENTIONS §5: every example runs as written).
//!
//! Three places hold examples:
//!
//! - fenced code blocks, one command per line starting with the tool's
//!   name (a quoted argument or a `<<'EOF'` here-document may go on over
//!   the next lines);
//! - lines indented by four spaces, as a `ref` corpus writes them: a
//!   command as in a block, or else Python, where consecutive lines are one
//!   script for `<tool> exec <target> '...'`;
//! - tables with a `code` column, each cell of which is code for
//!   `<tool> exec <target> '...'`.

/// One example: shell text to run with `sh -c`, and the exit status it
/// should end with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Example {
    /// 1-based line in the markdown, for failure messages.
    pub line: usize,
    pub script: String,
    /// From a trailing `# exit N`; 0 without one.
    pub exit: u8,
}

/// Every example in `markdown` for `tool`, in document order; table cells
/// run against `target`.
///
/// # Panics
///
/// An example's quote or here-document is never closed: the markdown is
/// wrong.
pub fn examples(markdown: &str, tool: &str, target: &str) -> Vec<Example> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = Vec::new();
    let mut fenced = false;
    let mut code_column = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let start = i + 1;
        i += 1;
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            if line.starts_with(&format!("{tool} ")) {
                let (script, exit) = command(&lines, &mut i, line, "", start);
                out.push(Example { line: start, script, exit });
            }
        } else if let Some(first) = line.strip_prefix(INDENT) {
            let mut code: Vec<&str> = Vec::new();
            let mut code_start = start;
            let mut current = Some(first);
            while let Some(text) = current {
                if text.starts_with(&format!("{tool} ")) {
                    flush(&mut out, &mut code, code_start, tool, target);
                    let at = i;
                    let (script, exit) = command(&lines, &mut i, text, INDENT, at);
                    out.push(Example { line: at, script, exit });
                } else {
                    if code.is_empty() {
                        code_start = i;
                    }
                    code.push(text);
                }
                current = lines.get(i).and_then(|l| l.strip_prefix(INDENT));
                if current.is_some() {
                    i += 1;
                }
            }
            flush(&mut out, &mut code, code_start, tool, target);
        } else if let Some(cells) = row(line) {
            match code_column {
                None => code_column = cells.iter().position(|c| c == "code"),
                Some(_) if cells.iter().all(|c| c.chars().all(|ch| ch == '-' || ch == ':')) => {}
                Some(column) => {
                    let Some(code) = cells.get(column).and_then(|c| c.strip_prefix('`')).and_then(|c| c.strip_suffix('`')) else {
                        continue;
                    };
                    out.push(Example { line: start, script: format!("{tool} exec {target} {}", quote(code)), exit: 0 });
                }
            }
        } else {
            code_column = None;
        }
    }
    out
}

const INDENT: &str = "    ";

/// The command starting with `first` (line `start`), with the lines a
/// quote or a here-document carries on over, `indent` taken off each; `i`
/// moves past them. Returns the script and its expected exit status.
fn command(lines: &[&str], i: &mut usize, first: &str, indent: &str, start: usize) -> (String, u8) {
    let next_line = |i: &mut usize, what: &str| {
        let next = lines.get(*i).unwrap_or_else(|| panic!("line {start}: {what}"));
        *i += 1;
        next.strip_prefix(indent).unwrap_or(next)
    };
    let mut script = first.to_owned();
    while script.matches('\'').count() % 2 == 1 {
        script.push('\n');
        script.push_str(next_line(i, "a quote is never closed"));
    }
    if script.ends_with("<<'EOF'") || script.ends_with("<<EOF") {
        loop {
            let next = next_line(i, "a here-document never ends");
            script.push('\n');
            script.push_str(next);
            if next.trim() == "EOF" {
                break;
            }
        }
    }
    expected_exit(&script)
}

/// The Python lines gathered in `code` as one `exec` example from line
/// `line`, if there are any.
fn flush(out: &mut Vec<Example>, code: &mut Vec<&str>, line: usize, tool: &str, target: &str) {
    if code.is_empty() {
        return;
    }
    let code = std::mem::take(code).join("\n");
    out.push(Example { line, script: format!("{tool} exec {target} {}", quote(&code)), exit: 0 });
}

/// `text` as one single-quoted shell word.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// A table row's cells, `\|` unescaped; `None` for a line outside a table.
fn row(line: &str) -> Option<Vec<String>> {
    let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                cell.push('|');
            }
            '|' => cells.push(std::mem::take(&mut cell).trim().to_owned()),
            c => cell.push(c),
        }
    }
    cells.push(cell.trim().to_owned());
    Some(cells)
}

/// `script` without a trailing `# exit N`, and N (0 without one).
fn expected_exit(script: &str) -> (String, u8) {
    let last = script.rsplit('\n').next().unwrap_or(script);
    match last.rsplit_once(" # exit ").and_then(|(_, n)| n.trim().parse().ok()) {
        Some(n) => {
            let cut = script.len() - last.len() + last.rfind(" # exit ").unwrap_or(last.len());
            (script[..cut].trim_end().to_owned(), n)
        }
        None => (script.to_owned(), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "\
intro `tx exec fw.bin -c 1` is prose, not an example

```sh
tx open fw.bin
not a command
tx exec fw.bin -c 'a = 1
a'
tx exec fw.bin -c 'nope'   # exit 1
tx exec fw.bin - <<'EOF'
print('x')
EOF
```

| question | code |
|---|---|
| one | `len(\"a\\|b\")` |
| prose | no code here |

| helper | gives |
|---|---|
| `hx(ea)` | not run: no code column |
";

    #[test]
    fn commands_in_blocks_and_code_cells_in_tables() {
        let got = examples(MD, "tx", "fw.bin");
        let scripts: Vec<(&str, u8)> = got.iter().map(|e| (e.script.as_str(), e.exit)).collect();
        assert_eq!(
            scripts,
            vec![
                ("tx open fw.bin", 0),
                ("tx exec fw.bin -c 'a = 1\na'", 0),
                ("tx exec fw.bin -c 'nope'", 1),
                ("tx exec fw.bin - <<'EOF'\nprint('x')\nEOF", 0),
                ("tx exec fw.bin 'len(\"a|b\")'", 0),
            ]
        );
        assert_eq!(got[0].line, 4);
    }

    const CORPUS: &str = "\
# t: a topic
## an entry
Prose.
    tx exec fw.bin -c 'a = 1
    a'
    len(bv.functions)
    [f.name for f in bv.functions
     if f.name]
    tx save fw.bin   # exit 1

## stdin
    tx exec fw.bin - <<'EOF'
    print('x')
    EOF
    x = 'q'
";

    #[test]
    fn indented_commands_and_python_in_a_corpus() {
        let got = examples(CORPUS, "tx", "fw.bin");
        let scripts: Vec<(usize, &str, u8)> = got.iter().map(|e| (e.line, e.script.as_str(), e.exit)).collect();
        assert_eq!(
            scripts,
            vec![
                (4, "tx exec fw.bin -c 'a = 1\na'", 0),
                (6, "tx exec fw.bin 'len(bv.functions)\n[f.name for f in bv.functions\n if f.name]'", 0),
                (9, "tx save fw.bin", 1),
                (12, "tx exec fw.bin - <<'EOF'\nprint('x')\nEOF", 0),
                (15, r"tx exec fw.bin 'x = '\''q'\'''", 0),
            ]
        );
    }

    #[test]
    fn quotes_in_a_cell_are_escaped_for_the_shell() {
        let got = examples("| code |\n|---|\n| `f('x')` |\n", "tx", "t");
        assert_eq!(got[0].script, r"tx exec t 'f('\''x'\'')'");
    }
}
