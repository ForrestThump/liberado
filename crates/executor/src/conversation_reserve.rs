//! Conversation wrap-up reserve: one tool-free turn after the work budget.

use liberado_provider::{CompletionResponse, Message, ToolDef};

use crate::loop_guard::RunPolicy;
use crate::{
    CONVERSATION_WRAP_UP_TURNS, ExecError, Mode, SUBMIT_REPORT_TOOL, WRAP_UP_TURNS,
    conversation_wrap_up_directive, wrap_up_directive,
};

/// Inclusive last turn index once the one-turn response reserve is included.
pub(crate) fn conversation_final_turn(max_turns: u32) -> u32 {
    max_turns.saturating_add(CONVERSATION_WRAP_UP_TURNS)
}

/// Withdraw tools and ask for a spoken summary once the work budget is spent.
pub(crate) fn enter_if_exhausted(
    turn: u32,
    max_turns: u32,
    tools: &mut Vec<ToolDef>,
    messages: &mut Vec<Message>,
) {
    if turn <= max_turns {
        return;
    }
    tools.clear();
    messages.push(Message::user(conversation_wrap_up_directive("turns")));
    tracing::warn!(
        turn,
        "conversation work budget exhausted; granting one tool-free response turn"
    );
}

/// Refuse tool calls made during the response-only reserve turn.
pub(crate) fn refuse_tools_in_reserve(
    turn: u32,
    max_turns: u32,
    response: &CompletionResponse,
    messages: &mut Vec<Message>,
) -> Result<(), ExecError> {
    if turn <= max_turns {
        return Ok(());
    }
    for call in &response.tool_calls {
        messages.push(Message::tool_result(
            &call.id,
            "Tool call refused: this turn was reserved for the final response.",
        ));
    }
    Err(ExecError::BudgetExceeded {
        resource: "turns",
        turns: max_turns,
    })
}

/// Grant the spoken-summary reserve for a conversational run that just exhausted its work budget.
/// Returns true when the caller should continue the loop instead of failing the turn.
pub(crate) fn grant_report_or_spoken_reserve(
    wrapping_up: &mut bool,
    max_turns: &mut u32,
    tools: &mut Vec<ToolDef>,
    messages: &mut Vec<Message>,
    turn: u32,
    name: &str,
    mode: Mode,
) -> bool {
    if *wrapping_up || !matches!(mode, Mode::Conversational) {
        return false;
    }
    *wrapping_up = true;
    *max_turns = turn;
    tools.clear();
    messages.push(Message::user(conversation_wrap_up_directive(name)));
    tracing::warn!(
        turn,
        resource = name,
        "conversation work budget exhausted; granting one tool-free response turn"
    );
    true
}

/// Remaining exhaustion policy after the resource name is known.
#[allow(clippy::too_many_arguments)]
pub(crate) fn after_named_exhaustion(
    wrapping_up: &mut bool,
    max_turns: &mut u32,
    tools: &mut Vec<ToolDef>,
    messages: &mut Vec<Message>,
    turn: u32,
    name: &'static str,
    mode: Mode,
    policy: &RunPolicy,
) -> Option<&'static str> {
    if grant_report_or_spoken_reserve(wrapping_up, max_turns, tools, messages, turn, name, mode) {
        return None;
    }
    if *wrapping_up || !policy.salvageable || !matches!(mode, Mode::Report) {
        return Some(name);
    }
    *wrapping_up = true;
    *max_turns = turn + WRAP_UP_TURNS - 1;
    tools.retain(|t| t.name == SUBMIT_REPORT_TOOL);
    messages.push(Message::user(wrap_up_directive(name, WRAP_UP_TURNS)));
    tracing::warn!(
        turn,
        resource = name,
        reserve = WRAP_UP_TURNS,
        "budget exhausted on salvageable work; granting wrap-up reserve to file a \
         partial report"
    );
    None
}

impl crate::Executor {
    /// `Ok(true)` means the stream finished with prose. `Ok(false)` means tools ran and the loop continues.
    pub(crate) async fn finish_stream_turn(
        &self,
        turn: u32,
        runtime: &dyn crate::ToolRuntime,
        response: &CompletionResponse,
        events: &tokio::sync::mpsc::Sender<crate::AgentEvent>,
        messages: &mut Vec<Message>,
    ) -> Result<bool, ExecError> {
        if response.tool_calls.is_empty() {
            self.mvl_end("succeeded", "model finished");
            return Ok(true);
        }
        if let Err(error) = refuse_tools_in_reserve(turn, self.budget.max_turns, response, messages)
        {
            self.mvl_end("failed", "model called tools during response-only reserve");
            return Err(error);
        }
        for call in &response.tool_calls {
            if let Some(call_id) = self
                .invoke_stream_tool(runtime, call, turn, events, messages)
                .await?
            {
                return Err(ExecError::AwaitingHuman { call_id });
            }
        }
        tracing::info!(turn, tools = response.tool_calls.len(), "turn used tools");
        Ok(false)
    }
}
