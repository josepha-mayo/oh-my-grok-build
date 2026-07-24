//! `/live` — start a live voice/text session.
//!
//! In the terminal this currently activates the voice dictation pipeline
//! (same as `/voice`), streaming microphone input to the prompt so the
//! user can speak naturally during a live session.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct LiveCommand;

impl SlashCommand for LiveCommand {
    fn name(&self) -> &str {
        "live"
    }

    fn description(&self) -> &str {
        "Start live voice input (Ctrl+Space/F8; Esc/Enter to stop)"
    }

    fn usage(&self) -> &str {
        "/live"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn offered_when_session_less(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::VoiceToggle)
    }
}
