//! `ref`: API notes bundled into the tool (CONVENTIONS §5, AX §9 L3).
//!
//! A corpus is markdown, one text per topic: the first line is
//! `# <topic>: <about>`, each entry a `## <title>` section of a one-line
//! description and a shortest example. A query that names a topic shows the
//! whole topic; any other ranks entries by how often its words occur, title
//! words counting thrice.
//!
//! The corpus holds only what is the tool's own (helpers, sessions,
//! headless pitfalls). The engine's API reference and official examples are
//! pointed at in the install the tool runs on ([`Docs`]), whose files match
//! the running version exactly.

use crate::pyapi::{ApiIndex, Naming};
use std::path::{Path, PathBuf};

/// Entries a word query shows unless `--limit` says otherwise.
pub const DEFAULT_HITS: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Corpus {
    topics: Vec<Topic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topic {
    pub name: &'static str,
    pub about: &'static str,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub topic: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

/// What `ref` answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer<'c> {
    /// No query: every topic.
    Topics(&'c [Topic]),
    /// The query named a topic.
    Topic(&'c Topic),
    /// Best matches first.
    Hits(Vec<&'c Entry>),
}

impl Corpus {
    /// The topics in `texts`, in that order; a text without a
    /// `# <topic>: <about>` first line is skipped.
    pub fn parse(texts: &[&'static str]) -> Corpus {
        Corpus { topics: texts.iter().filter_map(|t| Topic::parse(t)).collect() }
    }

    pub fn topics(&self) -> &[Topic] {
        &self.topics
    }

    pub fn lookup(&self, query: &[String]) -> Answer<'_> {
        if query.is_empty() {
            return Answer::Topics(&self.topics);
        }
        if let [one] = query
            && let Some(topic) = self.topics.iter().find(|t| t.name.eq_ignore_ascii_case(one))
        {
            return Answer::Topic(topic);
        }
        let words: Vec<String> = query.iter().flat_map(|q| q.split_whitespace()).map(str::to_lowercase).collect();
        let mut scored: Vec<(usize, &Entry)> =
            self.topics.iter().flat_map(|t| &t.entries).map(|e| (e.score(&words), e)).filter(|(score, _)| *score > 0).collect();
        // Stable: equal scores keep corpus order.
        scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        Answer::Hits(scored.into_iter().map(|(_, e)| e).collect())
    }
}

impl Topic {
    fn parse(text: &'static str) -> Option<Topic> {
        let (head, rest) = text.split_once('\n').unwrap_or((text, ""));
        let (name, about) = head.strip_prefix("# ")?.split_once(':')?;
        let name = name.trim();
        let first = rest.find("## ").filter(|&at| at == 0 || rest[..at].ends_with('\n'))?;
        let entries = rest[first + 3..]
            .split("\n## ")
            .map(|section| {
                let (title, body) = section.split_once('\n').unwrap_or((section, ""));
                Entry { topic: name, title: title.trim(), body }
            })
            .collect();
        Some(Topic { name, about: about.trim(), entries })
    }
}

impl Entry {
    /// The entry as one record: a `# ref <topic>: <title>` line, then the body.
    pub fn to_record(&self) -> String {
        format!("# ref {}: {}\n{}", self.topic, self.title, self.body.trim_end())
    }

    /// Whether the title or body has each of `words` (lower-case).
    pub fn mentions_all(&self, words: &[String]) -> bool {
        let text = format!("{}\n{}", self.title, self.body).to_lowercase();
        words.iter().all(|w| text.contains(w.as_str()))
    }

    fn score(&self, words: &[String]) -> usize {
        let (title, body) = (self.title.to_lowercase(), self.body.to_lowercase());
        words.iter().map(|w| 3 * title.matches(w.as_str()).count() + body.matches(w.as_str()).count()).sum()
    }
}

/// Where an engine install keeps its own API reference and examples.
#[derive(Clone, Copy, Debug)]
pub struct Docs {
    /// The file name the engine's shared library starts with, whose
    /// directory is the install: `libidalib`.
    pub library: &'static str,
    /// The variable naming the install when the library is not found:
    /// `IDADIR`.
    pub env: &'static str,
    /// How the API's files name what is in them.
    pub naming: Naming,
    pub places: &'static [Place],
}

/// One place in an install.
#[derive(Clone, Copy, Debug)]
pub struct Place {
    pub role: Role,
    /// Relative to the install, first match wins: an install is laid out
    /// differently per platform (BN's macOS bundle keeps its Python under
    /// `../Resources/`).
    pub paths: &'static [&'static str],
    pub holds: &'static str,
    /// Shell commands to paste, `{}` standing for the place's absolute path.
    pub tries: &'static [&'static str],
}

/// What a [`Place`] holds, for `ref <words>` to search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The API's `.py` files.
    Api,
    /// Official example scripts.
    Examples,
}

/// [`Docs`] resolved against an install on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub install: PathBuf,
    pub places: Vec<FoundPlace>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundPlace {
    pub role: Role,
    pub path: PathBuf,
    pub holds: &'static str,
    pub tries: Vec<String>,
}

impl Docs {
    /// The install this process loaded the engine from, else the one
    /// `var(env)` names; `Err` says why there is none.
    ///
    /// # Errors
    ///
    /// Neither gives a directory that holds the places.
    pub fn find(&self, var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<Found, String> {
        let install = match loaded_library(self.library) {
            Some(lib) => lib.parent().map(Path::to_path_buf).ok_or_else(|| format!("{} has no directory", lib.display()))?,
            None => var(self.env)
                .map(PathBuf::from)
                .ok_or_else(|| format!("this process has no {}* loaded and {} is not set", self.library, self.env))?,
        };
        Ok(self.at(&install))
    }

    /// The places under `install`, those that exist.
    pub fn at(&self, install: &Path) -> Found {
        let places = self
            .places
            .iter()
            .filter_map(|p| p.paths.iter().map(|rel| lexical_join(install, rel)).find(|path| path.exists()).map(|path| (p, path)))
            .map(|(p, path)| {
                let shown = crate::cli::shell_word(&path);
                FoundPlace { role: p.role, tries: p.tries.iter().map(|t| t.replace("{}", &shown)).collect(), path, holds: p.holds }
            })
            .collect();
        Found { install: install.to_path_buf(), places }
    }
}

impl Found {
    /// The install's API and examples, indexed for `ref <words>`; `None`
    /// when the install has no API place.
    pub fn index(&self, naming: Naming) -> Option<ApiIndex> {
        let at = |role: Role| self.places.iter().find(|p| p.role == role).map(|p| p.path.as_path());
        Some(ApiIndex::load(at(Role::Api)?, naming, at(Role::Examples)))
    }
}

/// `base/rel` with `..` folded away, so a path shown to the agent reads
/// `App/Contents/Resources/python`, not `App/Contents/MacOS/../Resources/python`.
fn lexical_join(base: &Path, rel: &str) -> PathBuf {
    use std::path::Component;
    let mut out = base.to_path_buf();
    for part in Path::new(rel).components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => out.push(part),
        }
    }
    out
}

