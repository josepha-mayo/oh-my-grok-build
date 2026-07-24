//! `/workflow` — run a saved omgb workflow from the pager.
//!
//! The command injects a prompt that instructs the agent to invoke the
//! `omgb workflow` CLI, so workflows can be launched without leaving the TUI.

use agent_client_protocol as acp;

use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct WorkflowCommand;

impl SlashCommand for WorkflowCommand {
    fn name(&self) -> &str {
        "workflow"
    }

    fn description(&self) -> &str {
        "Run a saved omgb workflow"
    }

    fn usage(&self) -> &str {
        "/workflow <name>|--file <path>|--list"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<name>|--file <path>|--list")
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return CommandResult::Error("workflow name or --file/--list required".to_string());
        }

        let (cli_args, display) = if trimmed == "--list" {
            ("list".to_string(), "/workflow --list".to_string())
        } else if trimmed.starts_with("--file ") {
            let path = trimmed.strip_prefix("--file ").unwrap_or(trimmed).trim();
            (
                format!("run --file {path}"),
                format!("/workflow --file {path}"),
            )
        } else {
            (format!("run {trimmed}"), format!("/workflow {trimmed}"))
        };

        let instruction = format!(
            "Run the omgb workflow using `omgb workflow {cli_args}` and report a concise summary of the result."
        );

        CommandResult::InjectSkill {
            display_text: display,
            prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(&instruction))],
            display_as_skill: false,
            scheduled_task_preview: None,
        }
    }
}
