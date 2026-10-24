//! What the daemon and a worker say to each other (CONVENTIONS §3): one
//! JSON object per line, [`Down`] on the worker's stdin, [`Up`] on its
//! stdout. The worker is started with its [`Launch`] as an argument and
//! answers it with [`Reply::Opened`] or [`Up::Failed`]; after that, one
//! reply per request. Closing its stdin ends it, unsaved.

use crate::analysis::Analysis;
use crate::exec::ScriptRun;
use axkit::exit::{ErrorKind, Failure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How a worker opens its session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    /// The target as the session knows it: the canonical path.
    pub target: PathBuf,
    /// The file to load: the target binary, or the session's working copy
    /// of a target database, so that nothing beside the target changes
    /// before `save`.
    pub input: PathBuf,
    /// The session's private directory: the engine's own database files
    /// and the prelude's scratch files.
    pub workdir: PathBuf,
}

/// Daemon to worker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Down {
    Request(Request),
    /// Interrupt the running request: its client has gone.
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Report the state once analysis has settled.
    Analyze,
    Exec(ExecRequest),
    /// Write a database to `to`, a fresh path; the session goes on as it was.
    Save {
        to: PathBuf,
    },
    Status,
}

/// One script to run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecRequest {
    pub code: String,
    /// The name tracebacks give the code, and `sys.argv[0]`: `<exec>`,
    /// `<stdin>`, a path.
    pub filename: String,
}

/// Worker to daemon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Up {
    /// A `# progress: ...` line for the client's stderr.
    Progress(String),
    Reply(Box<Reply>),
    /// The request failed; the session goes on (unless it was the launch).
    Failed(WorkerFailure),
}

// One reply per request, boxed in `Up`: the size of a variant does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Opened(State),
    Analyzed(State),
    /// `before` is the session as the script found it: its answer was
    /// computed with that analysis, whatever the script set off.
    Ran {
        run: ScriptRun,
        before: State,
        after: State,
    },
    Saved(State),
    Status(State),
}

impl Reply {
    /// The session's state after the request.
    pub fn state(&self) -> &State {
        match self {
            Reply::Opened(s) | Reply::Analyzed(s) | Reply::Saved(s) | Reply::Status(s) | Reply::Ran { after: s, .. } => s,
        }
    }
}

/// A session as the worker sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub analysis: Analysis,
    /// What analysis is doing, in the engine's words: `analyzing 12/40`.
    pub phase: Option<String>,
}

/// A request the engine could not carry out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerFailure {
    pub kind: FailKind,
    pub message: String,
    pub hint: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailKind {
    /// The input cannot be opened.
    TargetUnavailable,
    EngineError,
}

impl WorkerFailure {
    pub fn engine(message: impl Into<String>) -> WorkerFailure {
        WorkerFailure { kind: FailKind::EngineError, message: message.into(), hint: None }
    }

    pub fn target(message: impl Into<String>) -> WorkerFailure {
        WorkerFailure { kind: FailKind::TargetUnavailable, message: message.into(), hint: None }
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> WorkerFailure {
        self.hint = Some(hint.into());
        self
    }

    /// The report of this failure; `fallback` is its hint when the engine
    /// gave none, as every failure carries one (§6).
    pub fn into_failure(self, fallback: impl FnOnce() -> String) -> Failure {
        let kind = match self.kind {
            FailKind::TargetUnavailable => ErrorKind::TargetUnavailable,
            FailKind::EngineError => ErrorKind::EngineError,
        };
        Failure::new(kind, self.message).with_hint(self.hint.unwrap_or_else(fallback))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_message_is_one_json_line() {
        let down = Down::Request(Request::Exec(ExecRequest { code: "a\nb".into(), filename: "<exec>".into() }));
        let line = serde_json::to_string(&down).unwrap();
        assert!(!line.contains('\n'), "{line}");
        assert_eq!(serde_json::from_str::<Down>(&line).unwrap(), down);
        assert_eq!(serde_json::to_string(&Down::Cancel).unwrap(), r#""cancel""#);
        let save = serde_json::to_value(Down::Request(Request::Save { to: "/a/.b".into() })).unwrap();
        assert_eq!(save, json!({"request": {"op": "save", "to": "/a/.b"}}));
    }

    #[test]
    fn a_worker_failure_becomes_the_report_of_its_kind() {
        let up: Up = serde_json::from_value(json!({"failed": {"kind": "target_unavailable", "message": "m", "hint": "h"}})).unwrap();
        let Up::Failed(f) = up else { panic!() };
        let failure = f.clone().into_failure(|| "fallback".into());
        assert_eq!((failure.kind, failure.hint.as_deref()), (ErrorKind::TargetUnavailable, Some("h")));
        let bare = WorkerFailure { hint: None, ..f }.into_failure(|| "fallback".into());
        assert_eq!(bare.hint.as_deref(), Some("fallback"));
    }
}
