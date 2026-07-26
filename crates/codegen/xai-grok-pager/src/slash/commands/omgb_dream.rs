//! `/dream` -- omgb: pass through to the shell's memory consolidation command.

use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Run memory consolidation (`/dream`).
pub struct DreamCommand;

impl SlashCommand for DreamCommand {
    fn name(&self) -> &str {
        "dream"
    }

    fn description(&self) -> &str {
        "Run memory consolidation"
    }

    fn usage(&self) -> &str {
        "/dream"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn offered_when_session_less(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::PassThrough("/dream".to_string())
    }
}
