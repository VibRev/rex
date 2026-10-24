//! The command line both tools share (CONVENTIONS §1; AX §1, §4.1). The
//! tool's name, version and help texts are set on it at run time
//! ([`Cli::parse_for`]).

use crate::proto::ExecRequest;
use crate::tool::Tool;
use axkit::agent::{Mode, OVERRIDE_ENV};
use axkit::daemon::Request;
use axkit::exit::{ErrorKind, Failure};
use axkit::output::Window;
use clap::{ArgGroup, Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Parser)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

/// Flags every command takes, before or after it. `--max-bytes` and
/// `--timeout` have a `<TOOL>_*` variable too, read from the client's
/// environment, never the daemon's. They stay out of the per-flag help: one
/// block lists them all ([`Cli::command_for`]), so a command's help shows
/// what is its own.
#[derive(Debug, Default, Args)]
pub struct Global {
    /// Skip the first N records.
    #[arg(long, global = true, hide = true, value_name = "N", value_parser = parse_count)]
    pub offset: Option<usize>,
    /// Show at most N records.
    #[arg(long, global = true, hide = true, value_name = "N", value_parser = parse_count)]
    pub limit: Option<usize>,
    /// Show at most N bytes of records.
    #[arg(long, global = true, hide = true, value_name = "N", value_parser = parse_count)]
    pub max_bytes: Option<usize>,
    /// Show everything, no window.
    #[arg(long, global = true, hide = true, conflicts_with_all = ["offset", "limit", "max_bytes"])]
    pub full: bool,
    /// Also write the whole output to FILE.
    #[arg(short = 'o', long = "output", global = true, hide = true, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Seconds a request may take before its worker is killed and the
    /// session lost; 0 for no limit. Default: 600 for exec, none otherwise.
    #[arg(long, global = true, hide = true, value_name = "SECS", value_parser = parse_count)]
    pub timeout: Option<usize>,
    /// Use the defaults for an agent even on a terminal.
    #[arg(long, global = true, hide = true, conflicts_with = "no_agent")]
    pub agent: bool,
    /// Use the defaults for a person even under an agent harness.
    #[arg(long, global = true, hide = true)]
    pub no_agent: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open a binary or database and wait for analysis.
    Open {
        /// The binary or database; its canonical path names the session.
        target: PathBuf,
    },
    /// Show one session; all of them without a target.
    Status {
        /// The binary or database of the session.
        target: Option<PathBuf>,
    },
    /// List the open sessions.
    List,
    /// Write the session's database: beside a binary, over the database
    /// that was opened, or to --to.
    Save {
        /// The binary or database of the session.
        target: PathBuf,
        /// Where to write it instead.
        #[arg(long, value_name = "PATH")]
        to: Option<PathBuf>,
        /// Overwrite a file this session did not write.
        #[arg(long)]
        force: bool,
    },
    /// Close a session without saving.
    Close {
        /// The binary or database of the session.
        target: PathBuf,
    },
    /// Run Python against the target, opening it first if needed.
    Exec(ExecArgs),
    /// Search the engine's installed Python API and the tool's notes; no query lists the topics. Needs no session.
    Ref {
        /// A topic, or words to search for.
        query: Vec<String>,
    },
    /// Serve exec, ref, save, close and list as MCP tools on stdin/stdout:
    /// configure this command as a stdio MCP server.
    Mcp,
    /// The background daemon that holds the sessions.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Print the agent skill (SKILL.md); install it where your harness loads skills.
    Skills {
        #[command(subcommand)]
        command: Option<SkillsCommand>,
    },
}

