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
        let task = args.trim();
        if task.is_empty() {
            return CommandResult::Error(
                "describe the task, e.g. /create-workflow refactor auth".to_string(),
            );
        }
        let quoted = shell_quote(task);
        let instruction = format!(
            "Create a saved omgb workflow for this task by running `omgb workflow create {quoted}` and report the saved file path."
        );
        CommandResult::InjectSkill {
            display_text: format!("/create-workflow {task}"),
            prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(&instruction))],
            display_as_skill: false,
            scheduled_task_preview: None,
        }
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
