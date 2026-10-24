//! The daemon's sessions (CONVENTIONS §3): one worker process per canonical
//! target path, so a crash or a hung script takes down that session only.
//!
//! Each session has a private working directory under the cache,
//! `<tool>/sessions/<daemon pid>-<n>`, beside it the worker's log. A target
//! database is copied in and opened as the copy, so nothing beside the
//! target changes until `save`; closing a session or losing its worker
//! deletes the directory. Directories of daemons that are gone are removed
//! when the next one starts.

use crate::proto::{Down, Launch, Reply, Request, State, Up};
use crate::tool::Tool;
use axkit::daemon::Io;
use axkit::exit::{ErrorKind, Failure};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How often a waiting command says it still waits (AX §7), when the
/// worker says nothing itself.
const PROGRESS_EVERY: Duration = Duration::from_secs(5);
/// The first such line comes sooner; a request that answers before it
/// says nothing (AX §7).
const PROGRESS_FIRST: Duration = Duration::from_secs(3);
/// How long an interrupted request gets to answer before its worker is killed.
const CANCEL_GRACE: Duration = Duration::from_secs(10);
/// How long a closing worker gets to exit on its own.
const CLOSE_GRACE: Duration = Duration::from_secs(10);
/// How long `status` waits for a worker it asks for fresh state.
const STATUS_WAIT: Duration = Duration::from_secs(10);

pub struct Sessions {
    tool: &'static Tool,
    root: PathBuf,
    map: Mutex<HashMap<PathBuf, Arc<Session>>>,
    next: AtomicU64,
}

/// One open target.
pub struct Session {
    pub target: PathBuf,
    workdir: PathBuf,
    /// The worker's stderr: the engine's messages, and its last words.
    log: PathBuf,
    worker: Mutex<Option<Worker>>,
    info: Mutex<Info>,
}

/// What `status` and `list` show without waiting for a busy worker.
#[derive(Clone, Debug)]
pub struct Info {
    /// `None` until the worker has opened the target.
    pub state: Option<State>,
    pub used: Instant,
    /// The request running now and since when.
    pub busy: Option<(&'static str, Instant)>,
    /// Where the session saved last.
    pub saved_to: Option<PathBuf>,
}

/// A session, and whether this command started its worker.
pub struct Opened {
    pub session: Arc<Session>,
    pub fresh: bool,
}

impl Sessions {
    /// Sessions under `<cache>/<tool>/sessions`.
    pub fn new(tool: &'static Tool) -> Sessions {
        Sessions::with_root(tool, axkit::output::cache_dir(tool.name).join("sessions"))
    }

    pub fn with_root(tool: &'static Tool, root: PathBuf) -> Sessions {
        remove_stale(&root);
        Sessions { tool, root, map: Mutex::new(HashMap::new()), next: AtomicU64::new(0) }
    }

    fn map(&self) -> MutexGuard<'_, HashMap<PathBuf, Arc<Session>>> {
        self.map.lock().expect("session table mutex poisoned")
    }

    pub fn is_empty(&self) -> bool {
        self.map().is_empty()
    }

    pub fn get(&self, target: &Path) -> Option<Arc<Session>> {
        self.map().get(target).cloned()
    }

    /// Every session, by target.
    pub fn all(&self) -> Vec<Arc<Session>> {
        let mut all: Vec<Arc<Session>> = self.map().values().cloned().collect();
        all.sort_by(|a, b| a.target.cmp(&b.target));
        all
    }

    /// The session of `target` (a canonical path), its worker started if
    /// there is none; a new worker starts auto-analysis.
    ///
    /// # Errors
    ///
    /// The worker could not start, open the target, or was lost.
    ///
    /// # Panics
    ///
    /// Another command panicked while holding this session's lock.
    pub fn open(&self, target: &Path, io: &mut Io<'_>, timeout: Option<Duration>) -> Result<Opened, Failure> {
        let session = Arc::clone(self.map().entry(target.to_path_buf()).or_insert_with(|| Arc::new(self.create(target))));
        let mut worker = session.worker.lock().expect("worker mutex poisoned");
        let fresh = worker.is_none();
        if fresh {
            match self.start(&session, io, timeout) {
                Ok(w) => *worker = Some(w),
                Err(failure) => {
                    drop(worker);
                    self.discard(&session);
                    return Err(failure);
                }
            }
        }
        drop(worker);
        Ok(Opened { session, fresh })
    }