/// `exec <target> CODE`: the code is the argument, as `sqlite3 db SQL` takes
/// its SQL; `-c` says the same, `-f` names a file (§4).
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("source").required(true).args(["code", "code_flag", "file"])))]
pub struct ExecArgs {
    /// The binary or database to run against; opened on first use.
    pub target: PathBuf,
    /// Python to run, as one argument (several lines are fine); - reads it from stdin.
    #[arg(value_name = "CODE")]
    pub code: Option<String>,
    /// The same as CODE; given more than once, the pieces run as one script, a line each.
    #[arg(short = 'c', long = "code", value_name = "CODE")]
    pub code_flag: Vec<String>,
    /// Run the script in FILE; - reads it from stdin.
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    pub file: Option<PathBuf>,
    /// Print one JSON object: {"stdout", "result", "analysis"}; errors as {"error": {...}} on stderr.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Serve in the foreground (started for you on first use).
    Serve,
    /// Show running daemons and their sessions.
    Status,
    /// Stop daemons without sessions; --force also those with sessions.
    Stop {
        /// Also stop daemons holding sessions; their unsaved changes are lost.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    /// Print SKILL.md, or another file of the skill.
    Show {
        /// A file of the skill, as `skills list` names it.
        #[arg(default_value = "SKILL.md")]
        file: String,
    },
    /// List the skill's files.
    List,
}

impl Cli {
    /// The command tree with `tool`'s name, version and help. The top-level
    /// help reads: what the tool is, its commands, examples, exit status,
    /// then where the options are, so that its first lines are the ones an
    /// agent needs (AX §9). The global flags are listed once, in a block
    /// after each command's own help.
    pub fn command_for(tool: &Tool) -> clap::Command {
        let name = tool.name;
        let globals = global_options(name);
        let mut command = Cli::command()
            .name(name)
            .bin_name(name)
            .version(tool.version)
            .about(tool.about)
            .help_template("{about-with-newline}\n{usage-heading} {usage}\n\nCommands:\n{subcommands}{after-help}")
            .after_help(format!(
                "{}\n\nOptions: `{name} help <command>` or `{name} <command> --help` shows a command's own; \
                 every command also takes the output options there (--limit, --max-bytes, --timeout, ...).\n\n\
                 `{name} skills` prints the agent skill (SKILL.md); install it where your harness loads skills.",
                tool.examples
            ))
            .mut_subcommand("exec", |c| {
                // clap would put the required group before TARGET; the order matters to whoever copies it.
                c.override_usage(format!(
                    "{name} exec <TARGET> <CODE>\n       {name} exec <TARGET> -f <FILE>\n       {name} exec <TARGET> -    (the code on stdin)"
                ))
                .after_help(format!("{}\n\n{globals}", tool.exec_help))
            });
        for sub in ["open", "status", "list", "save", "close", "ref"] {
            command = command.mut_subcommand(sub, |c| c.after_help(globals.clone()));
        }
        command
    }

    /// Parse `args` (program name first) as `tool`'s command line.
    ///
    /// # Errors
    ///
    /// clap's: a usage error, or `--help` / `--version` to render.
    pub fn parse_for<I, T>(tool: &Tool, args: I) -> Result<Cli, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let mut command = Cli::command_for(tool);
        let matches = command.try_get_matches_from_mut(args)?;
        Cli::from_arg_matches(&matches).map_err(|e| e.format(&mut command))
    }

    /// Whether `args` reads the script from stdin (`exec <target> -` or
    /// `-f -`), for axkit's `Config::wants_stdin`.
    pub fn reads_stdin(args: &[OsString]) -> bool {
        Cli::try_parse_from(args).is_ok_and(|cli| matches!(cli.command, Command::Exec(e) if e.reads_stdin()))
    }
}

/// The block of global flags under a command's help, with the `<TOOL>_*`
/// variables spelt out.
fn global_options(tool: &str) -> String {
    let p = tool.to_ascii_uppercase();
    format!(
        "Output options (any command, before or after it):
  --limit N          at most N records
  --offset N         skip the first N records
  --max-bytes N      at most N bytes of records; agents get 8192 ({p}_MAX_BYTES)
  --full             everything, no window
  -o, --output FILE  also write the whole output to FILE
  --timeout SECS     kill the worker after SECS, losing the session; 0: no limit;
                     exec defaults to 600 ({p}_TIMEOUT)
  --agent, --no-agent  the defaults for an agent, or for a person"
    )
}

impl ExecArgs {
    pub fn reads_stdin(&self) -> bool {
        self.code.as_deref() == Some("-") || self.file.as_deref() == Some(Path::new("-"))
    }

