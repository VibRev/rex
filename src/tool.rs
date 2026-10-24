//! A whole tool but its engine: the commands of §1 over [`Sessions`], run
//! in axkit's daemon. `idax` and `bnx` each describe themselves in a
//! [`Tool`], implement a [`Backend`] and call [`main`].

use crate::cli::{Cli, Command, DaemonCommand, ExecArgs, Shape, shell_word};
use crate::proto::{self, Reply, State};
use crate::pyapi::{self, ApiIndex};
use crate::reference::{Answer, Corpus, Docs};
use crate::save::Databases;
use crate::session::{Info, Opened, Session, Sessions};
use crate::skills;
use crate::worker::{self, Backend};
use axkit::daemon::{self, Cancel, Config, Handler, Io, Outcome, Request};
use axkit::exit::{ErrorKind, Exit, Failure};
use axkit::output::{Sink, SpillDir};
use axkit::skills::{Skill, SkillFile};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Notes of the tool's own a word search shows before the engine's API.
const NOTE_HITS: usize = 3;

/// How long an `exec` may run unless `--timeout` says otherwise.
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(600);

/// What tells one tool from the other, but for its [`Backend`].
#[derive(Debug)]
pub struct Tool {
    /// `idax`: the command, directories, `<TOOL>_*` variables.
    pub name: &'static str,
    pub version: &'static str,
    /// The engine in messages: `IDA`.
    pub engine: &'static str,
    pub databases: Databases,
    /// `--help`: one line, then examples after the command list.
    pub about: &'static str,
    pub examples: &'static str,
    /// `exec --help` after the flags: what a script sees.
    pub exec_help: &'static str,
    /// What `exec` runs, in a phrase for the MCP tools: `IDAPython (IDA's
    /// modules imported)`.
    pub scripting: &'static str,
    /// The `ref` corpus: one markdown text per topic, only what is the
    /// tool's own (§5).
    pub corpus: &'static [&'static str],
    /// Where the engine install keeps its API reference and examples.
    pub docs: Docs,
    /// The agent skill: `SKILL.md` and the files it links to (§5).
    pub skill: &'static [SkillFile],
}

impl Tool {
    /// The daemon of a tool with sessions: no daemon is a failure, not a
    /// run elsewhere (AX §8).
    pub fn config(&self) -> Config {
        let mut config = Config::new(self.name, self.version);
        config.wants_stdin = Cli::reads_stdin;
        config
    }

    /// The bundled skill, named after the tool.
    pub fn skill(&self) -> Skill {
        Skill { name: self.name, files: self.skill }
    }
}

