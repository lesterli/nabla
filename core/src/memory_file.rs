use std::{
    fs::{File, OpenOptions, create_dir_all},
    io::{self, BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use crate::{memory::EventStore, protocol::Event};

/// File-backed append-only event store.
///
/// Format: one JSON-serialized `Event` per line (JSONL).
/// Recovery behavior is deterministic: invalid/corrupt lines are skipped and counted.
pub struct FileEventStore {
    path: PathBuf,
    writer: BufWriter<File>,
    events: Vec<Event>,
    skipped_corrupt_lines: usize,
}

impl FileEventStore {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }

        let mut events = Vec::new();
        let mut skipped_corrupt_lines = 0usize;
        if path.exists() {
            let reader = BufReader::new(File::open(&path)?);
            for line in reader.lines() {
                let line = line?;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Event>(line) {
                    Ok(event) => events.push(event),
                    Err(_) => skipped_corrupt_lines += 1,
                }
            }
        }

        let writer = BufWriter::new(OpenOptions::new().create(true).append(true).open(&path)?);

        Ok(Self {
            path,
            writer,
            events,
            skipped_corrupt_lines,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn skipped_corrupt_lines(&self) -> usize {
        self.skipped_corrupt_lines
    }
}

impl EventStore for FileEventStore {
    fn append(&mut self, event: Event) {
        let serialized =
            serde_json::to_string(&event).expect("serializing event to json must succeed");
        self.writer
            .write_all(serialized.as_bytes())
            .expect("writing event to persistent store must succeed");
        self.writer
            .write_all(b"\n")
            .expect("writing newline to persistent store must succeed");
        self.writer
            .flush()
            .expect("flushing persistent store must succeed");
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::{
        memory::{EventStore, reconstruct_submission},
        protocol::{Event, EventKind, StopReason},
    };

    use super::FileEventStore;

    fn temp_store_path(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir()
            .join("agent-core-file-store-tests")
            .join(format!("{test_name}-{nanos}-{}.jsonl", std::process::id()))
    }

    #[test]
    fn append_restart_replay_preserves_event_order_and_index() {
        let path = temp_store_path("replay");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create temp dir");
        }

        {
            let mut store = FileEventStore::open(&path).expect("open file store");
            store.append(Event::new(
                "submission-a".to_string(),
                0,
                EventKind::UserInput {
                    input: "hello".to_string(),
                },
            ));
            store.append(Event::new(
                "submission-a".to_string(),
                1,
                EventKind::ContextBuilt { recent_events: 1 },
            ));
            store.append(Event::new(
                "submission-b".to_string(),
                2,
                EventKind::TurnStopped {
                    reason: StopReason::Done,
                    facts: None,
                },
            ));
            assert_eq!(store.last_event_index(), Some(2));
        }

        let reopened = FileEventStore::open(&path).expect("reopen file store");
        assert_eq!(reopened.events().len(), 3);
        assert_eq!(
            reopened
                .events()
                .iter()
                .map(|event| event.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        let snapshot = reconstruct_submission(&reopened, "submission-a");
        assert_eq!(snapshot.events.len(), 2);
        assert_eq!(snapshot.events[0].index, 0);
        assert_eq!(snapshot.events[1].index, 1);

        fs::remove_file(&path).expect("cleanup test file");
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn corrupt_lines_are_skipped_deterministically() {
        let path = temp_store_path("corrupt-lines");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create temp dir");
        }

        let valid_0 = serde_json::to_string(&Event::new(
            "submission-c".to_string(),
            0,
            EventKind::UserInput {
                input: "first".to_string(),
            },
        ))
        .expect("serialize valid event");
        let valid_1 = serde_json::to_string(&Event::new(
            "submission-c".to_string(),
            1,
            EventKind::TurnStopped {
                reason: StopReason::Done,
                facts: None,
            },
        ))
        .expect("serialize valid event");
        let file_content = format!("{valid_0}\n{{\"broken\":true\n{valid_1}\n");
        fs::write(&path, file_content).expect("write malformed store file");

        let reopened = FileEventStore::open(&path).expect("open file store with malformed line");
        assert_eq!(reopened.events().len(), 2);
        assert_eq!(reopened.skipped_corrupt_lines(), 1);
        assert_eq!(
            reopened
                .events()
                .iter()
                .map(|event| event.index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );

        fs::remove_file(&path).expect("cleanup test file");
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}
