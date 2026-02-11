use crate::protocol::Event;

pub trait EventStore {
    fn append(&mut self, event: Event);
    fn events(&self) -> &[Event];
    fn events_for_submission(&self, submission_id: &str) -> Vec<Event>;
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryEventStore {
    events: Vec<Event>,
}

impl EventStore for InMemoryEventStore {
    fn append(&mut self, event: Event) {
        self.events.push(event);
    }

    fn events(&self) -> &[Event] {
        &self.events
    }

    fn events_for_submission(&self, submission_id: &str) -> Vec<Event> {
        self.events
            .iter()
            .filter(|event| event.submission_id == submission_id)
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub submission_id: String,
    pub events: Vec<Event>,
}

pub fn reconstruct_submission(store: &dyn EventStore, submission_id: &str) -> SessionSnapshot {
    SessionSnapshot {
        submission_id: submission_id.to_string(),
        events: store.events_for_submission(submission_id),
    }
}