/// The tool's `main`: a worker when started as one, the daemon commands
/// here, anything else in the daemon (or here with `<TOOL>_DAEMON=off`).
pub fn main<B: Backend>(tool: &'static Tool) -> ExitCode {
    match worker::launch() {
        Some(Ok(launch)) => return worker::serve::<B>(&launch),
        Some(Err(why)) => {
            eprintln!("{} worker: {why}", tool.name);
            return Exit::Error.into();
        }
        None => {}
    }
    let config = tool.config();
    match Cli::parse_for(tool, std::env::args_os()) {
        Ok(Cli { command: Command::Daemon { command }, .. }) => return daemon_command(tool, &config, &command),
        Ok(Cli { command: Command::Mcp, .. }) => return crate::mcp::serve(tool, &config),
        Ok(Cli { command: Command::Skills { command }, .. }) => {
            return match skills::run(tool, command.as_ref(), &mut io::stdout().lock()) {
                Ok(exit) => exit.into(),
                Err(failure) => fail(&failure),
            };
        }
        // Help and the version need no session: print them here, as the
        // daemon would, without starting one.
        Err(e) if Failure::from_clap(&e).is_none() => {
            // A reader that stops early (`| head -30`) is not an error.
            let _ = write!(io::stdout().lock(), "{}", e.render());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    if let Some(code) = daemon::forward(&config) {
        return ExitCode::from(code);
    }
    let request = match Request::capture(&config) {
        Ok(r) => r,
        Err(e) => {
            return fail(
                &Failure::new(ErrorKind::Internal, format!("cannot read the working directory or stdin: {e}"))
                    .with_hint("cd to a directory that exists and rerun"),
            );
        }
    };
    let rex = Rex::new(tool);
    let (stdout, stderr) = (io::stdout(), io::stderr());
    let (mut out, mut err) = (stdout.lock(), stderr.lock());
    let outcome = rex.run(&request, &mut Io::new(&mut out, &mut err, Cancel::default()));
    let _ = out.flush();
    rex.shutdown();
    match outcome {
        Outcome::Exit(exit) => exit.into(),
        Outcome::Decline(why) => fail(&Failure::new(ErrorKind::Internal, why).with_hint("rerun the command")),
    }
}

fn daemon_command(tool: &'static Tool, config: &Config, command: &DaemonCommand) -> ExitCode {
    let result = match command {
        DaemonCommand::Serve => daemon::serve(config, &Rex::new(tool)),
        DaemonCommand::Status => daemon::status(config).map(|lines| {
            if lines.is_empty() {
                println!("# no {} daemon running", tool.name);
            }
            lines.iter().for_each(|l| println!("{l}"));
        }),
        DaemonCommand::Stop { force } => daemon::stop(config, *force).map(|n| println!("stopped | {n}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&Failure::new(ErrorKind::Internal, format!("daemon: {e}")).with_hint(format!("{} daemon status", tool.name))),
    }
}

fn fail(failure: &Failure) -> ExitCode {
    let _ = failure.report(&mut io::stderr(), false);
    failure.exit().into()
}

/// The commands, run by the daemon or in-process (axkit [`Handler`]).
pub struct Rex {
    tool: &'static Tool,
    sessions: Sessions,
    spill: SpillDir,
    corpus: Corpus,
    /// The install's API, read on the first `ref` that needs it.
    api: OnceLock<Option<ApiIndex>>,
}

impl Rex {
    pub fn new(tool: &'static Tool) -> Rex {
        Rex {
            tool,
            sessions: Sessions::new(tool),
            spill: SpillDir::for_tool(tool.name),
            corpus: Corpus::parse(tool.corpus),
            api: OnceLock::new(),
        }
    }

    fn api(&self) -> Option<&ApiIndex> {
        let docs = &self.tool.docs;
        self.api.get_or_init(|| docs.find(|k| std::env::var_os(k)).ok()?.index(docs.naming)).as_ref()
    }
}

impl Handler for Rex {
    fn run(&self, request: &Request, io: &mut Io<'_>) -> Outcome {
        let cli = match Cli::parse_for(self.tool, &request.args) {
            Ok(cli) => cli,
            Err(e) => {
                return Outcome::Exit(match usage_failure(self.tool, &request.args, &e) {
                    None => {
                        let _ = write!(io.out, "{}", e.render());
                        Exit::Success
                    }
                    Some(failure) => {
                        let _ = failure.report(io.err, request.wants_json());
                        failure.exit()
                    }
                });
            }
        };
        let shape = cli.global.shape(request, self.tool.name);
        let json = matches!(&cli.command, Command::Exec(e) if e.json);
        let run = Run { rex: self, request, shape: &shape };
        let exit = run.dispatch(cli.command, io).unwrap_or_else(|failure| {
            let _ = failure.report(io.err, json);
            failure.exit()
        });
        Outcome::Exit(exit)
    }

    fn status(&self, _idle_left: Duration) -> String {
        self.sessions.all().iter().map(|s| format!("  {}\n", Row::new(s, &s.info()).text())).collect()
    }

    fn holds_sessions(&self) -> bool {
        !self.sessions.is_empty()
    }

    fn shutdown(&self) {
        self.sessions.shutdown();
    }
}

/// One command being run.
struct Run<'a> {
    rex: &'a Rex,
    request: &'a Request,
    shape: &'a Shape,
}

impl Run<'_> {
    fn dispatch(&self, command: Command, io: &mut Io<'_>) -> Result<Exit, Failure> {
        match command {
            Command::Open { target } => self.open(&target, io),
            Command::Status { target: Some(target) } => self.status(&target, io),
            Command::Status { target: None } | Command::List => self.list(io),
            Command::Save { target, to, force } => self.save(&target, to.as_deref(), force, io),
            Command::Close { target } => self.close(&target, io),
            Command::Exec(args) => self.exec(&args, io),
            Command::Ref { query } => self.reference(&query, io),
            Command::Daemon { .. } | Command::Skills { .. } | Command::Mcp => {
                let name = self.tool().name;
                Err(Failure::new(
                    ErrorKind::Usage,
                    format!("`{name} daemon`, `{name} skills` and `{name} mcp` run in the client, not in the daemon"),
                )
                .with_hint(format!("run it as `{name} ...` from a shell, not through the daemon")))
            }
        }
    }

    fn tool(&self) -> &'static Tool {
        self.rex.tool
    }

    fn sessions(&self) -> &Sessions {
        &self.rex.sessions
    }

    /// A target that must exist: its canonical path, the session key.
    fn existing(&self, target: &Path) -> Result<PathBuf, Failure> {
        self.request.canonical_path(target).map_err(|e| {
            Failure::new(ErrorKind::NotFound, format!("{}: {e}", self.request.resolve(target).display()))
                .with_hint(format!("paths are resolved against {}", self.request.cwd.display()))
        })
    }

    /// The key of a target that may since have gone from disk.
    fn key(&self, target: &Path) -> PathBuf {
        self.request.canonical_path(target).unwrap_or_else(|_| self.request.resolve(target))
    }

    fn session_of(&self, key: &Path, what: &str) -> Result<Arc<Session>, Failure> {
        self.sessions().get(key).ok_or_else(|| {
            Failure::new(ErrorKind::NotFound, format!("no session for {}{what}", key.display())).with_hint(format!(
                "{} open {}",
                self.tool().name,
                shell_word(key)
            ))
        })
    }

    fn sink<'s>(&'s self, out: &'s mut dyn Write) -> Result<Sink<'s, &'s mut dyn Write>, Failure> {
        let sink = Sink::new(out, self.shape.window, Some(&self.rex.spill));
        match &self.shape.output {
            Some(path) => sink.with_output_file(path).map_err(|e| {
                Failure::new(ErrorKind::Usage, e.to_string()).with_hint("-o needs a path in a directory that exists and is writable")
            }),
            None => Ok(sink),
        }
    }

    /// The session of `key`, opened if needed, once its analysis has
    /// settled. A new worker is always asked: an engine may look idle the
    /// moment it has loaded, before analysis has started.
    fn opened(&self, key: &Path, io: &mut Io<'_>) -> Result<Opened, Failure> {
        let opened = self.sessions().open(key, io, self.shape.timeout_or(None))?;
        let incomplete = opened.session.info().state.as_ref().is_none_or(|s| !s.analysis.complete);
        if opened.fresh || incomplete {
            self.sessions().call(&opened.session, proto::Request::Analyze, "auto-analysis", io, self.shape.timeout_or(None))?;
        }
        Ok(opened)
    }

    fn open(&self, target: &Path, io: &mut Io<'_>) -> Result<Exit, Failure> {
        let key = self.existing(target)?;
        let databases = self.tool().databases;
        if self.sessions().get(&key).is_none() && !databases.is_database(&key) {
            let beside = databases.beside(&key);
            if beside.exists() {
                let _ = writeln!(
                    io.err,
                    "hint: {} exists; `{} open {}` opens it instead",
                    beside.display(),
                    self.tool().name,
                    shell_word(&beside)
                );
            }
        }
        let opened = self.opened(&key, io)?;
        let info = opened.session.info().clone();
        self.rows(&[Row::new(&opened.session, &info)], io)
    }

    fn status(&self, target: &Path, io: &mut Io<'_>) -> Result<Exit, Failure> {
        let session = self.session_of(&self.key(target), "")?;
        let info = self.sessions().refresh(&session, io)?;
        self.rows(&[Row::new(&session, &info)], io)
    }

    fn list(&self, io: &mut Io<'_>) -> Result<Exit, Failure> {
        let rows: Vec<Row> = self.sessions().all().iter().map(|s| Row::new(s, &s.info())).collect();
        self.rows(&rows, io)
    }

    /// Sessions as a table.
    fn rows(&self, rows: &[Row], io: &mut Io<'_>) -> Result<Exit, Failure> {
        let mut sink = self.sink(&mut *io.out)?;
        if rows.is_empty() {
            sink.comment("# no sessions");
        } else {
            sink.comment(Row::HEADER);
            rows.iter().for_each(|r| sink.record(&r.text()));
        }
        finish(sink)?;
        Ok(Exit::Success)
    }

    fn save(&self, target: &Path, to: Option<&Path>, force: bool, io: &mut Io<'_>) -> Result<Exit, Failure> {
        let key = self.key(target);
        let session = self.session_of(&key, ", so nothing to save")?;
        let saved_to = session.info().saved_to.clone();
        let to = to.map(|p| self.request.resolve(p));
        let save = self.tool().databases.plan(self.tool().name, &key, to.as_deref(), saved_to.as_deref(), force)?;
        let reply =
            self.sessions().call(&session, proto::Request::Save { to: save.partial.clone() }, "save", io, self.shape.timeout_or(None));
        if let Err(failure) = reply {
            save.abandon();
            return Err(failure);
        }
        save.commit().map_err(|e| {
            Failure::new(ErrorKind::EngineError, format!("cannot move the saved database to {}: {e}", save.dest.display()))
                .with_hint("check that its directory is writable, then save again; the session is as it was")
        })?;
        self.sessions().note_saved(&session, &save.dest);
        let mut sink = self.sink(&mut *io.out)?;
        sink.record(&format!("saved | {}", save.dest.display()));
        finish(sink)?;
        Ok(Exit::Success)
    }

    fn close(&self, target: &Path, io: &mut Io<'_>) -> Result<Exit, Failure> {
        let key = self.key(target);
        let closed = self.sessions().close(&key);
        let mut sink = self.sink(&mut *io.out)?;
        match closed {
            Some(_) => sink.record(&format!("closed | {}", key.display())),
            None => sink.comment(&format!("# no session for {}", key.display())),
        }
        finish(sink)?;
        Ok(Exit::Success)
    }

    fn exec(&self, args: &ExecArgs, io: &mut Io<'_>) -> Result<Exit, Failure> {
        let exec = args.source(self.request, self.tool().name)?;
        let key = self.existing(&args.target)?;
        let opened = self.opened(&key, io)?;
        let reply =
            self.sessions().call(&opened.session, proto::Request::Exec(exec), "exec", io, self.shape.timeout_or(Some(EXEC_TIMEOUT)))?;
        let Reply::Ran { run, before, .. } = reply else {
            return Err(Failure::new(ErrorKind::Internal, format!("the {} worker answered {reply:?} to exec", self.tool().engine))
                .with_hint(format!("rerun; if it repeats, `{} close {}` and open it again", self.tool().name, shell_word(&key))));
        };
        let mut sink = self.sink(&mut *io.out)?;
        let exit = run.render(&mut sink, io.err, args.json, before.analysis);
        finish(sink)?;
        Ok(exit)
    }

    /// `ref`: a topic, or a word search over the tool's notes and then the
    /// engine's API and examples in the install it runs on (§5).
    fn reference(&self, query: &[String], io: &mut Io<'_>) -> Result<Exit, Failure> {
        let answer = self.rex.corpus.lookup(query);
        let Answer::Hits(notes) = answer else { return self.answer(answer, io) };
        let limit = if self.shape.full { usize::MAX } else { self.shape.window.limit.unwrap_or(pyapi::DEFAULT_HITS) };
        let api = self.rex.api().map(|index| index.search(query, limit));
        let mut sink = self.sink(&mut *io.out)?;
        // A note shows only when it has every word: the engine's API answers
        // the rest better.
        let words: Vec<String> = query.iter().flat_map(|q| q.split_whitespace()).map(str::to_lowercase).collect();
        let notes: Vec<_> = notes.into_iter().filter(|e| e.mentions_all(&words)).take(NOTE_HITS).collect();
        // An identifier (`get_func`, `BinaryView.hlil`) asks for the API; plain
        // words (`rename`) more likely for a note.
        let identifier = words.len() == 1 && query.iter().any(|q| q.contains(['_', '.']) || q.chars().any(char::is_uppercase));
        let api_records = api.as_ref().map(pyapi::Hits::records).unwrap_or_default();
        let note_records: Vec<String> = notes.iter().map(|e| e.to_record()).collect();
        let (first, then) = if identifier { (&api_records, &note_records) } else { (&note_records, &api_records) };
        first.iter().chain(then).for_each(|r| sink.record(r));
        if notes.is_empty() && api.as_ref().is_none_or(pyapi::Hits::is_empty) {
            let topics: Vec<&str> = self.rex.corpus.topics().iter().map(|t| t.name).collect();
            let _ = writeln!(
                io.err,
                "hint: nothing matches; try fewer or other words, a topic ({}), or `{} ref` for the install's files to grep",
                topics.join(", "),
                self.tool().name
            );
        }
        finish(sink)?;
        Ok(Exit::Success)
    }

    fn answer(&self, answer: Answer<'_>, io: &mut Io<'_>) -> Result<Exit, Failure> {
        let name = self.tool().name;
        let mut sink = self.sink(&mut *io.out)?;
        match answer {
            Answer::Topics(topics) => {
                let docs = self.tool().docs.find(|k| std::env::var_os(k));
                if let Err(why) = &docs {
                    let _ = writeln!(
                        io.err,
                        "hint: the {} API reference is not listed: {why}; set {} to the {} install",
                        self.tool().engine,
                        self.tool().docs.env,
                        self.tool().engine
                    );
                }
                sink.comment("# topic | about | entries");
                topics.iter().for_each(|t| sink.record(&format!("{} | {} | {}", t.name, t.about, t.entries.len())));
                sink.comment(&format!("# {name} ref <topic> shows one; {name} ref <words> searches them all"));
                if let Ok(found) = &docs {
                    sink.comment(&format!(
                        "# the {} API and its official examples, from the install this {name} runs on: {}",
                        self.tool().engine,
                        found.install.display()
                    ));
                    sink.comment("# path | holds | try");
                    for place in &found.places {
                        for try_ in &place.tries {
                            sink.record(&format!("{} | {} | {try_}", place.path.display(), place.holds));
                        }
                    }
                }
            }
            Answer::Topic(topic) => topic.entries.iter().for_each(|e| sink.record(&e.to_record())),
            // `reference` answers a word search itself.
            Answer::Hits(hits) => hits.iter().for_each(|e| sink.record(&e.to_record())),
        }
        finish(sink)?;
        Ok(Exit::Success)
    }
}

