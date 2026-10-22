use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, SecondsFormat, Utc};
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber, info, warn};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, reload};

use crate::error::{Error, Result};

/// How many lines a front end's log buffer keeps by default.
pub const DEFAULT_LOG_CAPACITY: usize = 5000;

/// Size at which a front end's log file is rotated.
pub const LOG_FILE_MAX_SIZE: u64 = 10 * 1024 * 1024;

/// Rotated log files kept next to the current one (`<file>.1` to `<file>.5`).
pub const LOG_FILES_KEPT: usize = 5;

/// Lines a slow subscriber may fall behind before it sees `RecvError::Lagged`.
const BROADCAST_CAPACITY: usize = 1024;

/// One formatted log event.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub timestamp: DateTime<Utc>,
    pub level: Level,
    /// The whole line: timestamp, level, message and fields.
    pub text: String,
}

/// A tracing layer that keeps the last log lines in a ring buffer and
/// broadcasts new ones. Clones share the same buffer.
#[derive(Clone)]
pub struct LogBuffer(Arc<Mutex<Ring>>);

struct Ring {
    lines: VecDeque<LogLine>,
    capacity: usize,
    tx: broadcast::Sender<LogLine>,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self(Arc::new(Mutex::new(Ring {
            lines: VecDeque::with_capacity(capacity),
            capacity,
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
        })))
    }

    /// The buffered lines, oldest first.
    pub fn snapshot(&self) -> Vec<LogLine> {
        self.ring().lines.iter().cloned().collect()
    }

    /// The buffered lines plus a receiver for every later line, without gap
    /// or overlap between the two.
    pub fn subscribe(&self) -> (Vec<LogLine>, broadcast::Receiver<LogLine>) {
        let ring = self.ring();
        (ring.lines.iter().cloned().collect(), ring.tx.subscribe())
    }

    fn ring(&self) -> std::sync::MutexGuard<'_, Ring> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push(&self, line: LogLine) {
        let mut ring = self.ring();
        if ring.lines.len() == ring.capacity {
            ring.lines.pop_front();
        }
        // Sending fails only without subscribers
        let _ = ring.tx.send(line.clone());
        ring.lines.push_back(line);
    }
}

impl<S: Subscriber> Layer<S> for LogBuffer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut fields = FieldText::default();
        event.record(&mut fields);
        let timestamp = Utc::now();
        let text = format!(
            "{} {:>5} {}{}",
            timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
            meta.level(),
            fields.message,
            fields.rest
        );
        self.push(LogLine {
            timestamp,
            level: *meta.level(),
            text,
        });
    }
}

/// Collects the message and ` key=value` pairs of an event.
#[derive(Default)]
struct FieldText {
    message: String,
    rest: String,
}

impl Visit for FieldText {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.rest, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }
}

/// Default log file of a front end: `~/Library/Logs/capycal/<name>.log` on
/// macOS, the local data directory elsewhere.
pub fn default_log_file(name: &str) -> Option<PathBuf> {
    let file = format!("{name}.log");
    if cfg!(target_os = "macos") {
        directories::BaseDirs::new().map(|d| d.home_dir().join("Library/Logs/capycal").join(file))
    } else {
        directories::ProjectDirs::from("", "", "capycal").map(|d| d.data_local_dir().join(file))
    }
}

/// A log file that is rotated at start and whenever it exceeds its size
/// limit: `<file>` becomes `<file>.1`, `<file>.1` becomes `<file>.2` and so
/// on, keeping `LOG_FILES_KEPT` rotated files. Clones share the same file.
#[derive(Clone)]
pub struct LogFile(Arc<Mutex<FileState>>);

struct FileState {
    path: PathBuf,
    file: File,
    size: u64,
    max_size: u64,
    /// Rotations since start: the files `<file>.1` to `<file>.<rotations>`
    /// belong to this session, as far as they are still kept.
    rotations: usize,
}