/// The shared library loaded into this process whose file name starts
/// with `stem`.
#[cfg(target_os = "linux")]
fn loaded_library(stem: &str) -> Option<PathBuf> {
    use std::ffi::{CStr, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" fn each(info: *mut libc::dl_phdr_info, _size: usize, names: *mut c_void) -> c_int {
        // SAFETY: dl_iterate_phdr hands a valid info for the call, and
        // `names` is the Vec passed below, alive and not otherwise borrowed.
        unsafe {
            let names = &mut *names.cast::<Vec<PathBuf>>();
            let name = (*info).dlpi_name;
            if !name.is_null() {
                names.push(PathBuf::from(std::ffi::OsStr::from_bytes(CStr::from_ptr(name).to_bytes())));
            }
        }
        0
    }
    let mut names: Vec<PathBuf> = Vec::new();
    // SAFETY: the callback only reads the info it is given and writes to
    // `names`, which outlives the call.
    unsafe { libc::dl_iterate_phdr(Some(each), (&raw mut names).cast()) };
    names.into_iter().find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(stem)))
}

#[cfg(target_os = "macos")]
fn loaded_library(stem: &str) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    // <mach-o/dyld.h>; libc's own declarations are deprecated in favour of
    // the mach2 crate, which is a dependency for two symbols.
    unsafe extern "C" {
        fn _dyld_image_count() -> u32;
        fn _dyld_get_image_name(image_index: u32) -> *const std::ffi::c_char;
    }
    // SAFETY: dyld's image list is safe to read from any thread; a name is
    // a NUL-terminated string owned by dyld, valid while the image stays
    // loaded, and it is copied out before the next call.
    let count = unsafe { _dyld_image_count() };
    (0..count)
        .filter_map(|i| {
            // SAFETY: `i` is below the count dyld just reported.
            let name = unsafe { _dyld_get_image_name(i) };
            // SAFETY: non-null names point at NUL-terminated strings (above).
            (!name.is_null()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(unsafe { CStr::from_ptr(name) }.to_bytes())))
        })
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(stem)))
}

