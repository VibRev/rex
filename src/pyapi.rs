//! `ref <query>` over the engine's own Python API (CONVENTIONS §5).
//!
//! The install a tool runs on carries the API as `.py` files with
//! signatures and docstrings, and official examples next to them; both
//! match the running version exactly. They are read on every query, line by
//! line, with no Python and no cache: the largest install is ~200k lines and
//! reads in well under the time an agent waits for a command.
//!
//! What is indexed: module functions, classes, methods, properties
//! (`@property` and IDA's `name = property(...)`) and upper-case module
//! constants. Names starting with `_` are left out, as are the engine's raw
//! bindings (`_ida_*`, `_binaryninjacore`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Hits a query shows unless `--limit` says otherwise.
pub const DEFAULT_HITS: usize = 12;
/// Examples shown after the API hits.
const EXAMPLE_HITS: usize = 3;
/// Parameters a one-line record shows; an exact hit shows them all.
const PARAMS_SHOWN: usize = 140;
/// The full docstring of an exact hit is cut here.
const FULL_DOC_LINES: usize = 40;

/// How a file's path becomes the prefix of the names in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Naming {
    /// Flat modules named after their file: `ida_funcs.py` → `ida_funcs.`.
    Stem,
    /// A package: `binaryninja/binaryview.py` → `binaryninja.` when the
    /// package's `__init__.py` star-imports the module, else
    /// `binaryninja.binaryview.`.
    Package(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Function,
    Method,
    Property,
    Class,
    Constant,
}

impl Kind {
    /// The word a record starts with; a function or method has none.
    fn label(self) -> &'static str {
        match self {
            Kind::Function | Kind::Method => "",
            Kind::Property => "property ",
            Kind::Class => "class ",
            Kind::Constant => "const ",
        }
    }

    /// A constant matching a word ranks below a function matching it.
    fn weight(self) -> f64 {
        match self {
            Kind::Function | Kind::Method | Kind::Class => 1.0,
            Kind::Property => 0.9,
            Kind::Constant => 0.6,
        }
    }
}

/// One name in the API.
#[derive(Clone, Debug, PartialEq)]
pub struct Def {
    pub qualname: String,
    pub kind: Kind,
    /// Parameters without `self`; empty for a property or constant.
    pub params: String,
    pub returns: Option<String>,
    pub file: PathBuf,
    pub line: usize,
    /// Dedented, trimmed.
    pub doc: String,
    /// `doc` lower-cased, filled in by the index.
    lower: String,
}

/// One official example script.
#[derive(Clone, Debug, PartialEq)]
pub struct Example {
    pub path: PathBuf,
    /// Relative to the examples directory.
    pub name: String,
    pub summary: String,
    head: String,
    body: String,
}

#[derive(Clone, Debug, Default)]
pub struct ApiIndex {
    defs: Vec<Def>,
    examples: Vec<Example>,
}

/// A query's answer, best first.
#[derive(Clone, Debug)]
pub struct Hits<'i> {
    pub defs: Vec<&'i Def>,
    /// Defs whose full docstring the answer shows: the query named them
    /// exactly.
    pub exact: Vec<&'i Def>,
    pub examples: Vec<&'i Example>,
}

