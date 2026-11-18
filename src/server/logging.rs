//! Bounded JSON Lines logs owned by the daemon holding the instance lock.

use super::task::{Task, now_ms};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct Log(Arc<Mutex<Sink>>);

struct Sink {
    path: PathBuf,
    write_failed: bool,
}

impl Log {
    pub fn new(path: PathBuf) -> Self {
        Self(Arc::new(Mutex::new(Sink {
            path,
            write_failed: false,
        })))
    }

    pub fn event(&self, level: &str, name: &str, details: Value) {
        self.write(level, name, None, details);
    }

    pub fn task(&self, level: &str, name: &str, task: &Task, details: Value) {
        self.write(level, name, Some(task_fields(task)), details);
    }

    fn write(&self, level: &str, name: &str, task: Option<Value>, details: Value) {
        let mut entry = json!({
            "timestamp_ms": now_ms(), "level": level, "event": name, "details": details
        });
        if let Some(task) = task {
            entry["task"] = task;
        }
        let mut bytes = entry.to_string().into_bytes();
        bytes.push(b'\n');
        let mut sink = self.0.lock().unwrap_or_else(|error| error.into_inner());
        match append(&sink.path, &bytes, MAX_BYTES) {
            Ok(()) => sink.write_failed = false,
            Err(error) if !sink.write_failed => {
                // Report once until writes recover. A log failure must not
                // crash the scheduler or flood the process diagnostics.
                let _ = writeln!(io::stderr().lock(), "Failed to write server log: {error}");
                sink.write_failed = true;
            }
            Err(_) => {}
        }
    }

    pub fn in_task<F: Future>(
        &self,
        task: &Task,
        future: F,
    ) -> impl Future<Output = F::Output> + use<F> {
        TASK_LOG.scope((self.clone(), task_fields(task)), future)
    }
}

fn task_fields(task: &Task) -> Value {
    json!({"key": task.key, "kind": task.kind, "attempt": task.attempts})
}

tokio::task_local! {
    static TASK_LOG: (Log, Value);
}

/// Use only known-safe fields. Never forward a request body, response or raw API error.
pub fn task_event(level: &str, name: &str, details: Value) {
    let _ = TASK_LOG.try_with(|(log, task)| log.write(level, name, Some(task.clone()), details));
}

pub(super) fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn append(path: &Path, bytes: &[u8], max_bytes: u64) -> io::Result<()> {
    if let Some(length) = fs::metadata(path).ok().map(|m| m.len())
        && length > 0
        && length.saturating_add(bytes.len() as u64) > max_bytes
    {
        let previous = path.with_extension("log.1");
        match fs::remove_file(&previous) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // No open handles owned by the daemon are left on the file between events.
        fs::rename(path, previous)?;
    }
    open_append(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tests::TestDir;

    #[test]
    fn rotation_keeps_only_the_current_and_previous_log() {
        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        let path = temp.0.join("server.log");
        for entry in [b"first\n".as_slice(), b"second\n", b"third\n"] {
            append(&path, entry, 10).unwrap();
        }
        assert_eq!(fs::read(path).unwrap(), b"third\n");
        assert_eq!(fs::read(temp.0.join("server.log.1")).unwrap(), b"second\n");
    }
}