fn finish<W: Write>(sink: Sink<'_, W>) -> Result<(), Failure> {
    sink.finish()
        .map(drop)
        .map_err(|e| Failure::new(ErrorKind::Internal, e.to_string()).with_hint("check the -o path and the disk, then rerun"))
}

/// A usage error as clap reports it, with the next step spelt out: clap's
/// own tips and usage line, the commands when the command is unknown, and
/// the `--help` to paste (§6).
fn usage_failure(tool: &Tool, args: &[std::ffi::OsString], e: &clap::Error) -> Option<Failure> {
    let failure = Failure::from_clap(e)?;
    let command = Cli::command_for(tool);
    let mut hints: Vec<String> =
        failure.hint.iter().flat_map(|h| h.split("; ")).filter(|h| !h.starts_with("For more information")).map(str::to_owned).collect();
    if e.kind() == clap::error::ErrorKind::InvalidSubcommand {
        let mut subcommands: Vec<&clap::Command> =
            command.get_subcommands().filter(|c| !c.is_hide_set() && c.get_name() != "help").collect();
        subcommands.sort_by_key(|c| c.get_display_order());
        let names: Vec<&str> = subcommands.iter().map(|c| c.get_name()).collect();
        hints.push(format!("the commands are {}", names.join(", ")));
    }
    hints.push(format!("`{} --help`", command_path(&command, args).join(" ")));
    Some(Failure { hint: Some(hints.join("; ")), ..failure })
}

