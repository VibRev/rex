//! `<tool> mcp`: the commands as a stdio MCP server in code mode (§5).
//!
//! Five tools — `exec`, `ref`, `save`, `close`, `list` — each one command
//! line, run as the CLI runs it: in the same daemon (or here when it is
//! off), rendered for an agent, with the same output window, spill files and
//! `Error:` / `hint:` lines. A tool call's text is the command's stdout, then
//! its stderr; any exit status but 0 marks the call an error.

use crate::tool::{Rex, Tool};
use axkit::daemon::{self, Cancel, Config, Handler as _, Io, Outcome, Request};
use axkit::mcp::{self, CallResult, ServerInfo, ToolDef, schema};
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

/// Serve on stdin and stdout until the client hangs up.
pub fn serve(tool: &'static Tool, config: &Config) -> ExitCode {
    let info = ServerInfo { name: tool.name.to_owned(), version: tool.version.to_owned(), instructions: Some(instructions(tool)) };
    let mut tools = Tools { tool, config, local: None };
    let result = mcp::serve(&info, &mut tools, io::stdin().lock(), io::stdout().lock());
    if let Some(rex) = &tools.local {
        rex.shutdown();
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} mcp: {e}", tool.name);
            ExitCode::FAILURE
        }
    }
}

fn instructions(tool: &Tool) -> String {
    format!(
        "exec runs {} against TARGET (a path to a binary or {} database) in a persistent headless session, opened on first use; \
         the value of the last expression is the result. Use ref(query) for API signatures instead of guessing. \
         Nothing is written to disk unless save is called.",
        tool.scripting, tool.engine
    )
}

struct Tools<'c> {
    tool: &'static Tool,
    config: &'c Config,
    /// The commands run here, sessions and all, when the daemon is off.
    local: Option<Rex>,
}

impl mcp::Handler for Tools<'_> {
    fn tools(&self) -> Vec<ToolDef> {
        let target = || schema::string("path to the binary or database, relative to the server's working directory");
        let def = |name: &str, description: String, input_schema: Value| ToolDef { name: name.to_owned(), description, input_schema };
        vec![
            def(
                "exec",
                format!(
                    "Run {} against target, opening it first if needed; returns what it printed and the last expression's value.",
                    self.tool.scripting
                ),
                schema::object(&[
                    ("target", target(), true),
                    ("code", schema::string("Python source; several lines are fine"), true),
                    ("timeout", schema::integer("seconds before the session is killed (default 600, 0 for none)"), false),
                ]),
            ),
            def(
                "ref",
                format!(
                    "Search the {} Python API (signatures, docstrings, file:line) and official examples of the install this server runs on.",
                    self.tool.engine
                ),
                schema::object(&[("query", schema::string("API names or words, e.g. `get_func` or `xrefs to`"), true)]),
            ),
            def(
                "save",
                "Write the session's database to disk: beside a binary, over the database that was opened, or to path.".to_owned(),
                schema::object(&[
                    ("target", target(), true),
                    ("path", schema::string("where to write the database instead"), false),
                    ("force", schema::boolean("overwrite a file this session did not write"), false),
                ]),
            ),
            def("close", "Close the session without saving.".to_owned(), schema::object(&[("target", target(), true)])),
            def("list", "List the open sessions and their analysis state.".to_owned(), schema::object(&[])),
        ]
    }

    fn call(&mut self, name: &str, args: &Map<String, Value>) -> Option<CallResult> {
        let argv = match argv(name, args)? {
            Ok(argv) => argv,
            Err(why) => return Some(CallResult::error(format!("Error: usage: {why}\nhint: tools/list shows each tool's arguments\n"))),
        };
        let mut command = vec![OsString::from(self.tool.name)];
        command.extend(argv.into_iter().map(OsString::from));
        Some(self.run(command))
    }
}