impl LogFile {
    /// Rotate an existing non-empty `path` away and start a fresh file.
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::with_max_size(path, LOG_FILE_MAX_SIZE)
    }

    fn with_max_size(path: &Path, max_size: u64) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if std::fs::metadata(path).is_ok_and(|m| m.len() > 0) {
            shift(path)?;
        }
        Ok(Self(Arc::new(Mutex::new(FileState {
            path: path.to_path_buf(),
            file: File::create(path)?,
            size: 0,
            max_size,
            rotations: 0,
        }))))
    }

    /// Everything written since start, oldest first, across rotations. If
    /// parts were already deleted, a first line says so.
    pub fn session_content(&self) -> io::Result<String> {
        // Holding the lock keeps the files from rotating while being read
        let state = self.state();
        let mut content = String::new();
        let lost = state.rotations.saturating_sub(LOG_FILES_KEPT);
        if lost > 0 {
            let _ = writeln!(
                content,
                "[{lost} older part(s) of this session's log were rotated away]"
            );
        }
        for i in (1..=state.rotations.min(LOG_FILES_KEPT)).rev() {
            match std::fs::read_to_string(numbered(&state.path, i)) {
                Ok(part) => content.push_str(&part),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        content.push_str(&std::fs::read_to_string(&state.path)?);
        Ok(content)
    }

    fn state(&self) -> MutexGuard<'_, FileState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl FileState {
    fn rotate(&mut self) -> io::Result<()> {
        shift(&self.path)?;
        self.rotations += 1;
        self.file = File::create(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

/// Move `path` to `path.1`, `path.1` to `path.2` and so on, dropping the oldest.
fn shift(path: &Path) -> io::Result<()> {
    ignore_missing(std::fs::remove_file(numbered(path, LOG_FILES_KEPT)))?;
    for i in (1..LOG_FILES_KEPT).rev() {
        ignore_missing(std::fs::rename(numbered(path, i), numbered(path, i + 1)))?;
    }
    std::fs::rename(path, numbered(path, 1))
}

fn numbered(path: &Path, i: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{i}"));
    name.into()
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = LogFileWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LogFileWriter(self.state())
    }
}

/// Writes one event to a `LogFile`, rotating it first if the event would
/// exceed the size limit.
pub struct LogFileWriter<'a>(MutexGuard<'a, FileState>);

impl io::Write for LogFileWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let state = &mut *self.0;
        if state.size > 0 && state.size + buf.len() as u64 > state.max_size {
            // On failure keep writing to the current file; logging the
            // error here would re-enter the logger
            let _ = state.rotate();
        }
        let written = state.file.write(buf)?;
        state.size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.file.flush()
    }
}

/// Changes the level of the installed logger, to apply `[daemon] log_level`
/// after a config reload. Does nothing if the level was fixed at start by
/// `RUST_LOG` or a command line flag, or for the default value.
#[derive(Default)]
pub struct LogLevel {
    handle: Option<reload::Handle<EnvFilter, Registry>>,
    current: String,
}

impl LogLevel {
    /// Switch to `level` if it differs from the current one.
    pub fn set(&mut self, level: &str) {
        let Some(handle) = &self.handle else {
            return;
        };
        if level == self.current {
            return;
        }
        match handle.reload(EnvFilter::new(level)) {
            Ok(()) => {
                info!(from = %self.current, to = level, "log level changed");
                self.current = level.to_string();
            }
            Err(e) => warn!(error = %e, "cannot change the log level"),
        }
    }
}

/// The global filter layer for `level`, and a handle to change it later.
/// `RUST_LOG` overrides `level` and, like `fixed`, keeps the handle from
/// changing it.
pub(crate) fn level_filter(
    level: &str,
    fixed: bool,
) -> (reload::Layer<EnvFilter, Registry>, LogLevel) {
    level_filter_with_env(level, fixed, EnvFilter::try_from_default_env().ok())
}