#[cfg(windows)]
fn loaded_library(stem: &str) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
    use windows_sys::Win32::System::ProcessStatus::EnumProcessModules;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut modules: Vec<HMODULE> = vec![std::ptr::null_mut(); 1024];
    let mut needed = 0u32;
    let room = u32::try_from(modules.len() * std::mem::size_of::<HMODULE>()).ok()?;
    // SAFETY: `modules` has room for `room` bytes and `needed` is valid for
    // a write; the pseudo handle of this process needs no closing.
    if unsafe { EnumProcessModules(GetCurrentProcess(), modules.as_mut_ptr(), room, &raw mut needed) } == 0 {
        return None;
    }
    modules.truncate(usize::try_from(needed).ok()? / std::mem::size_of::<HMODULE>());
    // `libidalib` is `idalib.dll` here.
    let stem = stem.strip_prefix("lib").unwrap_or(stem).to_ascii_lowercase();
    modules.into_iter().find_map(|module| {
        let mut name = vec![0u16; 1024];
        let len = u32::try_from(name.len()).ok()?;
        // SAFETY: `name` has room for `len` UTF-16 units; `module` is loaded.
        let n = usize::try_from(unsafe { GetModuleFileNameW(module, name.as_mut_ptr(), len) }).ok()?;
        let path = PathBuf::from(std::ffi::OsString::from_wide(name.get(..n)?));
        let file = path.file_name()?.to_str()?.to_ascii_lowercase();
        file.starts_with(&stem).then_some(path)
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn loaded_library(_stem: &str) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const XREFS: &str = "# xrefs: references to and from addresses\n## code references to a function\nWho calls it.\n    xrefs('main')\n\n## data references\nWho reads it.\n    drefs(0x1000)\n";
    const STRINGS: &str = "# strings: defined strings\n## find a string\nBy text.\n    strings()\n";

    fn corpus() -> Corpus {
        Corpus::parse(&[XREFS, "no header", STRINGS])
    }

    #[test]
    fn a_corpus_is_topics_of_entries() {
        let c = corpus();
        let names: Vec<_> = c.topics().iter().map(|t| (t.name, t.about, t.entries.len())).collect();
        assert_eq!(names, [("xrefs", "references to and from addresses", 2), ("strings", "defined strings", 1)]);
        assert_eq!(c.topics()[0].entries[1].body, "Who reads it.\n    drefs(0x1000)\n");
    }

    #[test]
    fn a_topic_name_shows_the_topic_and_words_rank_the_rest() {
        let c = corpus();
        assert!(matches!(c.lookup(&["XREFS".into()]), Answer::Topic(t) if t.name == "xrefs"));
        let Answer::Hits(hits) = c.lookup(&["string".into()]) else { panic!() };
        assert_eq!(hits[0].title, "find a string", "title words count thrice");
        let Answer::Hits(none) = c.lookup(&["zzzz".into()]) else { panic!() };
        assert!(none.is_empty());
        assert!(matches!(c.lookup(&[]), Answer::Topics(t) if t.len() == 2));
    }

    static DOCS: Docs = Docs {
        library: "libnothing",
        env: "ENGINEDIR",
        naming: Naming::Stem,
        places: &[
            Place { role: Role::Api, paths: &["nowhere", "python"], holds: "API", tries: &["grep -n x {}/a.py"] },
            Place { role: Role::Examples, paths: &["missing"], holds: "gone", tries: &[] },
        ],
    };

    #[test]
    fn docs_come_from_the_install_with_paths_filled_in() {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("my engine");
        std::fs::create_dir_all(install.join("python")).unwrap();
        let var = |k: &str| (k == "ENGINEDIR").then(|| install.clone().into_os_string());
        let found = DOCS.find(var).unwrap();
        assert_eq!(found.install, install);
        assert_eq!(found.places.len(), 1, "a place the install lacks is left out");
        assert_eq!(found.places[0].tries, [format!("grep -n x '{}'/a.py", install.join("python").display())]);
        assert_eq!(found.places[0].holds, "API");
        std::fs::write(install.join("python/eng.py"), "def go(x):\n    \"\"\"Go.\"\"\"\n").unwrap();
        let index = found.index(DOCS.naming).unwrap();
        assert_eq!(index.search(&["go".into()], 5).defs[0].qualname, "eng.go");
        assert_eq!(lexical_join(Path::new("/a/MacOS"), "../Resources/python"), Path::new("/a/Resources/python"));
        let why = DOCS.find(|_| None).unwrap_err();
        assert!(why.contains("ENGINEDIR is not set"), "{why}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_loaded_library_is_found_by_its_name() {
        let stem = if cfg!(target_os = "macos") {
            "libSystem"
        } else if cfg!(windows) {
            "libkernel32"
        } else {
            "libc.so"
        };
        let libc_so = loaded_library(stem).unwrap();
        assert!(libc_so.is_absolute(), "{}", libc_so.display());
    }

    #[test]
    fn a_record_starts_with_its_header() {
        let e = corpus().topics()[1].entries[0];
        assert_eq!(e.to_record(), "# ref strings: find a string\nBy text.\n    strings()");
    }
}
