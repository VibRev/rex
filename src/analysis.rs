//! How far the engine's analysis has got (CONVENTIONS §2). A global count
//! taken before analysis settles is well-formed, smaller and wrong, so every
//! such answer says how complete it is.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub complete: bool,
    /// 0.0–1.0, when the engine can tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
}

impl Analysis {
    pub const COMPLETE: Analysis = Analysis { complete: true, progress: Some(1.0) };
    /// Not finished, and how far it is unknown.
    pub const INCOMPLETE: Analysis = Analysis { complete: false, progress: None };

    /// The first line of a text answer; `None` once complete.
    pub fn comment(&self) -> Option<String> {
        if self.complete {
            return None;
        }
        Some(match self.progress {
            Some(p) => format!("# analysis incomplete ({:.0}%)", p * 100.0),
            None => "# analysis incomplete".to_owned(),
        })
    }

    /// The `analysis` field of a JSON answer.
    pub fn to_json(&self) -> Value {
        match self.progress {
            Some(p) => json!({ "complete": self.complete, "progress": p }),
            None => json!({ "complete": self.complete }),
        }
    }

    /// One table cell: `complete`, `62%`, or `incomplete`; `phase` is what
    /// the engine says it is doing.
    pub fn cell(&self, phase: Option<&str>) -> String {
        match (self.complete, self.progress, phase) {
            (true, _, _) => "complete".to_owned(),
            (false, Some(p), Some(phase)) => format!("{:.0}% ({phase})", p * 100.0),
            (false, Some(p), None) => format!("{:.0}%", p * 100.0),
            (false, None, Some(phase)) => phase.to_owned(),
            (false, None, None) => "incomplete".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_analysis_is_said() {
        let a = Analysis { complete: false, progress: Some(0.62) };
        assert_eq!(a.comment().as_deref(), Some("# analysis incomplete (62%)"));
        assert_eq!(a.to_json(), json!({"complete": false, "progress": 0.62}));
        assert_eq!(Analysis::INCOMPLETE.comment().as_deref(), Some("# analysis incomplete"));
        assert_eq!(a.cell(Some("analyzing 1/2")), "62% (analyzing 1/2)");
        assert_eq!(Analysis::INCOMPLETE.cell(None), "incomplete");
    }

    #[test]
    fn the_wire_form_is_the_json_field() {
        let a = Analysis { complete: false, progress: Some(0.5) };
        assert_eq!(serde_json::to_value(a).unwrap(), a.to_json());
        assert_eq!(serde_json::to_value(Analysis::INCOMPLETE).unwrap(), Analysis::INCOMPLETE.to_json());
        assert_eq!(serde_json::from_value::<Analysis>(json!({"complete": true})).unwrap().progress, None);
    }

    #[test]
    fn complete_analysis_adds_nothing_to_the_text() {
        assert_eq!(Analysis::COMPLETE.comment(), None);
    }
}
