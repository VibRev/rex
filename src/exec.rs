//! Rendering one `exec` run (CONVENTIONS §4).
//!
//! The engine runs the script in its own interpreter and hands over what
//! came out, as the prelude's JSON answer; how that reaches stdout and
//! stderr is decided here, once for both engines.

use crate::analysis::Analysis;
use axkit::exit::{ErrorKind, Exit, Failure};
use axkit::output::Sink;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::io::Write;

/// What one script run produced: the prelude's answer as it writes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptRun {
    pub stdout: String,
    pub stderr: String,
    /// The last expression's value, as JSON.
    pub result: Option<Value>,
    pub error: Option<ScriptError>,
}

/// The exception a script ended with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptError {
    /// The exception type: `AttributeError`.
    pub kind: String,
    pub message: String,
    /// Where in the user's code: `<exec>:3`.
    pub location: Option<String>,
    /// Names close to a missing one.
    pub suggestions: Vec<String>,
    /// The next step the prelude knows of: facts about the binary when a
    /// name is not found.
    pub hint: Option<String>,
}

impl ScriptRun {
    /// Write the run, computed with the analysis in state `analysis`:
    /// the script's stderr to `err`; into `sink` the analysis line when
    /// incomplete, the script's stdout one record per line, then the result
    /// — or, when `json`, one object `{"stdout","result","analysis"}`; the
    /// error report last. A run that printed nothing and returned no value
    /// says `# no output`, so that a run that did nothing visible reads
    /// apart from a lost answer (§4); an empty list or object is shown as
    /// one (`[]`). Returns the exit status: the script's error, else success.
    pub fn render<W: Write>(&self, sink: &mut Sink<'_, W>, err: &mut dyn Write, json: bool, analysis: Analysis) -> Exit {
        let _ = err.write_all(self.stderr.as_bytes());
        if json {
            sink.json(&json!({ "stdout": self.stdout, "result": self.result, "analysis": analysis.to_json() }));
        } else {
            if let Some(line) = analysis.comment() {
                sink.comment(&line);
            }
            sink.lines(&self.stdout);
            if let Some(result) = &self.result {
                render_value(sink, result);
            }
            if self.error.is_none() && self.stdout.is_empty() && self.result.as_ref().is_none_or(Value::is_null) {
                sink.comment("# no output");
            }
        }
        match &self.error {
            Some(e) => {
                let failure = e.to_failure();
                let _ = failure.report(err, json);
                failure.exit()
            }
            None => Exit::Success,
        }
    }
}

impl ScriptError {
    /// The report: close names first, then the prelude's hint; without
    /// either, where to look (§6: every failure has a hint).
    pub fn to_failure(&self) -> Failure {
        let at = self.location.as_ref().map(|l| format!(" (at {l})")).unwrap_or_default();
        let mut hints = Vec::new();
        if !self.suggestions.is_empty() {
            hints.push(format!("did you mean: {}?", self.suggestions.join(", ")));
        }
        hints.extend(self.hint.clone());
        if hints.is_empty() {
            hints.push(match &self.location {
                Some(l) => format!("fix the code at {l}, which the traceback above quotes, and rerun"),
                None => "the traceback above has the details".to_owned(),
            });
        }
        Failure::new(ErrorKind::ScriptError, format!("{}: {}{at}", self.kind, self.message)).with_hint(hints.join("; "))
    }
}

/// A result as records: a string is one record as it is; an array, one
/// record per element (a table with a `#` header when the elements are
/// objects); an object, one `key | value` record per key.
fn render_value<W: Write>(sink: &mut Sink<'_, W>, value: &Value) {
    match value {
        Value::Null => {}
        Value::String(s) => sink.record(s),
        Value::Bool(_) | Value::Number(_) => sink.record(&value.to_string()),
        Value::Array(items) if items.is_empty() => sink.record("[]"),
        Value::Object(map) if map.is_empty() => sink.record("{}"),
        Value::Array(items) => match table_columns(items) {
            Some(columns) => {
                sink.comment(&format!("# {}", columns.join(" | ")));
                for item in items {
                    let row: Vec<String> = columns.iter().map(|c| cell(item.get(c.as_str()).unwrap_or(&Value::Null))).collect();
                    sink.record(&row.join(" | "));
                }
            }
            None => items.iter().for_each(|item| match item {
                Value::String(s) => sink.record(s),
                other => sink.record(&cell(other)),
            }),
        },
        Value::Object(map) => map.iter().for_each(|(k, v)| sink.record(&format!("{k} | {}", cell(v)))),
    }
}

/// The keys of an array of objects, in first-seen order; `None` unless every
/// element is an object.
fn table_columns(items: &[Value]) -> Option<Vec<String>> {
    if items.is_empty() {
        return None;
    }
    let mut columns: Vec<String> = Vec::new();
    for item in items {
        let map: &Map<String, Value> = item.as_object()?;
        for key in map.keys() {
            if !columns.contains(key) {
                columns.push(key.clone());
            }
        }
    }
    Some(columns)
}

