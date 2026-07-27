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
             `omgb group new \"<name>\" --count <n> --model <provider-id> --names <n1,n2,...> --roles <r1,r2,...> [--human-name <host-name>]`\n\
             `<provider-id>` may be a configured provider id (e.g. xai, openai, anthropic) or a known model name.\n\
             `--human-name` (or `-H`) sets the creator/host display name; it defaults to `$USER`.\n\
             Add `--yolo` only if the user wants to auto-approve tool use.\n\n\
             Report the group id and the invite link. Tokens are sensitive; do not include them in logs.\n\
             The creator hosts the chat with: `omgb group chat <id> --name <host-name>`. A saved membership is used automatically; pass `--token <member-token>` only if the saved membership is missing.\n\
             Other members post with: `omgb group send <id> \"<message>\" --name <name>`. A saved membership is used automatically; otherwise pass `--token <member-token>`.\n\
             Others request to join with the **invite token** from `omgb group invite <id>`: `omgb group join <id> --token <invite-token> --name <your-name> [--remote <url>]`.\n\
             If the join is pending, poll with `omgb group join-status <id> <request_id>`. If the pending request was not saved locally, pass `--remote <url>` and `--pre-auth <token>` together.\n\
             An existing member approves with their **member token**: `omgb group approve <id> <request_id> --token <member-token> [--remote <url>]`.\n\
             To add a remote agent, run `omgb group remote-agent-add <id> <name> --url <callback-url> [--token <agent-token>]` on the group machine (retrieve a generated token with `omgb group remote-agent-token <id> <name>`).\n\
             Register the hosting side with `omgb group host-agent <id> <name> [--token <agent-token>]` on the remote machine (retrieve a generated token with `omgb group hosted-agent-token <id> <name>`).\n\
             Agents reply only when addressed (@name), when the topic matches their role, or when they have a relevant update; they can @mention each other for direct follow-ups."
                .to_string()
        } else {
            format!(
                "Run the appropriate `omgb group` command using `run_terminal_cmd` for: {trimmed}\n\n\
                 Use the **member token** for chat/send/approve and the **invite token** from `omgb group invite <id>` for join requests. \
                 Add `--remote <url>` for remote groups. \
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