fn level_filter_with_env(
    level: &str,
    fixed: bool,
    env: Option<EnvFilter>,
) -> (reload::Layer<EnvFilter, Registry>, LogLevel) {
    let reloadable = env.is_none() && !fixed;
    let (layer, handle) = reload::Layer::new(env.unwrap_or_else(|| EnvFilter::new(level)));
    let log_level = LogLevel {
        handle: reloadable.then_some(handle),
        current: level.to_string(),
    };
    (layer, log_level)
}

/// Install the global logger of a front end: write to `log_file` (see
/// `LogFile`) and keep the last `capacity` lines in the returned buffer.
/// `RUST_LOG` overrides `level`. The CLI sets up its own stdout logging
/// instead.
pub fn init_with_buffer(
    level: &str,
    log_file: &Path,
    capacity: usize,
) -> Result<(LogBuffer, LogFile, LogLevel)> {
    let file = LogFile::open(log_file)
        .map_err(|e| Error::Logging(format!("{}: {e}", log_file.display())))?;
    let buffer = LogBuffer::new(capacity);
    let (filter, log_level) = level_filter(level, false);
    file_and_buffer_subscriber(filter, file.clone(), buffer.clone())
        .try_init()
        .map_err(|e| Error::Logging(e.to_string()))?;
    Ok((buffer, file, log_level))
}