impl ApiIndex {
    /// Index the `.py` files under `api` and, when given, the example
    /// scripts under `examples`. Unreadable files are skipped: a partial
    /// index still answers.
    pub fn load(api: &Path, naming: Naming, examples: Option<&Path>) -> ApiIndex {
        let starred = match naming {
            Naming::Package(_) => star_imports(&std::fs::read_to_string(api.join("__init__.py")).unwrap_or_default()),
            Naming::Stem => HashSet::new(),
        };
        let files = python_files(api, matches!(naming, Naming::Package(_)));
        let defs = std::thread::scope(|s| {
            let chunks: Vec<_> = files
                .chunks(files.len().div_ceil(threads()).max(1))
                .map(|chunk| {
                    let starred = &starred;
                    s.spawn(move || {
                        chunk
                            .iter()
                            .filter_map(|file| {
                                let text = std::fs::read_to_string(file).ok()?;
                                let prefix = module_prefix(api, file, naming, starred)?;
                                Some(parse_module(&text, &prefix, file))
                            })
                            .flatten()
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            chunks.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        let examples = examples.map(load_examples).unwrap_or_default();
        ApiIndex::from_parts(defs, examples)
    }

    fn from_parts(defs: Vec<Def>, examples: Vec<Example>) -> ApiIndex {
        // A later definition of a name replaces an earlier one, as in Python
        // (ida_hexrays defines `decompile` twice).
        let mut last: HashMap<&str, usize> = HashMap::new();
        for (i, d) in defs.iter().enumerate() {
            last.insert(&d.qualname, i);
        }
        let keep: HashSet<usize> = last.into_values().collect();
        let defs =
            defs.into_iter().enumerate().filter(|(i, _)| keep.contains(i)).map(|(_, d)| Def { lower: d.doc.to_lowercase(), ..d }).collect();
        ApiIndex { defs, examples }
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Rank the API against `query`: words, `snake_case` or `CamelCase`
    /// names, or dotted names (`func_t.get_name`). A name hit outweighs any
    /// number of docstring hits.
    pub fn search(&self, query: &[String], limit: usize) -> Hits<'_> {
        let q = Query::new(query);
        if q.tokens.is_empty() {
            return Hits { defs: Vec::new(), exact: Vec::new(), examples: Vec::new() };
        }
        let n = self.defs.len().max(1) as f64;
        let idf: Vec<f64> = q
            .tokens
            .iter()
            .map(|t| {
                let df = self
                    .defs
                    .iter()
                    .filter(|d| has_word_prefix(d.doc_lower(), t) || name_tokens(&d.qualname).iter().any(|w| w.starts_with(t.as_str())))
                    .count();
                ((n + 1.0) / (df as f64 + 0.5)).ln().max(0.1)
            })
            .collect();
        let mut scored: Vec<(f64, &Def)> = self.defs.iter().map(|d| (q.score(d, &idf), d)).filter(|(s, _)| *s > 0.0).collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.qualname.len().cmp(&b.1.qualname.len())));
        let defs: Vec<&Def> = scored.iter().take(limit).map(|&(_, d)| d).collect();
        let exact = q.exact(&self.defs);
        let mut examples: Vec<(f64, &Example)> = self.examples.iter().map(|e| (q.score_example(e), e)).filter(|(s, _)| *s > 0.0).collect();
        examples.sort_by(|a, b| b.0.total_cmp(&a.0));
        Hits { defs, exact, examples: examples.into_iter().take(EXAMPLE_HITS).map(|(_, e)| e).collect() }
    }
}

impl Def {
    /// `ida_funcs.get_func(ea: ida_idaapi.ea_t) -> func_t * | /…/ida_funcs.py:674 | Get pointer to …`
    pub fn to_record(&self) -> String {
        self.to_record_with(PARAMS_SHOWN)
    }

    fn to_record_with(&self, params: usize) -> String {
        let mut head = format!("{}{}", self.kind.label(), self.qualname);
        match self.kind {
            Kind::Function | Kind::Method | Kind::Class => head.push_str(&format!("({})", cut(&self.params, params))),
            Kind::Property | Kind::Constant => {}
        }
        if let Some(ret) = &self.returns {
            head.push_str(if matches!(self.kind, Kind::Property) { ": " } else { " -> " });
            head.push_str(ret);
        }
        format!("{head} | {}:{} | {}", self.file.display(), self.line, summary(&self.doc))
    }

    /// The record, then the whole docstring indented, cut at
    /// [`FULL_DOC_LINES`].
    pub fn to_full_record(&self) -> String {
        let record = self.to_record_with(usize::MAX);
        // The docstring below holds the summary.
        let mut out = record.rsplit_once(" | ").map_or(record.clone(), |(head, _)| head.to_owned());
        let lines: Vec<&str> = self.doc.lines().collect();
        for line in lines.iter().take(FULL_DOC_LINES) {
            out.push_str("\n    ");
            out.push_str(&unrest(line));
        }
        if lines.len() > FULL_DOC_LINES {
            out.push_str(&format!("\n    … {} more lines at {}:{}", lines.len() - FULL_DOC_LINES, self.file.display(), self.line));
        }
        out
    }

    fn name(&self) -> &str {
        self.qualname.rsplit('.').next().unwrap_or(&self.qualname)
    }

    fn doc_lower(&self) -> &str {
        &self.lower
    }
}

impl Example {
    /// `example decompiler/vds1.py | /…/vds1.py:1 | summary`
    pub fn to_record(&self) -> String {
        format!("example {} | {}:1 | {}", self.name, self.path.display(), self.summary)
    }
}

impl Hits<'_> {
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty() && self.examples.is_empty()
    }

    /// Records in display order: exact names with their docstrings, the
    /// other hits, then examples.
    pub fn records(&self) -> Vec<String> {
        let exact: HashSet<&str> = self.exact.iter().map(|d| d.qualname.as_str()).collect();
        self.exact
            .iter()
            .map(|d| d.to_full_record())
            .chain(self.defs.iter().filter(|d| !exact.contains(d.qualname.as_str())).map(|d| d.to_record()))
            .chain(self.examples.iter().map(|e| e.to_record()))
            .collect()
    }
}

struct Query {
    /// Each word as typed, lower-cased: `get_func`, `func_t.get_name`.
    words: Vec<String>,
    /// The words split into name tokens: `get`, `func`, …
    tokens: Vec<String>,
}

impl Query {
    fn new(query: &[String]) -> Query {
        let words: Vec<String> = query.iter().flat_map(|q| q.split_whitespace()).map(str::to_lowercase).collect();
        let mut tokens: Vec<String> = Vec::new();
        for w in query.iter().flat_map(|q| q.split_whitespace()) {
            for t in name_tokens(w) {
                if !tokens.contains(&t) {
                    tokens.push(t);
                }
            }
        }
        Query { words, tokens }
    }

    fn score(&self, d: &Def, idf: &[f64]) -> f64 {
        let name = d.name().to_lowercase();
        let qual = d.qualname.to_lowercase();
        let own = name_tokens(d.name());
        let path = name_tokens(&d.qualname);
        let doc = d.doc_lower();
        let mut score = 0.0;
        let mut matched = 0;
        for (t, idf) in self.tokens.iter().zip(idf) {
            let in_name = if own.iter().any(|w| w == t) {
                10.0
            } else if own.iter().any(|w| w.starts_with(t.as_str())) {
                6.0
            } else if own.iter().any(|w| w.len() >= 4 && t.starts_with(w.as_str())) {
                // An abbreviation: `func` for `function`.
                4.0
            } else if path.iter().any(|w| w.starts_with(t.as_str())) {
                3.0
            } else {
                0.0
            };
            let in_doc = if has_word_prefix(doc, t) { 1.0 } else { 0.0 };
            if in_name + in_doc > 0.0 {
                matched += 1;
            }
            score += idf * (in_name + in_doc);
        }
        if score == 0.0 {
            return 0.0;
        }
        // With more words than one, a name equal to one of them is only a
        // partial answer.
        let scale = if self.words.len() == 1 { 1.0 } else { 0.15 };
        for w in &self.words {
            if qual == *w || qual.ends_with(&format!(".{w}")) {
                score += 200.0 * scale;
            } else if name == *w {
                score += 100.0 * scale;
            } else if name.starts_with(w.as_str()) {
                score += 20.0 * scale;
            }
        }
        // Every word found counts for more than one word found often.
        let coverage = matched as f64 / self.tokens.len() as f64;
        score * coverage * coverage * d.kind.weight() / (1.0 + 0.05 * own.len() as f64)
    }

    /// The defs the query names exactly: by qualified name or its dotted
    /// tail, else a name only one def has.
    fn exact<'i>(&self, defs: &'i [Def]) -> Vec<&'i Def> {
        let [word] = self.words.as_slice() else { return Vec::new() };
        let dotted: Vec<&Def> = defs
            .iter()
            .filter(|d| {
                word.contains('.') && {
                    let q = d.qualname.to_lowercase();
                    q == *word || q.ends_with(&format!(".{word}"))
                }
            })
            .collect();
        if !dotted.is_empty() {
            return dotted.into_iter().take(3).collect();
        }
        let named: Vec<&Def> = defs.iter().filter(|d| d.name().eq_ignore_ascii_case(word)).collect();
        if named.len() == 1 { named } else { Vec::new() }
    }

