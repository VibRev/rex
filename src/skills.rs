//! `<tool> skills`: the bundled agent skill, printed whole (CONVENTIONS §5;
//! AX §9). The agent installs it where its harness loads skills; the tool
//! keeps no table of harness directories. Runs in the client: it needs no
//! session, daemon or license, and is never cut to an agent's window.

use crate::cli::SkillsCommand;
use crate::tool::Tool;
use axkit::exit::{ErrorKind, Exit, Failure};
use std::io::Write;

/// Run `command` (`None`: `show`) for `tool`, writing to `out` (stdout). A
/// reader that goes away early (`| head`) is not an error.
///
/// # Errors
///
/// `show` names a file the skill does not have.
pub fn run(tool: &Tool, command: Option<&SkillsCommand>, out: &mut impl Write) -> Result<Exit, Failure> {
    let skill = tool.skill();
    match command {
        None => {
            let _ = out.write_all(skill.skill_md().as_bytes());
        }
        Some(SkillsCommand::Show { file }) => {
            let found = skill.file(file).ok_or_else(|| {
                Failure::new(ErrorKind::NotFound, format!("the {} skill has no file {file}", tool.name))
                    .with_hint(format!("{} skills list", tool.name))
            })?;
            let _ = out.write_all(found.text.as_bytes());
        }
        Some(SkillsCommand::List) => {
            let _ = writeln!(out, "# path | bytes");
            for file in skill.files {
                let _ = writeln!(out, "{} | {}", file.path, file.text.len());
            }
        }
    }
    Ok(Exit::Success)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use crate::tool::tests::TOOL;

    fn skills(args: &[&str]) -> Option<SkillsCommand> {
        let args = ["tx", "skills"].iter().chain(args).copied();
        match Cli::parse_for(&TOOL, args).unwrap().command {
            Command::Skills { command } => command,
            other => panic!("{other:?}"),
        }
    }

    fn output(args: &[&str]) -> Result<String, Failure> {
        let mut out = Vec::new();
        run(&TOOL, skills(args).as_ref(), &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn skills_prints_skill_md_as_bundled() {
        let skill_md = TOOL.skill().skill_md();
        assert_eq!(output(&[]).unwrap(), skill_md);
        assert_eq!(output(&["show"]).unwrap(), skill_md);
        assert_eq!(output(&["show", "SKILL.md"]).unwrap(), skill_md);
        assert_eq!(TOOL.skill().version(), Some(TOOL.version), "an agent compares it with `tx --version`");
    }

    #[test]
    fn other_files_by_path_and_a_missing_one_points_at_the_list() {
        assert_eq!(output(&["list"]).unwrap(), format!("# path | bytes\nSKILL.md | {}\n", TOOL.skill().skill_md().len()));
        let f = output(&["show", "nope.md"]).unwrap_err();
        assert_eq!(f.kind, ErrorKind::NotFound);
        assert_eq!(f.hint.as_deref(), Some("tx skills list"));
    }

    #[test]
    fn the_help_says_how_to_get_the_skill() {
        let help = Cli::command_for(&TOOL).render_long_help().to_string();
        assert!(help.contains("`tx skills` prints the agent skill (SKILL.md); install it where your harness loads skills."), "{help}");
    }
}
