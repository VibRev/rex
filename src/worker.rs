//! The worker process: one session's engine, one request at a time
//! (CONVENTIONS §3). The engine is a [`Backend`]; the loop around it, the
//! pipes and cancellation are here.
//!
//! Requests come in on the stdin the worker was started with and answers go
//! out on its original stdout. Both are moved off fds 0 and 1 before the
//! engine starts: engines print to stdout, and a script that reads stdin
//! must not eat requests. fd 0 becomes `/dev/null`, fd 1 a copy of stderr.

use crate::exec::ScriptRun;
use crate::proto::{Down, ExecRequest, Launch, Reply, Request, State, Up, WorkerFailure};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

/// The hidden `argv[1]` that makes the tool's executable a worker; its
/// [`Launch`] follows as JSON.
pub const ARG: &str = "__worker";

/// One engine, driven by [`serve`]. Every method runs on the worker's main
/// thread, one request at a time.
pub trait Backend: Sized {
    /// Load `launch.input` and start auto-analysis.
    ///
    /// # Errors
    ///
    /// The engine did not start, is not the version the tool is built for,
    /// or cannot open the input.
    fn open(launch: &Launch, channel: &Channel) -> Result<Self, WorkerFailure>;

    fn state(&self) -> State;

    /// Wait until analysis settles, saying how far it is through
    /// [`Channel::progress`]; stop waiting once [`Channel::is_cancelled`].
    fn analyze(&mut self, channel: &Channel);

    /// Run one script through the engine's prelude.
    ///
    /// # Errors
    ///
    /// The prelude itself failed; an exception in the script is a
    /// [`ScriptRun`] with an error, not this.
    fn exec(&mut self, exec: &ExecRequest, channel: &Channel) -> Result<ScriptRun, WorkerFailure>;

    /// Write a database to `to`, a fresh path; the session goes on as it was.
    ///
    /// # Errors
    ///
    /// The engine could not write it.
    fn save(&mut self, to: &Path) -> Result<(), WorkerFailure>;
}

/// The worker's `--launch` when this process was started as one.
///
/// # Errors
///
/// It was started as a worker, but the launch is not valid JSON.
pub fn launch() -> Option<Result<Launch, String>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().is_none_or(|a| a != ARG) {
        return None;
    }
    let launch = args.next().ok_or_else(|| "no launch argument".to_owned());
    Some(launch.and_then(|l| serde_json::from_str(&l.to_string_lossy()).map_err(|e| format!("bad launch argument: {e}"))))
}

/// Serve requests until stdin closes; the session is dropped unsaved.
pub fn serve<B: Backend>(launch: &Launch) -> ExitCode {
    let channel = match Channel::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("worker: cannot set up the request pipes: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut backend = match B::open(launch, &channel) {
        Ok(b) => b,
        Err(f) => {
            channel.send(&Up::Failed(f));
            return ExitCode::FAILURE;
        }
    };
    channel.send(&Up::Reply(Box::new(Reply::Opened(backend.state()))));
    while let Ok(request) = channel.requests.recv() {
        channel.cancelled.store(false, Ordering::SeqCst);
        let answer = match request {
            Request::Analyze => {
                backend.analyze(&channel);
                Ok(Reply::Analyzed(backend.state()))
            }
            Request::Exec(exec) => {
                let before = backend.state();
                backend.exec(&exec, &channel).map(|run| Reply::Ran { run, before, after: backend.state() })
            }
            Request::Save { to } => backend.save(&to).map(|()| Reply::Saved(backend.state())),
            Request::Status => Ok(Reply::Status(backend.state())),
        };
        channel.send(&match answer {
            Ok(reply) => Up::Reply(Box::new(reply)),
            Err(f) => Up::Failed(f),
        });
    }
    ExitCode::SUCCESS
}

/// What a cancel calls while [`Channel::interruptible`] runs.
type Interrupt = Arc<Mutex<Option<fn()>>>;

/// The worker's end of the pipes to the daemon.
pub struct Channel {
    requests: Receiver<Request>,
    replies: Mutex<File>,
    cancelled: Arc<AtomicBool>,
    /// Called on the reader thread when a cancel arrives, while
    /// [`Channel::interruptible`] runs.
    interrupt: Interrupt,
}

impl Channel {
    fn open() -> io::Result<Channel> {
        let (requests_in, replies) = take_pipes()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let interrupt: Interrupt = Arc::new(Mutex::new(None));
        let (tx, requests) = mpsc::channel();
        let (flag, hook) = (Arc::clone(&cancelled), Arc::clone(&interrupt));
        // Its own thread, so a cancel reaches a request in progress. The
        // channel closes when stdin does.
        std::thread::spawn(move || {
            for line in requests_in.lines().map_while(Result::ok) {
                match serde_json::from_str::<Down>(&line) {
                    Ok(Down::Request(request)) => {
                        if tx.send(request).is_err() {
                            break;
                        }
                    }
                    Ok(Down::Cancel) => {
                        flag.store(true, Ordering::SeqCst);
                        if let Some(interrupt) = *hook.lock().expect("interrupt mutex poisoned") {
                            interrupt();
                        }
                    }
                    Err(e) => eprintln!("worker: unreadable request ({e}): {line}"),
                }
            }
        });
        Ok(Channel { requests, replies: Mutex::new(replies), cancelled, interrupt })
    }

    /// A `# progress: ...` line for the client.
    pub fn progress(&self, line: &str) {
        self.send(&Up::Progress(line.to_owned()));
    }

    /// The client of the request in progress has gone.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Run `work` with `interrupt` called, on another thread, when a cancel
    /// arrives meanwhile: for an engine that is interrupted from outside
    /// (a signal) rather than by polling [`Channel::is_cancelled`]. Once
    /// this returns, `interrupt` is not called any more.
    ///
    /// # Panics
    ///
    /// The reader thread panicked while calling `interrupt`.
    pub fn interruptible<T>(&self, interrupt: fn(), work: impl FnOnce() -> T) -> T {
        *self.interrupt.lock().expect("interrupt mutex poisoned") = Some(interrupt);
        let out = work();
        *self.interrupt.lock().expect("interrupt mutex poisoned") = None;
        out
    }

    fn send(&self, up: &Up) {
        let line = serde_json::to_string(up).expect("protocol messages always serialize");
        let mut replies = self.replies.lock().expect("reply pipe mutex poisoned");
        // A daemon that went away finds out from the worker's exit.
        let _ = writeln!(replies, "{line}").and_then(|()| replies.flush());
    }
}

/// The request and reply pipes on fresh close-on-exec fds; stdin becomes
/// `/dev/null` and stdout a copy of stderr.
#[cfg(unix)]
fn take_pipes() -> io::Result<(BufReader<File>, File)> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let dup = |fd: i32| {
        // SAFETY: duplicates an fd this process was started with; the
        // result is checked before use.
        let new = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if new < 0 { Err(io::Error::last_os_error()) } else { Ok(new) }
    };
    let (requests, replies) = (dup(0)?, dup(1)?);
    let null = File::open("/dev/null")?;
    // SAFETY: dup2 onto the standard fds; both sources are open.
    let moved = unsafe { libc::dup2(null.as_raw_fd(), 0) >= 0 && libc::dup2(2, 1) >= 0 };
    if !moved {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both fds were just created by fcntl and nothing else owns them.
    Ok(unsafe { (BufReader::new(File::from_raw_fd(requests)), File::from_raw_fd(replies)) })
}

#[cfg(not(unix))]
fn take_pipes() -> io::Result<(BufReader<File>, File)> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "the worker needs Unix"))
}