    /// An identifier in the query (`get_func`, `BinaryView.hlil`) ranks
    /// the examples that call it; plain words rank by the example's name and
    /// header, all of them needed up to two.
    fn score_example(&self, e: &Example) -> f64 {
        let idents: Vec<&str> = self.words.iter().map(String::as_str).filter(|w| w.contains(['_', '.'])).collect();
        if !idents.is_empty() {
            return idents.iter().map(|w| whole_word_count(&e.body, w.rsplit('.').next().unwrap_or(w)) as f64).sum::<f64>().min(10.0);
        }
        let name = name_tokens(&e.name);
        let head = e.head.to_lowercase();
        let mut score = 0.0;
        let mut matched = 0;
        for t in &self.tokens {
            let s = if name.iter().any(|w| w.starts_with(t.as_str())) {
                3.0
            } else if has_word_prefix(&head, t) {
                2.0
            } else {
                0.0
            };
            if s > 0.0 {
                matched += 1;
            }
            score += s;
        }
        if matched < self.tokens.len().min(2) { 0.0 } else { score }
    }
}

/// `s` cut to `n` characters, `…` marking the cut.
fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

/// How often `word` occurs in `text` as a whole identifier.
fn whole_word_count(text: &str, word: &str) -> usize {
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    text.match_indices(word)
        .filter(|&(at, _)| {
            let bytes = text.as_bytes();
            (at == 0 || !ident(bytes[at - 1])) && bytes.get(at + word.len()).is_none_or(|&b| !ident(b))
        })
        .count()
}

/// reST roles shown as their target: ``:py:class:`~binaryninja.function.Function` `` → `Function`.
fn unrest(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(":py:") {
        let after = &rest[at + 4..];
        let Some(tick) = after.find('`').filter(|&t| t <= 10 && after[..t].ends_with(':')) else {
            out.push_str(&rest[..at + 4]);
            rest = after;
            continue;
        };
        let Some(end) = after[tick + 1..].find('`') else { break };
        let target = &after[tick + 1..tick + 1 + end];
        out.push_str(&rest[..at]);
        out.push_str(match target.strip_prefix('~') {
            Some(t) => t.rsplit('.').next().unwrap_or(t),
            None => target,
        });
        rest = &after[tick + 2 + end..];
    }
    out.push_str(rest);
    out.replace("``", "")
}

/// `get_functions_at` → `get functions at`; `HighLevelILFunction` → `high
/// level il function`; `ida_funcs.func_t` → `ida funcs func t`.
fn name_tokens(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|p| !p.is_empty()) {
        let chars: Vec<char> = part.chars().collect();
        let mut start = 0;
        for i in 1..chars.len() {
            let (prev, cur) = (chars[i - 1], chars[i]);
            let next_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            if (prev.is_ascii_lowercase() && cur.is_ascii_uppercase())
                || (prev.is_ascii_uppercase() && cur.is_ascii_uppercase() && next_lower)
            {
                out.push(chars[start..i].iter().collect::<String>().to_lowercase());
                start = i;
            }
        }
        out.push(chars[start..].iter().collect::<String>().to_lowercase());
    }
    out
}

