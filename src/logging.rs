use std::{
    collections::VecDeque,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::MakeWriter;

const BUFFER_LINES: usize = 8_192;
const RETENTION: Duration = Duration::from_hours(14 * 24);
const MAX_FILES: usize = 20;

struct QueueState {
    queue: VecDeque<Vec<u8>>,
    closed: bool,
}

#[derive(Clone)]
pub struct QueueMakeWriter {
    state: Arc<(Mutex<QueueState>, Condvar)>,
}

pub struct EventWriter {
    state: Arc<(Mutex<QueueState>, Condvar)>,
    bytes: Vec<u8>,
}

pub struct LogWriter {
    writer: QueueMakeWriter,
    state: Arc<(Mutex<QueueState>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl LogWriter {
    pub fn writer(&self) -> QueueMakeWriter {
        self.writer.clone()
    }
}

impl<'a> MakeWriter<'a> for QueueMakeWriter {
    type Writer = EventWriter;

    fn make_writer(&'a self) -> Self::Writer {
        EventWriter {
            state: Arc::clone(&self.state),
            bytes: Vec::new(),
        }
    }
}

impl Write for EventWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for EventWriter {
    fn drop(&mut self) {
        if self.bytes.is_empty() {
            return;
        }
        let (lock, notify) = &*self.state;
        let Ok(mut state) = lock.lock() else { return };
        if state.closed {
            return;
        }
        if state.queue.len() == BUFFER_LINES {
            state.queue.pop_front();
        }
        state.queue.push_back(std::mem::take(&mut self.bytes));
        notify.notify_one();
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        let (lock, notify) = &*self.state;
        if let Ok(mut state) = lock.lock() {
            state.closed = true;
            notify.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn build_writer(directory: &Path, _log_level: &str) -> Result<LogWriter> {
    fs::create_dir_all(directory).with_context(|| {
        format!(
            "Failed to create logfile directory at {}",
            directory.display()
        )
    })?;
    expire_logs(directory)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("chester")
        .filename_suffix("log")
        .max_log_files(MAX_FILES)
        .build(directory)
        .context("Failed to create rolling logfile")?;

    let state = Arc::new((
        Mutex::new(QueueState {
            queue: VecDeque::new(),
            closed: false,
        }),
        Condvar::new(),
    ));
    let worker_state = Arc::clone(&state);
    let worker = thread::Builder::new()
        .name("chester-log-writer".into())
        .spawn(move || write_loop(worker_state.as_ref(), appender))
        .context("Failed to start logfile writer")?;
    Ok(LogWriter {
        writer: QueueMakeWriter {
            state: Arc::clone(&state),
        },
        state,
        worker: Some(worker),
    })
}

fn write_loop(state: &(Mutex<QueueState>, Condvar), mut appender: RollingFileAppender) {
    loop {
        let item = {
            let (lock, notify) = state;
            let Ok(mut state) = lock.lock() else {
                return;
            };
            while state.queue.is_empty() && !state.closed {
                state = match notify.wait(state) {
                    Ok(state) => state,
                    Err(_) => return,
                };
            }
            state.queue.pop_front()
        };
        match item {
            Some(item) => {
                let _ = appender.write_all(&item);
            }
            None => break,
        }
    }
    let _ = appender.flush();
}

fn expire_logs(directory: &Path) -> Result<()> {
    let cutoff = SystemTime::now()
        .checked_sub(RETENTION)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut files: Vec<(PathBuf, SystemTime)> = fs::read_dir(directory)
        .with_context(|| {
            format!(
                "Failed to read logfile directory at {}",
                directory.display()
            )
        })?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            is_chester_log(&path).then_some(path)
        })
        .filter_map(|path| Some((path.clone(), fs::metadata(path).ok()?.modified().ok()?)))
        .collect();
    files.sort_by_key(|(_, modified)| *modified);
    for (index, (path, modified)) in files.iter().enumerate() {
        if *modified < cutoff || files.len().saturating_sub(index) > MAX_FILES {
            fs::remove_file(path)
                .with_context(|| format!("Failed to expire logfile {}", path.display()))?;
        }
    }
    Ok(())
}

fn is_chester_log(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with("chester.")
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("log"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tracing_subscriber::fmt::MakeWriter;

    #[test]
    fn queue_discards_oldest_event_when_full() -> anyhow::Result<()> {
        let state = Arc::new((
            Mutex::new(QueueState {
                queue: VecDeque::new(),
                closed: false,
            }),
            Condvar::new(),
        ));
        let writer = QueueMakeWriter {
            state: Arc::clone(&state),
        };
        for index in 0..=BUFFER_LINES {
            let mut event = writer.make_writer();
            write!(event, "{index}")?;
        }

        let guard = state
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("test queue mutex poisoned"))?;
        let queue = &guard.queue;
        assert_eq!(queue.len(), BUFFER_LINES);
        assert_eq!(
            queue
                .front()
                .ok_or_else(|| anyhow::anyhow!("test queue unexpectedly empty"))?,
            b"1"
        );
        assert_eq!(
            queue
                .back()
                .ok_or_else(|| anyhow::anyhow!("test queue unexpectedly empty"))?,
            BUFFER_LINES.to_string().as_bytes()
        );
        Ok(())
    }

    #[test]
    fn identifies_chester_logs_with_case_insensitive_extensions() {
        assert!(is_chester_log(Path::new("chester.2026-09-11.log")));
        assert!(is_chester_log(Path::new("chester.2026-09-11.LOG")));
        assert!(is_chester_log(Path::new("chester.2026-09-11.LoG")));
        assert!(!is_chester_log(Path::new("other.2026-09-11.log")));
        assert!(!is_chester_log(Path::new("chester.2026-09-11.txt")));
    }
}