    /// The code to run, wherever it comes from: the argument or `-c`, a
    /// file resolved against the client's cwd, or the client's stdin.
    ///
    /// # Errors
    ///
    /// The file cannot be read, stdin did not come with the request, or the
    /// argument names a file instead of being code.
    pub fn source(&self, request: &Request, tool: &str) -> Result<ExecRequest, Failure> {
        let target = shell_word(&self.target);
        let (code, filename) = match (self.code.as_deref(), self.file.as_deref()) {
            (Some("-"), _) => (stdin(request, tool)?, "<stdin>".to_owned()),
            (_, Some(file)) if file == Path::new("-") => (stdin(request, tool)?, "<stdin>".to_owned()),
            (_, Some(file)) => {
                let path = request.resolve(file);
                let code = std::fs::read_to_string(&path).map_err(|e| {
                    Failure::new(ErrorKind::NotFound, format!("cannot read the script {}: {e}", path.display()))
                        .with_hint(format!("paths are resolved against {}", request.cwd.display()))
                })?;
                (code, path.to_string_lossy().into_owned())
            }
            (Some(code), None) => {
                let path = request.resolve(Path::new(code));
                if looks_like_a_path(code) && path.is_file() {
                    return Err(Failure::new(ErrorKind::Usage, format!("CODE is Python, and {code} is a file"))
                        .with_hint(format!("{tool} exec {target} -f {}", shell_word(Path::new(code)))));
                }
                (code.to_owned(), "<exec>".to_owned())
            }
            (None, None) if !self.code_flag.is_empty() => (self.code_flag.join("\n"), "<exec>".to_owned()),
            (None, None) => {
                return Err(Failure::new(ErrorKind::Usage, "exec needs code: an argument, -c CODE, -f FILE, or - for stdin")
                    .with_hint(format!("{tool} exec {target} 'functions()'")));
            }
        };
        Ok(ExecRequest { code, filename })
    }
}

/// One word without anything code has in it: `s.py`, `./a/b`, not `f(x)`.
fn looks_like_a_path(code: &str) -> bool {
    !code.is_empty() && !code.chars().any(|c| c.is_whitespace() || "()[]{}=:'\",;".contains(c))
}

fn stdin(request: &Request, tool: &str) -> Result<String, Failure> {
    let stdin = request.stdin.as_deref().ok_or_else(|| {
        Failure::new(ErrorKind::Internal, "the script on stdin did not come with the request")
            .with_hint(format!("a daemon older than this {tool} may not forward stdin: `{tool} daemon stop`, then rerun"))
    })?;
    String::from_utf8(stdin.to_vec()).map_err(|_| {
        Failure::new(ErrorKind::Usage, "the script on stdin is not UTF-8").with_hint("save it as UTF-8, or pass the code as the argument")
    })
}

/// How one command's output is shaped: the flags given, then the client's
/// `<TOOL>_*` variables, then the reader's defaults (AX §4.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    pub window: Window,
    /// `--full`: no window, whatever the command's own default.
    pub full: bool,
    /// `-o`, resolved against the client's cwd.
    pub output: Option<PathBuf>,
    /// `--timeout` in seconds; 0 is no limit.
    pub timeout: Option<usize>,
}

impl Global {
    /// The shape of one command.
    pub fn shape(&self, request: &Request, tool: &str) -> Shape {
        let prefix = tool.to_ascii_uppercase();
        let count = |name: &str| request.var(&format!("{prefix}_{name}")).and_then(OsStr::to_str).and_then(|v| parse_count(v).ok());
        let forced = match (self.agent, self.no_agent) {
            (true, _) => Some("on"),
            (false, true) => Some("off"),
            (false, false) => None,
        };
        let mode = Mode::from_env(
            |name| match forced {
                Some(v) if name == OVERRIDE_ENV => Some(OsString::from(v)),
                _ => request.var(name).map(OsStr::to_os_string),
            },
            request.stdout_is_terminal,
        );
        let window = if self.full {
            Window::default()
        } else {
            let base = mode.default_window();
            Window {
                offset: self.offset.unwrap_or(base.offset),
                limit: self.limit.or(base.limit),
                max_bytes: self.max_bytes.or_else(|| count("MAX_BYTES")).or(base.max_bytes),
            }
        };
        Shape {
            window,
            full: self.full,
            output: self.output.as_ref().map(|p| request.resolve(p)),
            timeout: self.timeout.or_else(|| count("TIMEOUT")),
        }
    }
}