/// Whether `token` starts a word of `text` (both lower-case).
fn has_word_prefix(text: &str, token: &str) -> bool {
    text.match_indices(token).any(|(at, _)| at == 0 || !text.as_bytes()[at - 1].is_ascii_alphanumeric())
}

/// The first prose line of a docstring, cut at about 160 characters. An
/// IDA overload list (`This function has the following signatures:`) and
/// reST field lines are skipped.
fn summary(doc: &str) -> String {
    let line = doc
        .lines()
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !l.starts_with(':')
                && !l.starts_with("This function has the following signatures")
                && !l.starts_with('#')
                && !l.starts_with("..")
                && !l.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .unwrap_or("");
    let line = unrest(line);
    if line.chars().count() <= 160 {
        return line;
    }
    let cut: String = line.chars().take(160).collect();
    match cut.rfind(". ") {
        Some(at) if at > 60 => cut[..=at].to_owned(),
        _ => format!("{cut}…"),
    }
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, std::num::NonZero::get).min(8)
}

/// The `.py` files of an API: the directory's own for flat modules, the
/// whole tree for a package. Raw bindings (`_…`) are left out, and so are
/// `__pycache__` and the GUI toolkits IDA bundles.
fn python_files(dir: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if recursive && !name.starts_with(['_', '.']) && name != "examples" {
                    stack.push(path);
                }
            } else if name.ends_with(".py")
                && (name == "__init__.py" || !name.starts_with('_'))
                && !matches!(&*name, "init.py" | "idaapi.py")
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The modules `__init__.py` star-imports: `from .binaryview import *`.
fn star_imports(init: &str) -> HashSet<String> {
    init.lines().filter_map(|l| l.trim().strip_prefix("from .")?.strip_suffix("import *").map(|m| m.trim().to_owned())).collect()
}

fn module_prefix(root: &Path, file: &Path, naming: Naming, starred: &HashSet<String>) -> Option<String> {
    let stem = file.file_stem()?.to_str()?;
    match naming {
        Naming::Stem => Some(format!("{stem}.")),
        Naming::Package(package) => {
            let rel = file.strip_prefix(root).ok()?.with_extension("");
            let mut parts: Vec<&str> = rel.iter().filter_map(|p| p.to_str()).collect();
            if parts.last() == Some(&"__init__") {
                parts.pop();
            }
            let prefix = match parts.as_slice() {
                [one] if starred.contains(*one) => String::new(),
                _ => parts.iter().map(|p| format!("{p}.")).collect(),
            };
            Some(format!("{package}.{prefix}"))
        }
    }
}

/// The defs in one module's text, in order.
fn parse_module(text: &str, prefix: &str, file: &Path) -> Vec<Def> {
    let lines: Vec<&str> = text.lines().collect();
    let mut defs = Vec::new();
    // Open classes, innermost last: (indent, qualified name, index in defs).
    let mut classes: Vec<(usize, String, usize)> = Vec::new();
    let mut decorators: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        let indent = indent_of(line);
        while classes.last().is_some_and(|c| indent <= c.0) {
            classes.pop();
        }
        // One whole statement: brackets and strings may run over lines.
        let (stmt, next) = statement(&lines, i);
        let owner = classes.last().map(|c| (c.1.clone(), c.2));
        if let Some(rest) = trimmed.strip_prefix('@') {
            decorators.push(rest.trim());
            i = next;
            continue;
        }
        let head = stmt.trim_start().strip_prefix("async ").unwrap_or(stmt.trim_start());
        if let Some(rest) = head.strip_prefix("def ").or_else(|| head.strip_prefix("class ")) {
            let is_class = head.starts_with("class ");
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let (doc, after) = docstring(&lines, next);
            let in_fn = owner.is_none() && indent > 0;
            let base = match &owner {
                Some((q, _)) => format!("{q}."),
                None => prefix.to_owned(),
            };
            let public = !name.starts_with('_');
            if is_class {
                let qualname = format!("{base}{name}");
                if public && !in_fn {
                    defs.push(Def {
                        qualname: qualname.clone(),
                        kind: Kind::Class,
                        params: String::new(),
                        returns: None,
                        file: file.to_path_buf(),
                        line: i + 1,
                        doc,
                        lower: String::new(),
                    });
                    classes.push((indent, qualname, defs.len() - 1));
                } else {
                    // Its body is skipped below by indentation.
                    classes.push((indent, format!("{base}_{name}"), usize::MAX));
                }
            } else if let Some((params, returns)) = signature(rest) {
                let is_property = decorators.iter().any(|d| *d == "property" || d.ends_with(".getter"));
                let is_setter = decorators.iter().any(|d| d.ends_with(".setter") || d.ends_with(".deleter"));
                if name == "__init__"
                    && let Some((_, at)) = &owner
                    && let Some(class) = defs.get_mut(*at)
                {
                    class.params = drop_self(&params);
                } else if public && !is_setter && !in_fn && owner.as_ref().is_none_or(|(_, at)| *at != usize::MAX) {
                    let (params, returns, doc) = swig_overloads(&name, params, returns, doc);
                    let kind = match (&owner, is_property) {
                        (_, true) => Kind::Property,
                        (Some(_), false) => Kind::Method,
                        (None, false) => Kind::Function,
                    };
                    let params = if owner.is_some() { drop_self(&params) } else { params };
                    defs.push(Def {
                        qualname: format!("{base}{name}"),
                        kind,
                        params: if kind == Kind::Property { String::new() } else { params },
                        returns,
                        file: file.to_path_buf(),
                        line: i + 1,
                        doc,
                        lower: String::new(),
                    });
                }
            }
            decorators.clear();
            // Skip the body: a nested def is not part of the API.
            i = if is_class { after } else { skip_block(&lines, after, indent) };
            continue;
        }
        decorators.clear();
        if let Some(def) = assignment(head, owner.as_ref().map(|(q, at)| (q.as_str(), *at)), prefix, file, i + 1, &lines, next) {
            let (doc, after) = docstring(&lines, next);
            defs.push(Def { doc, ..def });
            i = after;
            continue;
        }
        i = next;
    }
    defs
}