    fn create(&self, target: &Path) -> Session {
        let name = format!("{}-{}", std::process::id(), self.next.fetch_add(1, Ordering::Relaxed));
        Session {
            target: target.to_path_buf(),
            workdir: self.root.join(&name),
            log: self.root.join(format!("{name}.log")),
            worker: Mutex::new(None),
            info: Mutex::new(Info { state: None, used: Instant::now(), busy: None, saved_to: None }),
        }
    }

    /// Start the worker and wait for it to open the target.
    fn start(&self, session: &Session, io: &mut Io<'_>, timeout: Option<Duration>) -> Result<Worker, Failure> {
        let restart = format!("rerun; if it repeats, `{} daemon stop --force` starts a fresh daemon", self.tool.name);
        let internal = |what: String, e: io::Error| Failure::new(ErrorKind::Internal, format!("{what}: {e}")).with_hint(restart.clone());
        let _ = fs::remove_dir_all(&session.workdir);
        create_private_dir(&session.workdir).map_err(|e| internal(format!("cannot create {}", session.workdir.display()), e))?;
        let input = if self.tool.databases.is_database(&session.target) {
            let copy = session.workdir.join(session.target.file_name().unwrap_or_else(|| "target".as_ref()));
            fs::copy(&session.target, &copy).map_err(|e| {
                Failure::new(ErrorKind::TargetUnavailable, format!("cannot copy {} to open it: {e}", session.target.display()))
                    .with_hint(format!("the session works on a copy in {}; check that it is readable there", session.workdir.display()))
            })?;
            copy
        } else {
            session.target.clone()
        };
        let launch = Launch { target: session.target.clone(), input, workdir: session.workdir.clone() };
        let mut worker =
            Worker::spawn(&launch, &session.log).map_err(|e| internal(format!("cannot start the {} worker", self.tool.engine), e))?;
        session.info().busy = Some(("open", Instant::now()));
        let opening = format!("opening {} in {}", session.target.display(), self.tool.engine);
        let opened = worker.wait(&opening, io, timeout);
        let mut info = session.info();
        info.busy = None;
        match opened {
            Ok(Up::Reply(reply)) if matches!(*reply, Reply::Opened(_)) => {
                info.state = Some(reply.state().clone());
                info.used = Instant::now();
                Ok(worker)
            }
            Ok(Up::Failed(f)) => {
                worker.kill();
                Err(f.into_failure(|| log_hint(session)))
            }
            Ok(other) => {
                worker.kill();
                Err(Failure::new(ErrorKind::Internal, format!("the {} worker answered {other:?} to opening", self.tool.engine))
                    .with_hint(log_hint(session)))
            }
            Err(lost) => {
                worker.kill();
                Err(self.lost(session, &lost))
            }
        }
    }

    /// One request to the session's worker; commands on one target take
    /// turns. A worker that dies, times out or does not stop when
    /// interrupted takes the session with it.
    ///
    /// # Errors
    ///
    /// The engine failed the request (the session lives on), or the worker
    /// was lost.
    ///
    /// # Panics
    ///
    /// Another command panicked while holding this session's lock.
    pub fn call(
        &self,
        session: &Arc<Session>,
        request: Request,
        what: &'static str,
        io: &mut Io<'_>,
        timeout: Option<Duration>,
    ) -> Result<Reply, Failure> {
        let mut worker = session.worker.lock().expect("worker mutex poisoned");
        self.call_locked(session, &mut worker, request, what, io, timeout)
    }

    /// The session's state as its worker reports it now, unless it is busy
    /// with another command; then the last one known.
    ///
    /// # Errors
    ///
    /// The worker was lost meanwhile.
    ///
    /// # Panics
    ///
    /// Another command panicked while holding this session's lock.
    pub fn refresh(&self, session: &Arc<Session>, io: &mut Io<'_>) -> Result<Info, Failure> {
        if let Ok(mut worker) = session.worker.try_lock() {
            self.call_locked(session, &mut worker, Request::Status, "status", io, Some(STATUS_WAIT))?;
        }
        Ok(session.info().clone())
    }