impl Shape {
    /// How long a request may take: `--timeout` if given (0: no limit),
    /// else `default`.
    pub fn timeout_or(&self, default: Option<Duration>) -> Option<Duration> {
        match self.timeout {
            Some(0) => None,
            Some(secs) => Some(Duration::from_secs(u64::try_from(secs).unwrap_or(u64::MAX))),
            None => default,
        }
    }
}

/// A count or a size: `16`, `0x10`. Fits clap's `value_parser`.
///
/// # Errors
///
/// `s` is not a non-negative integer.
pub fn parse_count(s: &str) -> Result<usize, String> {
    let s = s.trim();
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => usize::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.map_err(|_| format!("not a count: {s:?} (write 16 or 0x10)"))
}

/// `path` as one shell word, for hints that are pasted as they are.
pub fn shell_word(path: &Path) -> String {
    let s = path.to_string_lossy();
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c)) {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(env: &[(&str, &str)], terminal: bool) -> Request {
        Request {
            cwd: PathBuf::from("/w"),
            args: vec![],
            env: env.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))).collect(),
            stdout_is_terminal: terminal,
            stderr_is_terminal: terminal,
            stdin: None,
        }
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    #[test]
    fn the_command_tree_is_consistent() {
        Cli::command().debug_assert();
        Cli::command_for(&crate::tool::tests::TOOL).debug_assert();
    }

    #[test]
    fn global_flags_go_anywhere_and_take_decimal_or_hex() {
        let a = parse(&["t", "--limit", "0x10", "list"]);
        let b = parse(&["t", "list", "--limit", "16"]);
        assert_eq!((a.global.limit, b.global.limit), (Some(16), Some(16)));
        assert!(Cli::try_parse_from(["t", "list", "--limit", "-1"]).is_err());
    }

    #[test]
    fn exec_takes_code_as_its_argument_or_with_c_and_files_with_f() {
        let code = |args: &[&str]| {
            let Command::Exec(e) = parse(args).command else { panic!() };
            e.source(&request(&[], false), "t").unwrap()
        };
        let s = code(&["t", "exec", "fw", "for f in x:\n    print(f)"]);
        assert_eq!((s.code.as_str(), s.filename.as_str()), ("for f in x:\n    print(f)", "<exec>"));
        assert_eq!(code(&["t", "exec", "fw", "-c", "a = 1", "-c", "a"]).code, "a = 1\na", "several -c are one script");
        let cli = parse(&["t", "exec", "fw", "1", "--json", "--limit", "3"]);
        let Command::Exec(e) = &cli.command else { panic!() };
        assert!(e.json && cli.global.limit == Some(3), "flags after the code are the tool's");
        assert!(Cli::try_parse_from(["t", "list", "--json"]).is_err(), "JSON is exec's alone");
        assert!(Cli::try_parse_from(["t", "exec", "fw"]).is_err());
        assert!(Cli::try_parse_from(["t", "exec", "fw", "-c", "1", "2"]).is_err());
        assert!(Cli::try_parse_from(["t", "exec", "fw", "1", "extra"]).is_err(), "no script arguments any more");
        for stdin in [&["t", "exec", "fw", "-"][..], &["t", "exec", "fw", "-f", "-"]] {
            let args: Vec<OsString> = stdin.iter().map(OsString::from).collect();
            assert!(Cli::reads_stdin(&args), "{stdin:?}");
        }
        assert!(!Cli::reads_stdin(&["t".into(), "exec".into(), "fw".into(), "-c".into(), "1".into()]));
    }

    #[test]
    fn exec_sources_name_their_code() {
        let Command::Exec(e) = parse(&["t", "exec", "fw", "-"]).command else { panic!() };
        let f = e.source(&request(&[], false), "t").unwrap_err();
        assert!(f.kind == ErrorKind::Internal && f.hint.is_some(), "{f:?}");
        let with_stdin = Request { stdin: Some(b"print(1)".to_vec()), ..request(&[], false) };
        let s = e.source(&with_stdin, "t").unwrap();
        assert_eq!((s.code.as_str(), s.filename.as_str()), ("print(1)", "<stdin>"));
        let Command::Exec(e) = parse(&["t", "exec", "fw", "-f", "missing.py"]).command else { panic!() };
        let f = e.source(&request(&[], false), "t").unwrap_err();
        assert!(f.kind == ErrorKind::NotFound && f.message.contains("/w/missing.py") && f.hint.is_some(), "{f:?}");
    }

    #[test]
    fn code_that_names_a_file_says_to_use_f() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("s.py"), "1").unwrap();
        let at = Request { cwd: dir.path().to_path_buf(), ..request(&[], false) };
        let Command::Exec(e) = parse(&["t", "exec", "fw", "s.py"]).command else { panic!() };
        let f = e.source(&at, "t").unwrap_err();
        assert_eq!((f.kind, f.hint.as_deref()), (ErrorKind::Usage, Some("t exec fw -f s.py")));
        let Command::Exec(e) = parse(&["t", "exec", "fw", "f(s.py)"]).command else { panic!() };
        assert!(e.source(&at, "t").is_ok(), "code is code even when a word of it is a file");
        let Command::Exec(e) = parse(&["t", "exec", "fw", "-f", "s.py"]).command else { panic!() };
        assert_eq!(e.source(&at, "t").unwrap().code, "1");
    }

    #[test]
    fn the_top_level_help_shows_examples_early_and_options_in_one_block() {
        let help = Cli::command_for(&crate::tool::tests::TOOL).render_help().to_string();
        let examples = help.lines().position(|l| l.starts_with("Examples:")).unwrap_or(usize::MAX);
        assert!(examples < 30, "examples on line {examples}:\n{help}");
        assert!(!help.contains("--max-bytes <N>"), "no per-flag lines at the top: {help}");
        let mut command = Cli::command_for(&crate::tool::tests::TOOL);
        let exec = command.find_subcommand_mut("exec").unwrap().render_help().to_string();
        assert!(exec.contains("TX_MAX_BYTES") && !exec.contains("<TOOL>"), "{exec}");
        assert!(exec.contains("The binary or database"), "TARGET is described: {exec}");
        assert!(exec.matches("--max-bytes").count() == 1, "the globals once, in the block: {exec}");
    }

    #[test]
    fn flags_beat_the_environment_and_agents_get_a_byte_window() {
        let global = parse(&["t", "list"]).global;
        assert_eq!(global.shape(&request(&[("CLAUDECODE", "1")], false), "t").window.max_bytes, Some(8192));
        assert!(global.shape(&request(&[("CLAUDECODE", "1")], true), "t").window.is_unbounded());

        let global = parse(&["t", "list", "--max-bytes", "100", "-o", "out.txt"]).global;
        let shape = global.shape(&request(&[("T_MAX_BYTES", "5"), ("T_LIMIT", "3"), ("T_TIMEOUT", "9")], false), "t");
        assert_eq!((shape.window.max_bytes, shape.window.limit), (Some(100), None), "T_LIMIT is no variable of ours");
        assert_eq!(shape.output.as_deref(), Some(Path::new("/w/out.txt")));
        assert_eq!(shape.timeout_or(None), Some(Duration::from_secs(9)));
        let shape = parse(&["t", "list"]).global.shape(&request(&[("T_MAX_BYTES", "5")], true), "t");
        assert_eq!(shape.window.max_bytes, Some(5));

        let global = parse(&["t", "list", "--full", "--timeout", "0"]).global;
        let shape = global.shape(&request(&[("CLAUDECODE", "1")], false), "t");
        assert!(shape.window.is_unbounded());
        assert_eq!(shape.timeout_or(Some(Duration::from_secs(600))), None, "0 is no limit");
        let global = parse(&["t", "list", "--agent"]).global;
        assert_eq!(global.shape(&request(&[], true), "t").window.max_bytes, Some(8192));
    }

    #[test]
    fn hints_quote_paths_only_when_needed() {
        assert_eq!(shell_word(Path::new("/usr/bin/true")), "/usr/bin/true");
        assert_eq!(shell_word(Path::new("/w/my fw.bin")), "'/w/my fw.bin'");
        assert_eq!(shell_word(Path::new("/w/it's")), r"'/w/it'\''s'");
    }
}