/// `NAME = …` at module level, or `name[: type] = property(…)` in a
/// class, followed by its docstring.
fn assignment(
    head: &str,
    owner: Option<(&str, usize)>,
    prefix: &str,
    file: &Path,
    line: usize,
    lines: &[&str],
    next: usize,
) -> Option<Def> {
    let (target, value) = head.split_once('=')?;
    if value.starts_with('=') || target.contains(['(', '[', '.', ',']) || target.ends_with(['!', '<', '>', '+', '-', '*', '/']) {
        return None;
    }
    let (name, annotation) = match target.split_once(':') {
        Some((n, a)) => (n.trim(), Some(a.trim().trim_matches(['\'', '"']).to_owned())),
        None => (target.trim(), None),
    };
    if name.is_empty() || name.starts_with('_') || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let value = value.trim();
    match owner {
        Some((_, usize::MAX)) => None,
        Some((class, _)) if value.starts_with("property(") && name != "thisown" => Some(Def {
            qualname: format!("{class}.{name}"),
            kind: Kind::Property,
            params: String::new(),
            returns: annotation,
            file: file.to_path_buf(),
            line,
            doc: String::new(),
            lower: String::new(),
        }),
        Some(_) => None,
        // Upper-case constants only, and only documented ones or ones the
        // engine binds: plain module state is not API.
        None if name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            && name.chars().any(|c| c.is_ascii_uppercase())
            && (starts_docstring(lines.get(next).copied()) || value.starts_with("_ida")) =>
        {
            Some(Def {
                qualname: format!("{prefix}{name}"),
                kind: Kind::Constant,
                params: String::new(),
                returns: None,
                file: file.to_path_buf(),
                line,
                doc: String::new(),
                lower: String::new(),
            })
        }
        None => None,
    }
}

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

fn starts_docstring(line: Option<&str>) -> bool {
    line.map(str::trim_start).is_some_and(|l| {
        let l = l.trim_start_matches(['r', 'u', 'R', 'U']);
        l.starts_with("\"\"\"") || l.starts_with("'''")
    })
}

/// The statement starting at line `i`, joined, and the line after it.
/// Brackets and triple-quoted strings keep a statement open.
fn statement(lines: &[&str], i: usize) -> (String, usize) {
    let mut depth = 0i32;
    let mut quote: Option<&str> = None;
    let mut out = String::new();
    let mut j = i;
    while j < lines.len() {
        let line = lines[j];
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line.trim());
        let bytes = line.as_bytes();
        let mut k = 0;
        while k < bytes.len() {
            let rest = &line[k..];
            if let Some(q) = quote {
                if rest.starts_with('\\') {
                    k += 2;
                    continue;
                }
                if rest.starts_with(q) {
                    quote = None;
                    k += q.len();
                    continue;
                }
                k += 1;
                continue;
            }
            match bytes[k] {
                b'#' => break,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b'"' | b'\'' => {
                    let q = if rest.starts_with("\"\"\"") {
                        "\"\"\""
                    } else if rest.starts_with("'''") {
                        "'''"
                    } else if bytes[k] == b'"' {
                        "\""
                    } else {
                        "'"
                    };
                    quote = Some(q);
                    k += q.len();
                    continue;
                }
                _ => {}
            }
            k += 1;
        }
        // A one-line string never runs past its line.
        if matches!(quote, Some("\"" | "'")) {
            quote = None;
        }
        j += 1;
        let continued = line.trim_end().ends_with('\\');
        if depth <= 0 && quote.is_none() && !continued {
            break;
        }
    }
    (out, j)
}

/// The docstring starting at line `i` if there is one, dedented, and the
/// line after it.
fn docstring(lines: &[&str], i: usize) -> (String, usize) {
    let mut j = i;
    while j < lines.len() && lines[j].trim().is_empty() {
        j += 1;
    }
    let Some(first) = lines.get(j) else { return (String::new(), i) };
    let t = first.trim_start().trim_start_matches(['r', 'u', 'R', 'U']);
    let quote = if t.starts_with("\"\"\"") {
        "\"\"\""
    } else if t.starts_with("'''") {
        "'''"
    } else {
        return (String::new(), i);
    };
    let body = &t[3..];
    if let Some(end) = body.find(quote) {
        return (body[..end].trim().to_owned(), j + 1);
    }
    let mut parts = vec![body];
    let mut k = j + 1;
    while k < lines.len() {
        if let Some(end) = lines[k].find(quote) {
            parts.push(&lines[k][..end]);
            k += 1;
            break;
        }
        parts.push(lines[k]);
        k += 1;
    }
    (dedent(&parts), k)
}