/// The command `args` names, as far as it does: `idax`, `idax exec`,
/// `idax daemon stop`.
fn command_path<'c>(command: &'c clap::Command, args: &[std::ffi::OsString]) -> Vec<&'c str> {
    let mut path = vec![command.get_name()];
    let mut current = command;
    for arg in args.iter().skip(1).filter_map(|a| a.to_str()) {
        if let Some(sub) = current.find_subcommand(arg).filter(|s| s.get_name() != "help") {
            path.push(sub.get_name());
            current = sub;
        }
    }
    path
}

/// One session as `open`, `status` and `list` show it.
struct Row {
    target: PathBuf,
    state: Option<State>,
    /// The request running now and since when.
    busy: Option<(&'static str, Duration)>,
    idle: Duration,
}

impl Row {
    const HEADER: &'static str = "# target | analysis | idle";

    fn new(session: &Session, info: &Info) -> Row {
        Row {
            target: session.target.clone(),
            state: info.state.clone(),
            busy: info.busy.map(|(what, since)| (what, since.elapsed())),
            idle: info.used.elapsed(),
        }
    }

    /// `analysis` is `starting` until the worker has opened the target;
    /// `idle` says what runs while one does.
    fn text(&self) -> String {
        let analysis = self.state.as_ref().map_or_else(|| "starting".to_owned(), |s| s.analysis.cell(s.phase.as_deref()));
        let idle = match self.busy {
            Some((what, for_)) => format!("busy: {what} for {}s", for_.as_secs()),
            None => format!("{}s", self.idle.as_secs()),
        };
        format!("{} | {analysis} | {idle}", self.target.display())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::analysis::Analysis;

    pub(crate) static TOOL: Tool = Tool {
        name: "tx",
        version: "0.0.0",
        engine: "Test",
        databases: Databases { extensions: &["tdb"] },
        about: "a test tool",
        examples: "Examples:\n  tx exec t 1",
        exec_help: "db is the database",
        scripting: "test Python",
        corpus: &["# t: a topic\n## an entry\nText.\n    tx ref t\n"],
        docs: Docs { library: "libnothing", env: "TX_ENGINE_DIR", naming: crate::pyapi::Naming::Stem, places: &[] },
        skill: &[SkillFile { path: "SKILL.md", text: "---\nname: tx\nmetadata:\n  version: \"0.0.0\"\n---\n" }],
    };

    fn state(complete: bool) -> State {
        State {
            analysis: if complete { Analysis::COMPLETE } else { Analysis { complete: false, progress: Some(0.5) } },
            phase: (!complete).then(|| "analyzing 1/2".to_owned()),
        }
    }

    fn row(state: Option<State>, busy: Option<(&'static str, Duration)>) -> Row {
        Row { target: "/w/true".into(), state, busy, idle: Duration::from_secs(3) }
    }

    #[test]
    fn a_session_row_is_one_line_in_header_order() {
        assert_eq!(Row::HEADER, "# target | analysis | idle");
        assert_eq!(row(Some(state(true)), None).text(), "/w/true | complete | 3s");
        let half = row(Some(state(false)), Some(("exec", Duration::from_secs(1))));
        assert_eq!(half.text(), "/w/true | 50% (analyzing 1/2) | busy: exec for 1s");
        assert_eq!(row(None, None).text(), "/w/true | starting | 3s");
    }

    fn usage(args: &[&str]) -> Failure {
        let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
        let e = Cli::parse_for(&TOOL, &args).unwrap_err();
        usage_failure(&TOOL, &args, &e).expect("a usage error")
    }

    #[test]
    fn usage_errors_say_what_to_paste_next() {
        let f = usage(&["tx", "exec", "t", "1", "--limt", "3"]);
        let hint = f.hint.unwrap();
        assert!(hint.contains("--limit") && hint.ends_with("`tx exec --help`"), "{hint}");
        let f = usage(&["tx", "decompile", "t"]);
        let hint = f.hint.unwrap();
        assert!(hint.contains("the commands are open, status") && hint.ends_with("`tx --help`"), "{hint}");
        let f = usage(&["tx", "exec", "t"]);
        assert!(f.hint.unwrap().ends_with("`tx exec --help`"));
        let f = usage(&["tx", "daemon", "stp"]);
        assert!(f.hint.unwrap().ends_with("`tx daemon --help`"));
        for help in [&["tx", "help"][..], &["tx", "help", "exec"], &["tx", "exec", "--help"]] {
            let args: Vec<std::ffi::OsString> = help.iter().map(Into::into).collect();
            let e = Cli::parse_for(&TOOL, &args).unwrap_err();
            assert!(usage_failure(&TOOL, &args, &e).is_none(), "{help:?} is help, not an error");
        }
        let e = Cli::parse_for(&TOOL, ["tx", "help", "exec"]).unwrap_err();
        assert!(e.render().to_string().contains("Run Python against the target"), "help exec is exec --help");
    }

    #[test]
    fn the_config_reads_stdin_for_exec_dash() {
        let config = TOOL.config();
        assert!((config.wants_stdin)(&["tx".into(), "exec".into(), "t".into(), "-".into()]));
    }
}