fn file_and_buffer_subscriber(
    filter: reload::Layer<EnvFilter, Registry>,
    file: LogFile,
    buffer: LogBuffer,
) -> impl Subscriber + Send + Sync {
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .with_writer(file),
        )
        .with(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tracing::{debug, info, warn};
    use tracing_subscriber::Registry;

    fn with_buffer(buffer: &LogBuffer, f: impl FnOnce()) {
        tracing::subscriber::with_default(Registry::default().with(buffer.clone()), f);
    }

    #[test]
    fn test_lines_carry_level_target_message_and_fields() {
        let buffer = LogBuffer::new(10);
        with_buffer(&buffer, || {
            info!(sync = "a-to-b", count = 3, "cycle {}", "done");
            warn!(error = %"boom", "failed");
        });

        let lines = buffer.snapshot();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].level, Level::INFO);
        assert!(
            lines[0]
                .text
                .ends_with(" INFO cycle done sync=a-to-b count=3"),
            "{}",
            lines[0].text
        );
        assert_eq!(lines[1].level, Level::WARN);
        assert!(
            lines[1].text.ends_with("failed error=boom"),
            "{}",
            lines[1].text
        );
    }

    #[test]
    fn test_ring_buffer_keeps_newest_lines() {
        let buffer = LogBuffer::new(3);
        with_buffer(&buffer, || {
            for i in 0..5 {
                info!("line {i}");
            }
        });

        let lines = buffer.snapshot();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].text.ends_with("line 2"));
        assert!(lines[2].text.ends_with("line 4"));
    }

    #[test]
    fn test_subscriber_receives_new_lines() {
        let buffer = LogBuffer::new(10);
        with_buffer(&buffer, || info!("before"));
        let (backlog, mut rx) = buffer.subscribe();
        with_buffer(&buffer, || info!("after"));

        assert_eq!(backlog.len(), 1);
        assert!(backlog[0].text.ends_with("before"));
        assert!(rx.try_recv().unwrap().text.ends_with("after"));
        assert!(rx.try_recv().is_err());
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("capycal-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    fn write(file: &LogFile, text: &str) {
        file.make_writer().write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn test_file_and_buffer_subscriber_writes_both() {
        let dir = temp_dir("logging");
        let path = dir.join("logs").join("ui.log");
        let buffer = LogBuffer::new(10);
        let file = LogFile::open(&path).unwrap();

        let (filter, _) = level_filter_with_env("info", false, None);
        let subscriber = file_and_buffer_subscriber(filter, file, buffer.clone());
        tracing::subscriber::with_default(subscriber, || {
            info!(sync = "a", "written");
            debug!("filtered out");
        });

        assert_eq!(buffer.snapshot().len(), 1);
        let content = read(&path);
        assert!(content.contains("written sync=\"a\""), "{content}");
        assert!(!content.contains("filtered out"), "{content}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn messages(buffer: &LogBuffer) -> Vec<String> {
        buffer.snapshot().into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn test_log_level_changes_the_filter() {
        let buffer = LogBuffer::new(10);
        let (filter, mut level) = level_filter_with_env("info", false, None);
        let subscriber = Registry::default().with(filter).with(buffer.clone());
        tracing::subscriber::with_default(subscriber, || {
            debug!("hidden");
            level.set("info");
            level.set("debug");
            debug!("shown");
            level.set("warn");
            info!("hidden again");
        });

        let messages = messages(&buffer);
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0].ends_with("log level changed from=info to=debug"),
            "{messages:?}"
        );
        assert!(messages[1].ends_with("shown"), "{messages:?}");
    }

    #[test]
    fn test_log_level_fixed_by_rust_log_or_flag() {
        for (fixed, env) in [(true, None), (false, Some(EnvFilter::new("info")))] {
            let buffer = LogBuffer::new(10);
            let (filter, mut level) = level_filter_with_env("info", fixed, env);
            let subscriber = Registry::default().with(filter).with(buffer.clone());
            tracing::subscriber::with_default(subscriber, || {
                level.set("debug");
                debug!("hidden");
            });
            assert!(buffer.snapshot().is_empty(), "{:?}", messages(&buffer));
        }

        // RUST_LOG wins over the configured level
        let buffer = LogBuffer::new(10);
        let (filter, _) = level_filter_with_env("info", false, Some(EnvFilter::new("debug")));
        let subscriber = Registry::default().with(filter).with(buffer.clone());
        tracing::subscriber::with_default(subscriber, || debug!("shown"));
        assert_eq!(buffer.snapshot().len(), 1);
    }

    #[test]
    fn test_open_rotates_previous_sessions_and_keeps_five() {
        let dir = temp_dir("log-retention");
        let path = dir.join("ui.log");
        for session in 0..7 {
            let file = LogFile::open(&path).unwrap();
            write(&file, &format!("session {session}\n"));
        }

        assert_eq!(read(&path), "session 6\n");
        for i in 1..=LOG_FILES_KEPT {
            assert_eq!(read(&numbered(&path, i)), format!("session {}\n", 6 - i));
        }
        assert!(!numbered(&path, LOG_FILES_KEPT + 1).exists());

        // An empty file is reused instead of rotated
        drop(LogFile::open(&path).unwrap());
        LogFile::open(&path).unwrap();
        assert_eq!(read(&numbered(&path, 1)), "session 6\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_size_limit_rotates_and_export_covers_the_session() {
        let dir = temp_dir("log-size");
        let path = dir.join("ui.log");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "previous session\n").unwrap();
        let file = LogFile::with_max_size(&path, 20).unwrap();

        write(&file, "first line\n");
        write(&file, "second line\n");
        write(&file, "third\n");

        assert_eq!(read(&path), "second line\nthird\n");
        assert_eq!(read(&numbered(&path, 1)), "first line\n");
        assert_eq!(read(&numbered(&path, 2)), "previous session\n");
        assert_eq!(
            file.session_content().unwrap(),
            "first line\nsecond line\nthird\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_export_notes_parts_rotated_away() {
        let dir = temp_dir("log-lost");
        let path = dir.join("ui.log");
        let file = LogFile::with_max_size(&path, 5).unwrap();
        for i in 0..8 {
            write(&file, &format!("line {i}\n"));
        }

        let content = file.session_content().unwrap();
        let expected: String = (2..8).map(|i| format!("line {i}\n")).collect();
        assert_eq!(
            content,
            format!("[2 older part(s) of this session's log were rotated away]\n{expected}")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