fn dedent(parts: &[&str]) -> String {
    let common = parts.iter().skip(1).filter(|l| !l.trim().is_empty()).map(|l| indent_bytes(l)).min().unwrap_or(0);
    let mut out: Vec<&str> = vec![parts[0].trim()];
    out.extend(parts.iter().skip(1).map(|l| if l.len() >= common { l[common..].trim_end() } else { l.trim() }));
    out.join("\n").trim().to_owned()
}

fn indent_bytes(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Past the body of the block whose header is indented `indent`.
fn skip_block(lines: &[&str], mut i: usize, indent: usize) -> usize {
    while i < lines.len() {
        let l = lines[i];
        if !l.trim().is_empty() && indent_of(l) <= indent {
            break;
        }
        // A string in the body may hold text at any indent.
        let (_, next) = statement(lines, i);
        i = next.max(i + 1);
    }
    i
}

/// `name(params) -> ret:` after `def ` → its parameters and return
/// annotation, both with whitespace folded and the annotation unquoted.
fn signature(rest: &str) -> Option<(String, Option<String>)> {
    let open = rest.find('(')?;
    let mut depth = 0;
    let mut close = None;
    for (at, c) in rest[open..].char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + at);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let params = fold(&rest[open + 1..close]).trim_end_matches(',').trim().to_owned();
    let tail = rest[close + 1..].trim();
    let returns = tail
        .strip_prefix("->")
        .map(|r| r.rsplit_once(':').map_or(r, |(r, _)| r).trim().trim_matches(['\'', '"']).to_owned())
        .filter(|r| !r.is_empty());
    Some((params, returns))
}

fn fold(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").replace("( ", "(").replace(" )", ")")
}

fn drop_self(params: &str) -> String {
    let mut parts = params.splitn(2, ',');
    let first = parts.next().unwrap_or("").trim();
    if first == "self" || first == "cls" || first.starts_with("self:") {
        parts.next().unwrap_or("").trim().to_owned()
    } else {
        params.to_owned()
    }
}

/// An IDA SWIG wrapper takes `*args` and lists its real overloads at the
/// top of its docstring (`0. decompile(mbr: …) -> cfuncptr_t`): the first
/// one not marked deprecated becomes the signature, the others are noted.
fn swig_overloads(name: &str, params: String, returns: Option<String>, doc: String) -> (String, Option<String>, String) {
    if params != "*args" {
        return (params, returns, doc);
    }
    let overloads: Vec<(String, Option<String>)> = doc
        .lines()
        .map(str::trim)
        .filter_map(|l| {
            let (n, sig) = l.split_once(". ")?;
            if !n.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let rest = sig.strip_prefix(name)?;
            let (p, r) = signature(&format!("{rest}:"))?;
            Some((p, r))
        })
        .collect();
    let Some(pick) = overloads.iter().find(|(_, r)| !r.as_deref().is_some_and(|r| r.contains("DEPRECATED"))).or(overloads.first()) else {
        return (params, returns, doc);
    };
    let more = overloads.len() - 1;
    let params = if more > 0 { format!("{}  /* +{more} overload{} */", pick.0, if more == 1 { "" } else { "s" }) } else { pick.0.clone() };
    (params, pick.1.clone().or(returns), doc)
}

fn load_examples(dir: &Path) -> Vec<Example> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "py") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let name = path.strip_prefix(dir).ok()?.to_string_lossy().into_owned();
            let head = example_head(&text);
            let summary = example_summary(&head);
            Some(Example { body: text.to_lowercase(), name, summary, head, path })
        })
        .collect()
}

/// The module docstring, else the leading `#` comments.
fn example_head(text: &str) -> String {
    let lines: Vec<&str> = text.lines().skip_while(|l| l.starts_with("#!") || l.contains("coding")).collect();
    let (doc, _) = docstring(&lines, 0);
    if !doc.is_empty() {
        return doc;
    }
    lines
        .iter()
        .map(|l| l.trim())
        .skip_while(|l| l.is_empty())
        .take_while(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim())
        .collect::<Vec<_>>()
        .join("\n")
}

