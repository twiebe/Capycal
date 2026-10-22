//! Platform-independent side of the log window: which lines are shown, how
//! they look, follow mode, trimming and the export. A platform window only
//! renders the `Change`s this model returns and reports user input.

use std::collections::VecDeque;

use capycal::{LogBuffer, LogFile, LogLine};
use chrono::{DateTime, Utc};
use tracing::Level;

/// Entries of the level menu: the least severe level shown.
pub const LEVELS: [(&str, Level); 4] = [
    ("Error", Level::ERROR),
    ("Warn", Level::WARN),
    ("Info", Level::INFO),
    ("Debug", Level::TRACE),
];
/// Index into `LEVELS` selected when the window opens.
pub const DEFAULT_LEVEL: usize = 2;

/// Something the user did in the log window. The platform glue reports it and
/// reads the control values when it is handled.
// Only the platform windows use it
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Search,
    Level,
    Follow,
    Scrolled,
    Clear,
    Export,
    Closed,
}

/// How a line is colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Error,
    Warn,
    Normal,
    Dim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewLine {
    pub level: Level,
    pub text: String,
}

impl From<&LogLine> for ViewLine {
    fn from(line: &LogLine) -> Self {
        Self {
            level: line.level,
            text: line.text.clone(),
        }
    }
}

impl ViewLine {
    /// Marker for lines the view missed because it fell behind.
    pub fn skipped(count: u64) -> Self {
        Self {
            level: Level::WARN,
            text: format!("… {count} log lines skipped"),
        }
    }

    pub fn tone(&self) -> Tone {
        match self.level {
            Level::ERROR => Tone::Error,
            Level::WARN => Tone::Warn,
            Level::INFO => Tone::Normal,
            _ => Tone::Dim,
        }
    }

    /// Length in the text view, newline included. Text views index UTF-16.
    fn utf16_len(&self) -> usize {
        self.text.encode_utf16().count() + 1
    }
}

/// What the window has to do to its text, in this order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// Remove all text.
    pub reset: bool,
    /// UTF-16 units to remove from the start.
    pub trim: usize,
    /// Text to append, one run per sequence of lines with the same tone.
    pub append: Vec<(Tone, String)>,
    pub scroll_to_end: bool,
}

pub struct LogView {
    /// Received lines since the last clear, at most `capacity`.
    lines: VecDeque<ViewLine>,
    /// UTF-16 length of each line in the text view, oldest first.
    shown: VecDeque<usize>,
    capacity: usize,
    level: Level,
    /// Lowercase search text.
    query: String,
    follow: bool,
    /// Last reported scroll offset from the top.
    offset: f64,
}

impl LogView {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            shown: VecDeque::new(),
            capacity: capacity.max(1),
            level: LEVELS[DEFAULT_LEVEL].1,
            query: String::new(),
            follow: true,
            offset: 0.0,
        }
    }

    /// Show `lines` instead of everything before.
    pub fn reset(&mut self, lines: impl IntoIterator<Item = ViewLine>) -> Change {
        self.lines.clear();
        self.store(lines);
        self.rebuild()
    }

    /// Add new lines; only the matching ones reach the view.
    pub fn push(&mut self, lines: impl IntoIterator<Item = ViewLine>) -> Change {
        let first = self.lines.len();
        let dropped = self.store(lines);
        let start = first.saturating_sub(dropped);
        let new: Vec<&ViewLine> = self
            .lines
            .range(start..)
            .filter(|l| self.matches(l))
            .collect();

        // The view may grow to capacity plus a chunk, then drops back to
        // capacity, so trimming is not needed for every line
        let mut trim = 0;
        let mut skip = 0;
        let total = self.shown.len() + new.len();
        if total >= self.capacity + trim_chunk(self.capacity) {
            let excess = total - self.capacity;
            let old = excess.min(self.shown.len());
            trim = self.shown.drain(..old).sum();
            skip = excess - old;
        }
        let new = &new[skip..];
        self.shown.extend(new.iter().map(|l| l.utf16_len()));
        Change {
            reset: false,
            trim,
            scroll_to_end: self.follow && !new.is_empty(),
            append: runs(new.iter().copied()),
        }
    }

    /// Remove all lines from the view; new ones still arrive.
    pub fn clear(&mut self) -> Change {
        self.lines.clear();
        self.rebuild()
    }

    pub fn set_query(&mut self, query: &str) -> Change {
        self.query = query.to_lowercase();
        self.rebuild()
    }

    /// Select `LEVELS[index]`; out of range selects the default.
    pub fn set_level(&mut self, index: usize) -> Change {
        self.level = LEVELS.get(index).unwrap_or(&LEVELS[DEFAULT_LEVEL]).1;
        self.rebuild()
    }

    pub fn set_follow(&mut self, follow: bool) -> Change {
        self.follow = follow;
        Change {
            scroll_to_end: follow,
            ..Default::default()
        }
    }

    /// The view was scrolled to `offset` from the top. Scrolling up away from
    /// the end stops following, scrolling down to the end follows again.
    /// Returns the new follow state if it changed. Resizing or trimming does
    /// not move the offset up, so they leave following alone.
    pub fn scrolled(&mut self, offset: f64, at_end: bool) -> Option<bool> {
        let up = offset < self.offset;
        let down = offset > self.offset;
        self.offset = offset;
        let follow = match self.follow {
            true if up && !at_end => false,
            false if down && at_end => true,
            _ => return None,
        };
        self.follow = follow;
        Some(follow)
    }

    /// Append to `lines`, keeping the newest `capacity`. Returns how many
    /// old lines were dropped.
    fn store(&mut self, lines: impl IntoIterator<Item = ViewLine>) -> usize {
        let mut dropped = 0;
        for line in lines {
            if self.lines.len() == self.capacity {
                self.lines.pop_front();
                dropped += 1;
            }
            self.lines.push_back(line);
        }
        dropped
    }

    fn rebuild(&mut self) -> Change {
        let lines: Vec<&ViewLine> = self.lines.iter().filter(|l| self.matches(l)).collect();
        self.shown = lines.iter().map(|l| l.utf16_len()).collect();
        Change {
            reset: true,
            trim: 0,
            append: runs(lines.into_iter()),
            scroll_to_end: self.follow,
        }
    }

    fn matches(&self, line: &ViewLine) -> bool {
        line.level <= self.level
            && (self.query.is_empty() || line.text.to_lowercase().contains(&self.query))
    }
}