    fn call_locked(
        &self,
        session: &Arc<Session>,
        guard: &mut MutexGuard<'_, Option<Worker>>,
        request: Request,
        what: &'static str,
        io: &mut Io<'_>,
        timeout: Option<Duration>,
    ) -> Result<Reply, Failure> {
        let Some(worker) = guard.as_mut() else {
            return Err(Failure::new(ErrorKind::SessionLost, format!("the session of {} is gone", session.target.display()))
                .with_hint(format!("{} open {}", self.tool.name, crate::cli::shell_word(&session.target))));
        };
        session.info().busy = Some((what, Instant::now()));
        let answer = worker.call(&Down::Request(request), what, io, timeout);
        let mut info = session.info();
        info.busy = None;
        info.used = Instant::now();
        match answer {
            Ok(Up::Reply(reply)) => {
                info.state = Some(reply.state().clone());
                Ok(*reply)
            }
            Ok(Up::Failed(f)) => Err(f.into_failure(|| log_hint(session))),
            Ok(Up::Progress(_)) => unreachable!("Worker::wait relays progress and returns only answers"),
            Err(lost) => {
                drop(info);
                worker.kill();
                **guard = None;
                self.discard(session);
                Err(self.lost(session, &lost))
            }
        }
    }

    pub fn note_saved(&self, session: &Session, to: &Path) {
        session.info().saved_to = Some(to.to_path_buf());
    }

    /// Close without saving: what the session was, or `None` without one.
    pub fn close(&self, target: &Path) -> Option<Info> {
        let session = self.map().remove(target)?;
        session.stop();
        Some(session.info().clone())
    }

    /// Every session, closed without saving.
    pub fn shutdown(&self) {
        let all: Vec<Arc<Session>> = self.map().drain().map(|(_, s)| s).collect();
        all.iter().for_each(|s| s.stop());
    }

    /// Forget a session whose worker is gone; its log stays for the hint.
    fn discard(&self, session: &Arc<Session>) {
        let mut map = self.map();
        if map.get(&session.target).is_some_and(|s| Arc::ptr_eq(s, session)) {
            map.remove(&session.target);
        }
        drop(map);
        let _ = fs::remove_dir_all(&session.workdir);
    }

    fn lost(&self, session: &Session, lost: &Lost) -> Failure {
        let (tool, engine, target) = (self.tool.name, self.tool.engine, session.target.display());
        let log = log_hint(session);
        let reopen = format!("{tool} open {}", crate::cli::shell_word(&session.target));
        match lost {
            Lost::Exited(status) => Failure::new(
                ErrorKind::SessionLost,
                format!("the {engine} worker for {target} exited ({status}); unsaved changes are lost"),
            )
            .with_hint(format!("{reopen} starts a new session; {log}")),
            Lost::TimedOut(t) => Failure::new(
                ErrorKind::Timeout,
                format!(
                    "no answer from the {engine} worker for {target} within {} s; it was killed and unsaved changes are lost",
                    t.as_secs()
                ),
            )
            .with_hint(format!("rerun with a larger --timeout (0: no limit); {log}")),
            Lost::Cancelled => Failure::new(
                ErrorKind::SessionLost,
                format!(
                    "the interrupted request on {target} did not stop within {} s; the worker was killed and unsaved changes are lost",
                    CANCEL_GRACE.as_secs()
                ),
            )
            .with_hint(format!("{reopen} starts a new session; {log}")),
        }
    }
}

/// Where to look next when the engine failed a request: its own words are
/// in the worker's log.
fn log_hint(session: &Session) -> String {
    format!("the worker's log is {}", session.log.display())
}

impl Session {
    /// # Panics
    ///
    /// Another command panicked while holding this session's lock.
    pub fn info(&self) -> MutexGuard<'_, Info> {
        self.info.lock().expect("session info mutex poisoned")
    }

    /// End the worker without saving and remove the working directory.
    fn stop(&self) {
        if let Some(worker) = self.worker.lock().expect("worker mutex poisoned").take() {
            worker.close();
        }
        let _ = fs::remove_dir_all(&self.workdir);
        let _ = fs::remove_file(&self.log);
    }
}

/// Why a worker stopped answering.
#[derive(Debug)]
enum Lost {
    Exited(String),
    TimedOut(Duration),
    /// Interrupted, it did not answer within the grace period.
    Cancelled,
}

/// The daemon's end of one worker process.
struct Worker {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<Up>,
}