/// IDA's examples say `summary: …`; others get their first prose line.
fn example_summary(head: &str) -> String {
    head.lines().find_map(|l| l.trim().strip_prefix("summary:").map(|s| s.trim().to_owned())).unwrap_or_else(|| {
        head.lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.to_lowercase().contains("copyright") && !l.contains("Vector 35"))
            .unwrap_or("")
            .chars()
            .take(160)
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDA: &str = r#"
import _ida_funcs

FUNC_NORET = _ida_funcs.FUNC_NORET
"""Function doesn't return
        """
helper_state = {}

class func_t(ida_range.range_t):
    thisown = property(lambda x: x.this.own(), lambda x, v: x.this.own(v),
        doc='The membership flag')
    flags: 'uint64' = property(_ida_funcs.func_t_flags_get, _ida_funcs.
        func_t_flags_set)
    """Function flags
        """

    def __init__(self, *args):
        pass

    def get_name(self):
        """Get the function name

:returns: the function name"""
        return _ida_funcs.func_t_get_name(self)

def get_func(ea: ida_idaapi.ea_t) ->'func_t *':
    """Get pointer to function structure by address.

:param ea: any address in a function
:returns: ptr to a function or nullptr."""
    def inner():
        pass
    return _ida_funcs.get_func(ea)

def decompile(*args) ->'cfuncptr_t':
    """This function has the following signatures:

    0. decompile(mbr: const mba_ranges_t &, hf: hexrays_failure_t *=nullptr) -> DEPRECATED cfuncptr_t
    1. decompile(dcr: const decomp_ranges_t &, hf: hexrays_failure_t *=nullptr) -> cfuncptr_t

# 0: decompile(mbr) -> DEPRECATED cfuncptr_t

Decompile a snippet or a function.
"""
    return _ida_hexrays.decompile(*args)

def get_func_name(ea, very_long_parameter_name_one=None,
        very_long_parameter_name_two=0):
    """Return the name of the function containing ea."""
    pass

def _private():
    pass
"#;

    const BN: &str = "class BinaryView:\n\t\"\"\"\n\t``class BinaryView`` implements a view on binary data.\n\t\"\"\"\n\tdef __init__(self, file_metadata=None, handle=None):\n\t\tpass\n\n\t@property\n\tdef functions(self) -> 'FunctionList':\n\t\t\"\"\"returns a FunctionList object (read-only)\"\"\"\n\t\treturn x\n\n\t@functions.setter\n\tdef functions(self, v):\n\t\tpass\n\n\tdef get_functions_at(self, addr: int) -> List['_function.Function']:\n\t\t\"\"\"\n\n\t\t``get_functions_at`` get a list of Function objects that start at the\n\t\tgiven virtual address.\n\t\t\"\"\"\n\t\treturn []\n\n\tclass _Inner:\n\t\tdef hidden(self):\n\t\t\tpass\n\n\tdef hlil_at(self, addr):\n\t\treturn 1\n";

    fn ida() -> ApiIndex {
        ApiIndex::from_parts(parse_module(IDA, "ida_funcs.", Path::new("/ida/python/ida_funcs.py")), Vec::new())
    }

    fn bn() -> ApiIndex {
        ApiIndex::from_parts(parse_module(BN, "binaryninja.", Path::new("/bn/binaryview.py")), Vec::new())
    }

    fn q(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    fn names(index: &ApiIndex) -> Vec<&str> {
        index.defs.iter().map(|d| d.qualname.as_str()).collect()
    }

    #[test]
    fn functions_classes_properties_and_constants_are_indexed() {
        let index = ida();
        assert_eq!(
            names(&index),
            [
                "ida_funcs.FUNC_NORET",
                "ida_funcs.func_t",
                "ida_funcs.func_t.flags",
                "ida_funcs.func_t.get_name",
                "ida_funcs.get_func",
                "ida_funcs.decompile",
                "ida_funcs.get_func_name"
            ]
        );
        let flags = &index.defs[2];
        assert_eq!((flags.kind, flags.returns.as_deref(), flags.doc.as_str()), (Kind::Property, Some("uint64"), "Function flags"));
        assert_eq!(index.defs[1].params, "*args", "the class takes its __init__ parameters");
        assert_eq!(index.defs[3].params, "", "self is dropped");
        assert_eq!(index.defs[0].doc, "Function doesn't return");
    }

    #[test]
    fn a_record_has_signature_place_and_summary() {
        let index = ida();
        let get_func = index.defs.iter().find(|d| d.qualname == "ida_funcs.get_func").unwrap();
        assert_eq!(
            get_func.to_record(),
            "ida_funcs.get_func(ea: ida_idaapi.ea_t) -> func_t * | /ida/python/ida_funcs.py:26 | Get pointer to function structure by address."
        );
        let long = index.defs.iter().find(|d| d.qualname == "ida_funcs.get_func_name").unwrap();
        assert_eq!(long.params, "ea, very_long_parameter_name_one=None, very_long_parameter_name_two=0");
        assert!(index.defs[2].to_record().starts_with("property ida_funcs.func_t.flags: uint64 | "), "{}", index.defs[2].to_record());
    }

    #[test]
    fn swig_overloads_give_the_signature() {
        let index = ida();
        let d = index.defs.iter().find(|d| d.qualname == "ida_funcs.decompile").unwrap();
        assert_eq!(d.params, "dcr: const decomp_ranges_t &, hf: hexrays_failure_t *=nullptr  /* +1 overload */");
        assert_eq!(d.returns.as_deref(), Some("cfuncptr_t"));
        assert_eq!(summary(&d.doc), "Decompile a snippet or a function.");
    }

    #[test]
    fn package_classes_methods_and_properties() {
        let index = bn();
        assert_eq!(
            names(&index),
            [
                "binaryninja.BinaryView",
                "binaryninja.BinaryView.functions",
                "binaryninja.BinaryView.get_functions_at",
                "binaryninja.BinaryView.hlil_at"
            ]
        );
        let class = &index.defs[0];
        assert_eq!(class.params, "file_metadata=None, handle=None");
        assert_eq!(summary(&class.doc), "class BinaryView implements a view on binary data.");
        assert_eq!(index.defs[1].kind, Kind::Property, "the setter is not a second entry");
        assert_eq!(index.defs[2].returns.as_deref(), Some("List['_function.Function']"));
    }

    #[test]
    fn a_name_beats_a_docstring_and_an_exact_name_shows_its_docs() {
        let index = ida();
        let hits = index.search(&q(&["get_func"]), 10);
        assert_eq!(hits.defs[0].qualname, "ida_funcs.get_func");
        assert_eq!(hits.exact.len(), 1);
        let records = hits.records();
        assert!(records[0].contains("\n    :param ea: any address in a function"), "{}", records[0]);

        let hits = index.search(&q(&["function", "name"]), 10);
        assert_eq!(hits.defs[0].qualname, "ida_funcs.get_func_name", "{:?}", hits.defs.iter().map(|d| &d.qualname).collect::<Vec<_>>());
        assert!(hits.exact.is_empty());
    }

    #[test]
    fn dotted_and_camel_case_queries() {
        let index = bn();
        let hits = index.search(&q(&["BinaryView.get_functions_at"]), 5);
        assert_eq!(hits.exact[0].qualname, "binaryninja.BinaryView.get_functions_at");
        let hits = index.search(&q(&["hlil"]), 5);
        assert_eq!(hits.defs[0].qualname, "binaryninja.BinaryView.hlil_at");
        assert_eq!(name_tokens("HighLevelILFunction"), ["high", "level", "il", "function"]);
        assert_eq!(name_tokens("ida_funcs.func_t"), ["ida", "funcs", "func", "t"]);
        assert!(index.search(&q(&["zzzz"]), 5).is_empty());
    }

    #[test]
    fn rest_roles_read_as_their_target() {
        assert_eq!(
            unrest("a list of :py:class:`~binaryninja.function.Function` objects, see :py:func:`get_x` or ``y``"),
            "a list of Function objects, see get_x or y"
        );
        assert_eq!(unrest("odd :py: text"), "odd :py: text");
    }

    #[test]
    fn package_names_follow_star_imports() {
        let starred = star_imports("import os\nfrom .binaryview import *\nfrom .enums import *\n");
        let root = Path::new("/bn/python/binaryninja");
        assert_eq!(module_prefix(root, &root.join("binaryview.py"), Naming::Package("binaryninja"), &starred).unwrap(), "binaryninja.");
        assert_eq!(
            module_prefix(root, &root.join("debugger/debuggercontroller.py"), Naming::Package("binaryninja"), &starred).unwrap(),
            "binaryninja.debugger.debuggercontroller."
        );
        assert_eq!(
            module_prefix(root, &root.join("debugger/__init__.py"), Naming::Package("binaryninja"), &starred).unwrap(),
            "binaryninja.debugger."
        );
    }

    #[test]
    fn examples_rank_by_name_header_and_the_api_they_call() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("decompiler")).unwrap();
        std::fs::write(dir.path().join("decompiler/vds1.py"), "\"\"\"\nsummary: decompile and print the current function\n\nkeywords: decompiler\n\"\"\"\nimport ida_hexrays\nida_hexrays.decompile(ea)\n").unwrap();
        std::fs::write(dir.path().join("list_strings.py"), "# List the strings\nimport idautils\n").unwrap();
        let examples = load_examples(dir.path());
        assert_eq!(examples[0].summary, "decompile and print the current function");
        assert_eq!(examples[1].summary, "List the strings");
        let index = ApiIndex::from_parts(Vec::new(), examples);
        let hits = index.search(&q(&["decompile"]), 5);
        assert_eq!(hits.examples.len(), 1);
        assert!(hits.records()[0].starts_with("example decompiler/vds1.py | "), "{:?}", hits.records());
    }

    /// Against the installs on this machine, when there are any:
    /// `REX_IDA_PYTHON=<IDA>/python REX_BN_PYTHON=<Binary Ninja>/python cargo test -- --ignored`.
    #[test]
    #[ignore = "needs an engine install"]
    fn real_installs_answer_quickly() {
        for (var, naming, sub, query, want) in [
            ("REX_IDA_PYTHON", Naming::Stem, "examples", "get_func", "ida_funcs.get_func"),
            (
                "REX_BN_PYTHON",
                Naming::Package("binaryninja"),
                "../examples/python",
                "get_functions_at",
                "binaryninja.BinaryView.get_functions_at",
            ),
        ] {
            let Some(dir) = std::env::var_os(var) else { continue };
            let dir = PathBuf::from(dir);
            let api = if matches!(naming, Naming::Package(_)) { dir.join("binaryninja") } else { dir.clone() };
            let started = std::time::Instant::now();
            let index = ApiIndex::load(&api, naming, Some(&api.join(sub)));
            let hits = index.search(&q(&[query]), DEFAULT_HITS);
            let took = started.elapsed();
            eprintln!("{var}: {} defs, {} examples, {took:?}\n{}", index.len(), index.examples.len(), hits.records().join("\n"));
            assert_eq!(hits.defs[0].qualname, want);
            assert!(took.as_millis() < 1500, "{took:?}");
        }
    }
}