/// One value inside a row: kept on one line, so a row stays one line.
fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.replace('\n', "\\n"),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axkit::output::Window;

    fn render(run: &ScriptRun, json: bool) -> (String, String, Exit) {
        render_with(run, json, Analysis::COMPLETE)
    }

    fn render_with(run: &ScriptRun, json: bool, analysis: Analysis) -> (String, String, Exit) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut sink = Sink::new(&mut out, Window::default(), None);
        let exit = run.render(&mut sink, &mut err, json, analysis);
        sink.finish().unwrap();
        (String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap(), exit)
    }

    #[test]
    fn prints_come_before_the_result() {
        let run = ScriptRun { stdout: "a\nb\n".into(), result: Some(json!(3)), ..ScriptRun::default() };
        assert_eq!(render(&run, false), ("a\nb\n3\n".into(), String::new(), Exit::Success));
    }

    #[test]
    fn a_string_result_is_one_record_as_it_is() {
        let code = "int main(void) {\n  return 0;\n}";
        let run = ScriptRun { result: Some(json!(code)), ..ScriptRun::default() };
        assert_eq!(render(&run, false).0, format!("{code}\n"));
    }

    #[test]
    fn an_array_of_objects_is_a_table() {
        let run = ScriptRun {
            result: Some(json!([{"addr": "0x401000", "name": "main"}, {"addr": "0x4013a0", "name": "parse\nhdr", "size": 640}])),
            ..ScriptRun::default()
        };
        assert_eq!(render(&run, false).0, "# addr | name | size\n0x401000 | main | \n0x4013a0 | parse\\nhdr | 640\n");
    }

    #[test]
    fn objects_and_mixed_arrays_are_one_record_per_entry() {
        let run = ScriptRun { result: Some(json!({"count": 2, "names": ["a", "b"]})), ..ScriptRun::default() };
        assert_eq!(render(&run, false).0, "count | 2\nnames | [\"a\",\"b\"]\n");
        let run = ScriptRun { result: Some(json!(["x", 1, null, {"k": 1}])), ..ScriptRun::default() };
        assert_eq!(render(&run, false).0, "x\n1\n\n{\"k\":1}\n");
        let run = ScriptRun { result: Some(json!([null])), ..ScriptRun::default() };
        assert_eq!(render(&run, false).0, "\n");
    }

    #[test]
    fn nothing_printed_and_nothing_returned_says_so() {
        for result in [None, Some(Value::Null)] {
            let run = ScriptRun { result, ..ScriptRun::default() };
            assert_eq!(render(&run, false).0, "# no output\n");
        }
        for (result, shown) in [(json!([]), "[]\n"), (json!({}), "{}\n")] {
            let run = ScriptRun { result: Some(result), ..ScriptRun::default() };
            assert_eq!(render(&run, false).0, shown, "an empty answer is an answer");
        }
        let printed = ScriptRun { stdout: "x\n".into(), ..ScriptRun::default() };
        assert_eq!(render(&printed, false).0, "x\n");
        let (out, _, _) = render(&ScriptRun { result: Some(json!([])), ..ScriptRun::default() }, true);
        assert!(serde_json::from_str::<Value>(&out).is_ok(), "JSON stays one object: {out}");
        let failed = ScriptRun { error: Some(ScriptError { kind: "E".into(), ..ScriptError::default() }), ..ScriptRun::default() };
        assert_eq!(render(&failed, false).0, "", "an error is not an empty answer");
    }

    #[test]
    fn every_script_error_has_a_hint() {
        let bare = ScriptError { kind: "ZeroDivisionError".into(), message: "division by zero".into(), ..ScriptError::default() };
        assert_eq!(bare.to_failure().hint.as_deref(), Some("the traceback above has the details"));
        let at = ScriptError { location: Some("<exec>:2".into()), ..bare };
        assert!(at.to_failure().hint.is_some_and(|h| h.contains("<exec>:2")));
        let lookup = ScriptError {
            kind: "LookupError".into(),
            suggestions: vec!["main_".into()],
            hint: Some("the entry point is start at 0x6c0".into()),
            ..ScriptError::default()
        };
        assert_eq!(lookup.to_failure().hint.as_deref(), Some("did you mean: main_?; the entry point is start at 0x6c0"));
    }

    #[test]
    fn json_mode_is_one_object_with_the_analysis() {
        let run = ScriptRun { stdout: "hi\n".into(), result: Some(json!([1, 2])), ..ScriptRun::default() };
        let (out, _, _) = render(&run, true);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, json!({"stdout": "hi\n", "result": [1, 2], "analysis": {"complete": true, "progress": 1.0}}));
    }

    #[test]
    fn an_answer_from_unfinished_analysis_says_so() {
        let run = ScriptRun { result: Some(json!(94)), ..ScriptRun::default() };
        let half = Analysis { complete: false, progress: Some(0.5) };
        assert_eq!(render_with(&run, false, half), ("# analysis incomplete (50%)\n94\n".into(), String::new(), Exit::Success));
        let (out, _, _) = render_with(&run, true, half);
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["analysis"], json!({"complete": false, "progress": 0.5}));
    }

    #[test]
    fn the_preludes_answer_reads_with_fields_missing() {
        let run: ScriptRun = serde_json::from_value(json!({"stdout": "a\n", "error": {"kind": "NameError", "message": "x"}})).unwrap();
        assert_eq!((run.result, run.stderr.as_str()), (None, ""));
        assert_eq!(run.error.unwrap().suggestions, Vec::<String>::new());
    }

    #[test]
    fn an_exception_is_a_script_error_after_the_scripts_own_stderr() {
        let run = ScriptRun {
            stdout: "partial\n".into(),
            stderr: "warning: x\n".into(),
            error: Some(ScriptError {
                kind: "AttributeError".into(),
                message: "'Functions' object has no attribute 'get_al'".into(),
                location: Some("<exec>:2".into()),
                suggestions: vec!["get_all".into(), "get_at".into()],
                hint: None,
            }),
            ..ScriptRun::default()
        };
        let (out, err, exit) = render(&run, false);
        assert_eq!(out, "partial\n");
        assert_eq!(
            err,
            "warning: x\nError: script_error: AttributeError: 'Functions' object has no attribute 'get_al' (at <exec>:2)\nhint: did you mean: get_all, get_at?\n"
        );
        assert_eq!(exit, Exit::Error);
    }
}
