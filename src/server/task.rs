//! Durable, domain-independent task state and wall-clock scheduling.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Running,
    Succeeded,
    Failed,
    Expired,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Task {
    pub key: String,
    pub kind: String,
    pub payload: Value,
    pub due_ms: i64,
    pub expires_ms: Option<i64>,
    pub group: Option<String>,
    #[serde(default)]
    pub priority: u8,
    #[serde(default)]
    pub preemptible: bool,
    pub max_attempts: Option<u32>,
    pub status: Status,
    pub attempts: u32,
    pub last_error: Option<String>,
}

impl Task {
    pub fn new(key: String, kind: &str, payload: Value, due_ms: i64) -> Self {
        Self {
            key,
            kind: kind.into(),
            payload,
            due_ms,
            expires_ms: None,
            group: None,
            priority: 10,
            preemptible: false,
            max_attempts: Some(10),
            status: Status::Pending,
            attempts: 0,
            last_error: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct State {
    pub version: u32,
    pub settings: Value,
    pub tasks: Vec<Task>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            settings: serde_json::json!({}),
            tasks: Vec::new(),
        }
    }
}

impl State {
    /// Recover work interrupted by termination. External effects are at-least-once;
    /// domain handlers must reconcile the remote state before retrying a mutation.
    pub fn recover(&mut self) {
        for task in &mut self.tasks {
            if task.status == Status::Running {
                task.status = Status::Pending;
                task.last_error = Some("Interrupted; recovered after restart".into());
            }
        }
    }

    pub fn enqueue(&mut self, task: Task) {
        if let Some(existing) = self.tasks.iter_mut().find(|t| t.key == task.key) {
            // Explicit submissions survive disabling an automatic policy.
            if task.group.is_none() {
                existing.group = None;
            }
            // Discovery can update future windows, but must not replay an outcome.
            if existing.status == Status::Pending {
                if existing.attempts == 0 {
                    existing.due_ms = task.due_ms;
                }
                existing.expires_ms = task.expires_ms;
                existing.payload = task.payload;
            }
        } else {
            self.tasks.push(task);
        }
    }

    pub fn cancel_group(&mut self, group: &str) {
        self.tasks.retain(|t| {
            t.group.as_deref() != Some(group)
                || !matches!(t.status, Status::Pending | Status::Running)
        });
    }

    pub fn expire(&mut self, now: i64) -> bool {
        let mut changed = false;
        for task in &mut self.tasks {
            if task.status == Status::Pending && task.expires_ms.is_some_and(|end| now >= end) {
                task.status = Status::Expired;
                task.last_error = Some("Execution window has passed".into());
                changed = true;
            }
        }
        changed
    }

    pub fn next_due(&self, now: i64) -> Option<usize> {
        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.status == Status::Pending && t.due_ms <= now)
            .min_by_key(|(_, t)| (std::cmp::Reverse(t.priority), t.due_ms))
            .map(|(index, _)| index)
    }

    /// Bound every relative wait, then read SystemTime again. Clock corrections
    /// and suspend/resume therefore affect dispatch without a stale long sleep.
    pub fn wait_duration(&self, now: i64) -> Duration {
        let millis = self
            .tasks
            .iter()
            .filter(|t| t.status == Status::Pending)
            .map(|t| t.due_ms.saturating_sub(now).clamp(1, 200))
            .min()
            .unwrap_or(200);
        Duration::from_millis(millis as u64)
    }
}

#[derive(Default)]
pub struct Outcome {
    pub tasks: Vec<Task>,
    /// Recurrence is measured from completion; actual due checks use wall time.
    pub repeat_after_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clock_changes_and_missed_windows() {
        let mut state = State::default();
        let mut task = Task::new("a".into(), "test", json!({}), 10_000);
        task.expires_ms = Some(12_000);
        state.enqueue(task);
        assert!(state.next_due(9_999).is_none());
        assert_eq!(state.next_due(10_000), Some(0));
        // A clock moving backwards never causes early dispatch.
        assert!(state.next_due(8_000).is_none());
        assert_eq!(state.wait_duration(0), Duration::from_millis(200));
        assert!(state.expire(12_000));
        assert_eq!(state.tasks[0].status, Status::Expired);
    }

    #[test]
    fn restart_and_deduplication() {
        let mut state = State::default();
        let mut task = Task::new("a".into(), "test", json!({}), 100);
        task.group = Some("auto".into());
        state.enqueue(task.clone());
        state.tasks[0].status = Status::Running;
        state.recover();
        assert_eq!(state.tasks[0].status, Status::Pending);
        state.tasks[0].status = Status::Succeeded;
        state.enqueue(task.clone());
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.tasks[0].status, Status::Succeeded);
        task.key = "b".into();
        state.enqueue(task.clone());
        task.group = None;
        state.enqueue(task);
        state.cancel_group("auto");
        assert!(state.tasks.iter().any(|t| t.key == "b"));
    }
}
