//! Where `save` writes, and how (CONVENTIONS §3): beside a binary under the
//! engine's database name, over a database that was opened, or where `--to`
//! says; never over a file this session did not write without `--force`;
//! through a fresh file beside the destination, renamed over it once whole.

use crate::cli::shell_word;
use axkit::exit::{ErrorKind, Failure};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// An engine's database files, by extension, the one a binary is saved as
/// first: `["i64", "idb"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Databases {
    pub extensions: &'static [&'static str],
}

/// One save under way: the engine writes `partial`, then [`Save::commit`]
/// moves it to `dest`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Save {
    pub dest: PathBuf,
    pub partial: PathBuf,
}

impl Databases {
    pub fn is_database(&self, path: &Path) -> bool {
        path.extension().and_then(OsStr::to_str).is_some_and(|e| self.extensions.iter().any(|x| x.eq_ignore_ascii_case(e)))
    }

    /// Where a binary's database goes by default: `fw.bin` → `fw.bin.i64`.
    pub fn beside(&self, binary: &Path) -> PathBuf {
        let mut name = binary.as_os_str().to_os_string();
        name.push(".");
        name.push(self.extensions.first().copied().unwrap_or("db"));
        PathBuf::from(name)
    }

    /// Where `save` of `target`'s session writes: `to`, else the database
    /// that was opened, else [`Databases::beside`]. `saved_to` is where the
    /// session saved last; that file and the opened database are its own.
    ///
    /// # Errors
    ///
    /// The destination exists, is not the session's own, and `force` is off.
    pub fn plan(&self, tool: &str, target: &Path, to: Option<&Path>, saved_to: Option<&Path>, force: bool) -> Result<Save, Failure> {
        let dest = match to {
            Some(to) => to.to_path_buf(),
            None if self.is_database(target) => target.to_path_buf(),
            None => self.beside(target),
        };
        let ours = saved_to == Some(dest.as_path()) || (self.is_database(target) && dest == target);
        if dest.exists() && !ours && !force {
            return Err(Failure::new(ErrorKind::Usage, format!("{} exists and this session did not write it", dest.display()))
                .with_hint(format!("{tool} save {} --force   (or --to another path)", shell_word(target))));
        }
        let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let partial = dest.with_file_name(format!(".{name}.{tool}-{}.partial", std::process::id()));
        Ok(Save { dest, partial })
    }
}

impl Save {
    /// Move the written database into place.
    ///
    /// # Errors
    ///
    /// The rename failed; the partial file is removed.
    pub fn commit(&self) -> io::Result<()> {
        std::fs::rename(&self.partial, &self.dest).inspect_err(|_| self.abandon())
    }

    /// Remove what the engine may have written.
    pub fn abandon(&self) {
        let _ = std::fs::remove_file(&self.partial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDA: Databases = Databases { extensions: &["i64", "idb"] };

    #[test]
    fn databases_are_told_apart_by_extension() {
        assert!(IDA.is_database(Path::new("/a/fw.i64")) && IDA.is_database(Path::new("/a/OLD.IDB")));
        assert!(!IDA.is_database(Path::new("/a/fw.bin")) && !IDA.is_database(Path::new("/a/true")));
        assert_eq!(IDA.beside(Path::new("/w/fw.bin")), Path::new("/w/fw.bin.i64"));
    }

    #[test]
    fn a_save_goes_beside_a_binary_or_over_the_database_opened() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fw.bin");
        let save = IDA.plan("idax", &bin, None, None, false).unwrap();
        assert_eq!(save.dest, dir.path().join("fw.bin.i64"));
        assert_eq!(save.partial.parent(), Some(dir.path()), "beside it, so the rename stays on one filesystem");
        let db = dir.path().join("fw.i64");
        std::fs::write(&db, "").unwrap();
        assert_eq!(IDA.plan("idax", &db, None, None, false).unwrap().dest, db, "its own database");
    }

    #[test]
    fn a_file_the_session_did_not_write_needs_force() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fw.bin");
        let beside = IDA.beside(&bin);
        std::fs::write(&beside, "someone else's").unwrap();
        let refused = IDA.plan("idax", &bin, None, None, false).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Usage);
        assert!(refused.hint.unwrap().starts_with(&format!("idax save {} --force", bin.display())));
        assert!(IDA.plan("idax", &bin, None, Some(&beside), false).is_ok(), "written by this session before");
        assert!(IDA.plan("idax", &bin, None, None, true).is_ok());
    }

    #[test]
    fn a_commit_moves_the_partial_file_into_place() {
        let dir = tempfile::tempdir().unwrap();
        let save = IDA.plan("idax", &dir.path().join("fw.bin"), None, None, false).unwrap();
        std::fs::write(&save.partial, "db").unwrap();
        save.commit().unwrap();
        assert_eq!(std::fs::read_to_string(&save.dest).unwrap(), "db");
        assert!(!save.partial.exists());
    }
}
