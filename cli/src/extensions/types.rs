use nabla::protocol::{Event, StopFacts, StopReason};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextAction {
    Continue { input: String },
    AskHumanMessage { message: String },
    Stop,
}

#[derive(Debug, Clone)]
pub struct TurnContext {
    pub submission_id: String,
    pub stop_reason: StopReason,
    pub stop_facts: StopFacts,
    pub events: Vec<Event>,
}

pub trait CliExtension {
    fn name(&self) -> &'static str;

    fn priority(&self) -> i32 {
        0
    }

    fn on_turn_end(&mut self, _context: &TurnContext) -> Result<(), String> {
        Ok(())
    }

    fn on_stop_facts(&mut self, _facts: &StopFacts) -> Result<(), String> {
        Ok(())
    }

    fn propose_next_action(
        &mut self,
        _context: &TurnContext,
    ) -> Result<Option<NextAction>, String> {
        Ok(None)
    }
}