impl Tools<'_> {
    fn run(&mut self, args: Vec<OsString>) -> CallResult {
        let request = match Request::new(self.config, args) {
            Ok(r) => r,
            Err(e) => {
                return CallResult::error(format!(
                    "Error: internal: cannot read the working directory: {e}\nhint: restart the server from a directory that exists\n"
                ));
            }
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = match daemon::call(self.config, &request, &mut out, &mut err) {
            Some(code) => code,
            None => {
                let rex = self.local.get_or_insert_with(|| Rex::new(self.tool));
                match rex.run(&request, &mut Io::new(&mut out, &mut err, Cancel::default())) {
                    Outcome::Exit(exit) => exit.code(),
                    Outcome::Decline(why) => return CallResult::error(format!("Error: internal: {why}\nhint: call the tool again\n")),
                }
            }
        };
        let mut text = String::from_utf8_lossy(&out).into_owned();
        // Progress is for a person watching a terminal; the call's answer
        // comes whole.
        let err: String = String::from_utf8_lossy(&err).split_inclusive('\n').filter(|l| !l.starts_with("# progress: ")).collect();
        if !err.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&err);
        }
        if text.is_empty() {
            text.push_str("(no output)");
        }
        CallResult { text, is_error: code != 0 }
    }
}

/// The command line a tool call stands for, after the program name; `None`
/// for a tool there is not, `Err` for arguments that do not fit it. `--`
/// keeps a target or code that starts with `-` from reading as a flag.
fn argv(name: &str, args: &Map<String, Value>) -> Option<Result<Vec<String>, String>> {
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    let need = |key: &str| text(key).map(str::to_owned).ok_or_else(|| format!("{name} needs `{key}`, a string"));
    let run = || -> Result<Vec<String>, String> {
        let mut argv = vec![name.to_owned(), "--agent".to_owned()];
        match name {
            "exec" => {
                if let Some(t) = args.get("timeout") {
                    let t = t.as_u64().ok_or("exec's `timeout` is a number of seconds, 0 or more")?;
                    argv.push(format!("--timeout={t}"));
                }
                argv.push(format!("--code={}", need("code")?));
                argv.extend(["--".to_owned(), need("target")?]);
            }
            "save" => {
                if let Some(path) = text("path") {
                    argv.push(format!("--to={path}"));
                }
                if args.get("force").and_then(Value::as_bool) == Some(true) {
                    argv.push("--force".to_owned());
                }
                argv.extend(["--".to_owned(), need("target")?]);
            }
            "close" => argv.extend(["--".to_owned(), need("target")?]),
            "ref" => {
                argv.push("--".to_owned());
                argv.extend(need("query")?.split_whitespace().map(str::to_owned));
            }
            "list" => {}
            _ => unreachable!("only known tools get here"),
        }
        Ok(argv)
    };
    matches!(name, "exec" | "ref" | "save" | "close" | "list").then(run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(v: Value) -> Map<String, Value> {
        let Value::Object(map) = v else { panic!("arguments are an object") };
        map
    }

    #[test]
    fn each_tool_is_one_command_line() {
        let exec = argv("exec", &args(json!({"target": "-odd.bin", "code": "-1\nprint(2)", "timeout": 5}))).unwrap().unwrap();
        assert_eq!(exec, ["exec", "--agent", "--timeout=5", "--code=-1\nprint(2)", "--", "-odd.bin"]);
        let save = argv("save", &args(json!({"target": "t", "path": "out.i64", "force": true}))).unwrap().unwrap();
        assert_eq!(save, ["save", "--agent", "--to=out.i64", "--force", "--", "t"]);
        assert_eq!(argv("ref", &args(json!({"query": "xrefs  to"}))).unwrap().unwrap(), ["ref", "--agent", "--", "xrefs", "to"]);
        assert_eq!(argv("list", &Map::new()).unwrap().unwrap(), ["list", "--agent"]);
        assert!(argv("open", &Map::new()).is_none(), "not a tool");
        let missing = argv("exec", &args(json!({"target": "t"}))).unwrap().unwrap_err();
        assert!(missing.contains("`code`"), "{missing}");
        assert!(argv("exec", &args(json!({"target": "t", "code": "1", "timeout": -1}))).unwrap().is_err());
    }

    #[test]
    fn tool_names_have_no_dots() {
        let config = crate::tool::tests::TOOL.config();
        let tools = Tools { tool: &crate::tool::tests::TOOL, config: &config, local: None };
        let names: Vec<String> = mcp::Handler::tools(&tools).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["exec", "ref", "save", "close", "list"]);
    }
}
