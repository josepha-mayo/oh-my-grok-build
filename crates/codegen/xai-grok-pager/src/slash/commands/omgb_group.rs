//! `/group` -- omgb: manage multi-agent group chats via the `omgb group` CLI.

use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
use agent_client_protocol as acp;

/// Manage omgb multi-agent group chats.
pub struct GroupCommand;

impl SlashCommand for GroupCommand {
    fn name(&self) -> &str {
        "group"
    }

    fn description(&self) -> &str {
        "Create or join an omgb multi-agent group chat"
    }

    fn usage(&self) -> &str {
        "/group [args]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn offered_when_session_less(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        let instruction = if trimmed.is_empty() {
            "Start an omgb multi-agent group chat.\n\n\
             Ask the user: how many agents (2-20), which provider/model to use, and each agent's name and role.\n\
             Then use `run_terminal_cmd` to execute:\n\
             `omgb group new \"<name>\" --count <n> --model <provider-id> --names <n1,n2,...> --roles <r1,r2,...>`\n\
             `<provider-id>` may be a configured provider id (e.g. xai, openai, anthropic) or a known model name.\n\
             Add `--yolo` only if the user wants to auto-approve tool use.\n\n\
             Report the group id, the invite link, and how to host/join/approve.\n\
             To host the chat, run `omgb group chat <id> --token <token> [--human-name <name>]`.\n\
             Others request to join with `omgb group join <id> --token <token> --name <your-name> [--remote <url>]`.\n\
             An existing member approves with `omgb group approve <id> <request_id> [--token <token>] [--remote <url>]`.\n\
             Members post with `omgb group send <id> \"<message>\" --token <token> [--human-name <name>]`.\n\
             Agents reply only when addressed (@name), when the topic matches their role, or when they have a relevant update; they can @mention each other for direct follow-ups."
                .to_string()
        } else {
            format!(
                "Run the appropriate `omgb group` command using `run_terminal_cmd` for: {trimmed}\n\n\
                 Quote any shell arguments that contain spaces or special characters. Report the result, including the group id, invite link, or recent agent replies."
            )
        };
        CommandResult::InjectSkill {
            display_text: if trimmed.is_empty() {
                "/group".to_string()
            } else {
                format!("/group {trimmed}")
            },
            prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(&instruction))],
            display_as_skill: false,
            scheduled_task_preview: None,
        }
    }
}
