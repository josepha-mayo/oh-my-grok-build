//! `/create-workflow` -- omgb: turn a plain-English task into a saved workflow.

use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
use agent_client_protocol as acp;

/// Create a saved `omgb` workflow from a description.
pub struct CreateWorkflowCommand;

impl SlashCommand for CreateWorkflowCommand {
    fn name(&self) -> &str {
        "create-workflow"
    }

    fn description(&self) -> &str {
        "Create a saved omgb workflow from a task description"
    }

    fn usage(&self) -> &str {
        "/create-workflow <task>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn offered_when_session_less(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let mut name: Option<String> = None;
        let mut model: Option<String> = None;
        let mut yolo = false;
        let mut parts = Vec::new();
        let mut tokens = args.split_whitespace().peekable();
        while let Some(t) = tokens.next() {
            match t {
                "--name" | "-n" => {
                    name = tokens.next().map(|s| s.to_string());
                }
                "--model" | "-m" => {
                    model = tokens.next().map(|s| s.to_string());
                }
                "--yolo" | "-y" => {
                    yolo = true;
                }
                _ => parts.push(t),
            }
        }
        let task = parts.join(" ");
        if task.is_empty() {
            return CommandResult::Error(
                "describe the task, e.g. /create-workflow refactor auth".to_string(),
            );
        }
        let mut flags = String::new();
        if let Some(n) = name {
            flags.push_str(&format!(" --name {}", shell_quote(&n)));
        }
        if let Some(m) = model {
            flags.push_str(&format!(" --model {}", shell_quote(&m)));
        }
        if yolo {
            flags.push_str(" --yolo");
        }
        let quoted = shell_quote(&task);
        let instruction = format!(
            "Create a saved omgb workflow for this task by running `omgb workflow create {quoted}{flags}` and report the saved file path."
        );
        CommandResult::InjectSkill {
            display_text: format!("/create-workflow {task}{flags}"),
            prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(&instruction))],
            display_as_skill: false,
            scheduled_task_preview: None,
        }
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