fn trim_chunk(capacity: usize) -> usize {
    (capacity / 10).max(1)
}

/// The lines as text, one entry per sequence with the same tone.
fn runs<'a>(lines: impl Iterator<Item = &'a ViewLine>) -> Vec<(Tone, String)> {
    let mut runs: Vec<(Tone, String)> = Vec::new();
    for line in lines {
        let tone = line.tone();
        match runs.last_mut() {
            Some((last, text)) if *last == tone => text.push_str(&line.text),
            _ => runs.push((tone, line.text.clone())),
        }
        if let Some((_, text)) = runs.last_mut() {
            text.push('\n');
        }
    }
    runs
}

/// Default file name for an export, e.g. `capycal-20261012-153000.log` (UTC).
pub fn export_file_name(now: DateTime<Utc>) -> String {
    format!("capycal-{}.log", now.format("%Y%m%d-%H%M%S"))
}

/// The unfiltered log of this session, or the buffered lines if the log
/// file cannot be read.
pub fn export_content(log_file: &LogFile, buffer: &LogBuffer) -> String {
    log_file.session_content().unwrap_or_else(|_| {
        buffer
            .snapshot()
            .iter()
            .map(|l| format!("{}\n", l.text))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::fmt::MakeWriter;

    fn line(level: Level, text: &str) -> ViewLine {
        ViewLine {
            level,
            text: text.to_string(),
        }
    }

    fn info(text: &str) -> ViewLine {
        line(Level::INFO, text)
    }

    fn text(change: &Change) -> String {
        change.append.iter().map(|(_, t)| t.as_str()).collect()
    }

    #[test]
    fn test_view_line_from_log_line() {
        let log = LogLine {
            timestamp: Utc::now(),
            level: Level::WARN,
            text: "full line".to_string(),
        };
        assert_eq!(ViewLine::from(&log), line(Level::WARN, "full line"));
        let skipped = ViewLine::skipped(7);
        assert_eq!(skipped.text, "… 7 log lines skipped");
        assert_eq!(skipped.tone(), Tone::Warn);
    }

    #[test]
    fn test_tones_and_runs() {
        let lines = [
            line(Level::ERROR, "e"),
            line(Level::INFO, "i1"),
            line(Level::INFO, "i2"),
            line(Level::DEBUG, "d"),
            line(Level::TRACE, "t"),
            line(Level::WARN, "w"),
        ];
        assert_eq!(
            runs(lines.iter()),
            [
                (Tone::Error, "e\n".to_string()),
                (Tone::Normal, "i1\ni2\n".to_string()),
                (Tone::Dim, "d\nt\n".to_string()),
                (Tone::Warn, "w\n".to_string()),
            ]
        );
    }

    #[test]
    fn test_reset_shows_matching_backlog_and_follows() {
        let mut view = LogView::new(10);
        let change = view.reset([info("a"), line(Level::DEBUG, "hidden"), info("b")]);
        assert!(change.reset);
        assert!(change.scroll_to_end);
        assert_eq!(text(&change), "a\nb\n");
    }

    #[test]
    fn test_push_appends_only_matching_lines() {
        let mut view = LogView::new(10);
        view.reset([]);
        let change = view.push([info("x"), line(Level::DEBUG, "y"), ViewLine::skipped(3)]);
        assert!(!change.reset);
        assert_eq!(change.trim, 0);
        assert_eq!(text(&change), "x\n… 3 log lines skipped\n");
        assert!(change.scroll_to_end);
        assert_eq!(view.push([line(Level::DEBUG, "z")]), Change::default());
    }

    #[test]
    fn test_level_filter() {
        let mut view = LogView::new(10);
        let all = [
            line(Level::ERROR, "e"),
            line(Level::WARN, "w"),
            info("i"),
            line(Level::DEBUG, "d"),
            line(Level::TRACE, "t"),
        ];
        view.reset(all.clone());
        let shown: Vec<String> = (0..LEVELS.len())
            .map(|i| text(&view.set_level(i)))
            .collect();
        assert_eq!(shown, ["e\n", "e\nw\n", "e\nw\ni\n", "e\nw\ni\nd\nt\n"]);
        assert_eq!(text(&view.set_level(99)), "e\nw\ni\n");
    }

    #[test]
    fn test_search_is_case_insensitive_and_applies_to_new_lines() {
        let mut view = LogView::new(10);
        view.reset([info("Sync WORK done"), info("other")]);
        let change = view.set_query("work");
        assert!(change.reset);
        assert_eq!(text(&change), "Sync WORK done\n");
        assert_eq!(
            text(&view.push([info("nope"), info("work again")])),
            "work again\n"
        );
        assert_eq!(
            text(&view.set_query("")),
            "Sync WORK done\nother\nnope\nwork again\n"
        );
    }

    #[test]
    fn test_clear_keeps_receiving() {
        let mut view = LogView::new(10);
        view.reset([info("old")]);
        let change = view.clear();
        assert!(change.reset);
        assert!(change.append.is_empty());
        assert_eq!(text(&view.push([info("new")])), "new\n");
        assert_eq!(text(&view.set_query("")), "new\n");
    }

    #[test]
    fn test_push_trims_in_chunks_counting_utf16() {
        // Capacity 10, chunk 1: the view drops back to 10 lines
        let mut view = LogView::new(10);
        view.reset((0..10).map(|i| info(&format!("l{i}"))));
        let change = view.push([info("ä😀")]);
        assert_eq!(change.trim, "l0\n".len());
        assert_eq!(text(&change), "ä😀\n");

        let mut view = LogView::new(20);
        view.reset((0..20).map(|i| info(&format!("{i:02}"))));
        // Chunk 2: one line over capacity is kept, the second trims both
        assert_eq!(view.push([info("😀")]).trim, 0);
        let change = view.push([info("x")]);
        assert_eq!(change.trim, 2 * "00\n".len());
        // The emoji is two UTF-16 units plus the newline
        let change = view.push((0..22).map(|_| info("y")));
        assert_eq!(change.trim, 18 * 3 + 3 + 2);
        assert_eq!(text(&change), "y\n".repeat(20));
    }

    #[test]
    fn test_lines_are_capped_for_rebuilds() {
        let mut view = LogView::new(3);
        view.reset([info("a"), info("b")]);
        view.push([info("c"), info("d"), info("e")]);
        assert_eq!(text(&view.set_query("")), "c\nd\ne\n");
    }

    #[test]
    fn test_follow_tracks_scrolling() {
        let mut view = LogView::new(10);
        assert!(view.follow);
        assert_eq!(view.scrolled(500.0, true), None);
        // Resizing or trimming at the same offset changes nothing
        assert_eq!(view.scrolled(500.0, false), None);
        assert_eq!(view.scrolled(300.0, false), Some(false));
        assert!(!view.follow);
        assert!(!view.push([info("x")]).scroll_to_end);
        assert!(!view.clear().scroll_to_end);
        assert_eq!(view.scrolled(400.0, false), None);
        assert_eq!(view.scrolled(450.0, true), Some(true));
        assert!(view.follow);
        // Rubber-banding back to the end keeps following
        assert_eq!(view.scrolled(440.0, true), None);
    }

    #[test]
    fn test_set_follow_scrolls_to_end() {
        let mut view = LogView::new(10);
        assert!(!view.set_follow(false).scroll_to_end);
        assert!(!view.follow);
        let change = view.set_follow(true);
        assert!(change.scroll_to_end);
        assert!(!change.reset);
    }

    #[test]
    fn test_export_file_name() {
        let now = DateTime::parse_from_rfc3339("2026-10-12T15:30:05Z")
            .unwrap()
            .to_utc();
        assert_eq!(export_file_name(now), "capycal-20261012-153005.log");
    }

    #[test]
    fn test_export_content_prefers_file_over_buffer() {
        let dir = std::env::temp_dir().join(format!("capycal-{}-ui-export", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("capycal-ui.log");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "previous session\n").unwrap();
        let file = LogFile::open(&path).unwrap();
        let buffer = LogBuffer::new(10);
        let subscriber = tracing_subscriber::layer::SubscriberExt::with(
            tracing_subscriber::Registry::default(),
            buffer.clone(),
        );
        tracing::subscriber::with_default(subscriber, || tracing::info!("buffered"));
        std::io::Write::write_all(&mut file.make_writer(), b"from file\n").unwrap();
        assert_eq!(export_content(&file, &buffer), "from file\n");

        std::fs::remove_dir_all(&dir).unwrap();
        let content = export_content(&file, &buffer);
        assert!(content.ends_with(" INFO buffered\n"), "{content}");
    }
}
