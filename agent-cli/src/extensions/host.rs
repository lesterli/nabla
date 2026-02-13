use std::panic::{AssertUnwindSafe, catch_unwind};

use super::types::{CliExtension, NextAction, TurnContext};

#[derive(Debug, Clone)]
struct ActionProposal {
    extension_name: &'static str,
    priority: i32,
    action: NextAction,
}

pub struct ExtensionHost {
    extensions: Vec<Box<dyn CliExtension>>,
    diagnostics: Vec<String>,
    max_follow_up_turns: usize,
}

impl Default for ExtensionHost {
    fn default() -> Self {
        Self {
            extensions: Vec::new(),
            diagnostics: Vec::new(),
            max_follow_up_turns: 4,
        }
    }
}

impl ExtensionHost {
    pub fn add_extension(&mut self, extension: Box<dyn CliExtension>) {
        self.extensions.push(extension);
        self.sort_extensions();
    }

    pub fn set_max_follow_up_turns(&mut self, max_follow_up_turns: usize) {
        self.max_follow_up_turns = max_follow_up_turns;
    }

    pub fn max_follow_up_turns(&self) -> usize {
        self.max_follow_up_turns
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub fn record_diagnostic(&mut self, message: impl Into<String>) {
        self.diagnostics.push(message.into());
    }

    pub fn process_turn(&mut self, context: &TurnContext) -> Option<NextAction> {
        let mut proposals = Vec::new();

        for extension in &mut self.extensions {
            let extension_name = extension.name();
            if let Err(err) = safe_invoke(extension_name, "on_turn_end", || {
                extension.on_turn_end(context)
            }) {
                self.diagnostics.push(format!(
                    "{err} [submission_id={}, stop_reason={:?}, events={}]",
                    context.submission_id,
                    context.stop_reason,
                    context.events.len()
                ));
                continue;
            }
            if let Err(err) = safe_invoke(extension_name, "on_stop_facts", || {
                extension.on_stop_facts(&context.stop_facts)
            }) {
                self.diagnostics.push(format!(
                    "{err} [submission_id={}, stop_reason={:?}, events={}]",
                    context.submission_id,
                    context.stop_reason,
                    context.events.len()
                ));
                continue;
            }
            match safe_invoke(extension_name, "propose_next_action", || {
                extension.propose_next_action(context)
            }) {
                Ok(Some(action)) => proposals.push(ActionProposal {
                    extension_name,
                    priority: extension.priority(),
                    action,
                }),
                Ok(None) => {}
                Err(err) => self.diagnostics.push(format!(
                    "{err} [submission_id={}, stop_reason={:?}, events={}]",
                    context.submission_id,
                    context.stop_reason,
                    context.events.len()
                )),
            }
        }

        merge_proposals(&mut proposals, &mut self.diagnostics)
    }

    fn sort_extensions(&mut self) {
        self.extensions.sort_by(|left, right| {
            right
                .priority()
                .cmp(&left.priority())
                .then_with(|| left.name().cmp(right.name()))
        });
    }
}

fn safe_invoke<T>(
    extension_name: &'static str,
    hook_name: &'static str,
    invoke: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let result = catch_unwind(AssertUnwindSafe(invoke));
    match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(format!(
            "extension `{extension_name}` failed at `{hook_name}`: {err}"
        )),
        Err(_) => Err(format!(
            "extension `{extension_name}` panicked at `{hook_name}`"
        )),
    }
}

fn merge_proposals(
    proposals: &mut Vec<ActionProposal>,
    diagnostics: &mut Vec<String>,
) -> Option<NextAction> {
    if proposals.is_empty() {
        return None;
    }

    proposals.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.extension_name.cmp(right.extension_name))
    });

    let winner = proposals.remove(0);
    for proposal in proposals {
        if proposal.action != winner.action {
            diagnostics.push(format!(
                "extension action conflict: chose `{}` from `{}` (priority {}), ignored `{}` from `{}` (priority {})",
                describe_action(&winner.action),
                winner.extension_name,
                winner.priority,
                describe_action(&proposal.action),
                proposal.extension_name,
                proposal.priority
            ));
        }
    }

    Some(winner.action)
}

fn describe_action(action: &NextAction) -> &'static str {
    match action {
        NextAction::Continue { .. } => "continue",
        NextAction::AskHumanMessage { .. } => "ask_human_message",
        NextAction::Stop => "stop",
    }
}