impl Worker {
    /// `<exe> __worker <launch>`, its stderr into `log`.
    fn spawn(launch: &Launch, log: &Path) -> io::Result<Worker> {
        let launch = serde_json::to_string(launch).map_err(io::Error::other)?;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg(crate::worker::ARG).arg(launch).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(File::create(log)?);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            /// The daemon has no console; without this Windows would open a
            /// console window for each worker.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| io::Error::other("worker stdout not piped"))?;
        let (tx, messages) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                // A line that does not parse is the worker's bug; it shows
                // as the worker going silent, then lost.
                let Ok(up) = serde_json::from_str::<Up>(&line) else { continue };
                if tx.send(up).is_err() {
                    break;
                }
            }
        });
        Ok(Worker { stdin: child.stdin.take(), child, messages })
    }

    fn call(&mut self, down: &Down, what: &str, io: &mut Io<'_>, timeout: Option<Duration>) -> Result<Up, Lost> {
        if !self.send(down) {
            return Err(self.exited());
        }
        self.wait(what, io, timeout)
    }

    fn send(&mut self, down: &Down) -> bool {
        let line = serde_json::to_string(down).expect("protocol messages always serialize");
        self.stdin.as_mut().is_some_and(|s| writeln!(s, "{line}").and_then(|()| s.flush()).is_ok())
    }

    /// Relay progress until the answer. A timeout kills the worker: the
    /// session is lost either way. A client that went away interrupts the
    /// request; the worker lives on if it answers within the grace period,
    /// and its answer, which nobody reads, is returned as usual.
    fn wait(&mut self, what: &str, io: &mut Io<'_>, timeout: Option<Duration>) -> Result<Up, Lost> {
        let start = Instant::now();
        let mut said = Instant::now();
        let mut every = PROGRESS_FIRST;
        let mut interrupted: Option<Instant> = None;
        loop {
            match self.messages.recv_timeout(Duration::from_millis(100)) {
                Ok(Up::Progress(line)) => {
                    io.progress(&line);
                    said = Instant::now();
                    every = PROGRESS_EVERY;
                }
                Ok(answer) => return Ok(answer),
                Err(RecvTimeoutError::Disconnected) => return Err(self.exited()),
                Err(RecvTimeoutError::Timeout) => {}
            }
            match interrupted {
                Some(at) if at.elapsed() >= CANCEL_GRACE => {
                    self.kill();
                    return Err(Lost::Cancelled);
                }
                Some(_) => {}
                None if io.cancel().is_cancelled() => {
                    self.send(&Down::Cancel);
                    interrupted = Some(Instant::now());
                }
                None => {
                    if let Some(t) = timeout.filter(|t| start.elapsed() >= *t) {
                        self.kill();
                        return Err(Lost::TimedOut(t));
                    }
                    if said.elapsed() >= every {
                        io.progress(&format!("# progress: {what}, {}s elapsed", start.elapsed().as_secs()));
                        said = Instant::now();
                        every = PROGRESS_EVERY;
                    }
                }
            }
        }
    }

    fn exited(&mut self) -> Lost {
        let _ = self.child.kill();
        Lost::Exited(self.child.wait().map_or_else(|e| format!("unknown status: {e}"), |s| s.to_string()))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Close its stdin, which ends it unsaved, and kill it if it does not
    /// exit in time.
    fn close(mut self) {
        self.stdin = None;
        let start = Instant::now();
        while start.elapsed() < CLOSE_GRACE {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.kill();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill();
        }
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Working directories and logs left by daemons that are gone: named
/// `<pid>-<n>`, they are stale once that process is.
fn remove_stale(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let pid = name.split('-').next().and_then(|p| p.parse::<u32>().ok());
        if pid.is_some_and(|pid| pid != std::process::id() && !is_alive(pid)) {
            let path = entry.path();
            let _ = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        }
    }
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

#[cfg(windows)]
fn is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    // SAFETY: OpenProcess takes no pointers; a handle it returns is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        // Someone else's process is alive; a pid that names none is not.
        return io::Error::last_os_error().raw_os_error() == i32::try_from(ERROR_ACCESS_DENIED).ok();
    }
    let mut code = 0u32;
    // SAFETY: `process` is the handle just opened, `code` valid for a write.
    let alive = unsafe { GetExitCodeProcess(process, &raw mut code) != 0 } && i32::try_from(code).ok() == Some(STILL_ACTIVE);
    // SAFETY: closes the handle opened above, once.
    unsafe { CloseHandle(process) };
    alive
}

#[cfg(not(any(unix, windows)))]
fn is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leftovers_of_dead_daemons_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let dead = root.path().join("999999999-0");
        let mine = root.path().join(format!("{}-0", std::process::id()));
        fs::create_dir(&dead).unwrap();
        fs::write(root.path().join("999999999-0.log"), "").unwrap();
        fs::create_dir(&mine).unwrap();
        let _sessions = Sessions::with_root(&crate::tool::tests::TOOL, root.path().to_owned());
        assert!(!dead.exists() && !root.path().join("999999999-0.log").exists());
        assert!(mine.exists());
    }
}
