//! The live log window in GTK. `logview::LogView` decides what is shown;
//! this only renders its `Change`s and reports input.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use capycal::{LogBuffer, LogFile, LogLine};
use chrono::Utc;
use gtk::{gio, glib};
use tokio::sync::broadcast::{self, error::TryRecvError};
use tracing::error;

use crate::logview::{
    Change, DEFAULT_LEVEL, LEVELS, LogView, Tone, ViewLine, export_content, export_file_name,
};

/// How often new log lines are shown.
const POLL: Duration = Duration::from_millis(250);
/// Distance from the end that still counts as at the end.
const END_SLACK: f64 = 4.0;

pub struct LogWindow {
    window: adw::Window,
}

struct Inner {
    buffer: gtk::TextBuffer,
    text: gtk::TextView,
    adjustment: gtk::Adjustment,
    follow: gtk::ToggleButton,
    view: LogView,
    live: broadcast::Receiver<LogLine>,
}

impl LogWindow {
    /// Open the log window with the buffered lines and follow new ones.
    /// `closed` runs when the window closes.
    pub fn open(
        application: &adw::Application,
        logs: &LogBuffer,
        log_file: &LogFile,
        capacity: usize,
        closed: impl Fn() + 'static,
    ) -> Self {
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search")
            .hexpand(true)
            .build();
        let levels: Vec<&str> = LEVELS.iter().map(|(name, _)| *name).collect();
        let level = gtk::DropDown::from_strings(&levels);
        level.set_selected(DEFAULT_LEVEL as u32);
        level.set_tooltip_text(Some("Show this level and more severe ones"));
        let follow = gtk::ToggleButton::builder()
            .label("Follow")
            .active(true)
            .tooltip_text("Keep the newest line in view")
            .build();
        let clear = gtk::Button::builder()
            .label("Clear View")
            .tooltip_text("Empty the window; the log itself is kept")
            .build();
        let export = gtk::Button::builder()
            .label("Export")
            .tooltip_text("Save the complete log of this session")
            .build();

        let bar = gtk::Box::builder()
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        for widget in [
            search.upcast_ref::<gtk::Widget>(),
            level.upcast_ref(),
            follow.upcast_ref(),
            clear.upcast_ref(),
            export.upcast_ref(),
        ] {
            bar.append(widget);
        }

        let buffer = gtk::TextBuffer::new(None);
        for tone in TINTED {
            buffer.create_tag(Some(tag_name(tone)), &[]);
        }
        let style = adw::StyleManager::default();
        set_tone_colors(&buffer, style.is_dark());
        let weak = buffer.downgrade();
        let dark_changed = style.connect_dark_notify(move |style| {
            if let Some(buffer) = weak.upgrade() {
                set_tone_colors(&buffer, style.is_dark());
            }
        });
        let text = gtk::TextView::builder()
            .buffer(&buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(6)
            .right_margin(6)
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&text)
            .build();
        let adjustment = scroll.vadjustment();

        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.add_top_bar(&bar);
        view.set_content(Some(&scroll));
        let window = adw::Window::builder()
            .application(application)
            .title("Log")
            .default_width(crate::dialog::layout::LOG_WINDOW.0 as i32)
            .default_height(crate::dialog::layout::LOG_WINDOW.1 as i32)
            .content(&view)
            .build();
        // The style manager outlives the window
        let dark_changed = Cell::new(Some(dark_changed));
        window.connect_destroy(move |_| {
            if let Some(handler) = dark_changed.take() {
                adw::StyleManager::default().disconnect(handler);
            }
        });

        let (backlog, live) = logs.subscribe();
        let inner = Rc::new(RefCell::new(Inner {
            buffer,
            text,
            adjustment: adjustment.clone(),
            follow: follow.clone(),
            view: LogView::new(capacity),
            live,
        }));
        {
            let mut inner = inner.borrow_mut();
            let change = inner.view.reset(backlog.iter().map(ViewLine::from));
            inner.apply(change);
        }

        let weak = Rc::downgrade(&inner);
        search.connect_search_changed(move |search| {
            with(&weak, |inner| inner.view.set_query(&search.text()));
        });
        let weak = Rc::downgrade(&inner);
        level.connect_selected_notify(move |level| {
            with(&weak, |inner| {
                inner.view.set_level(level.selected() as usize)
            });
        });
        let weak = Rc::downgrade(&inner);
        follow.connect_toggled(move |follow| {
            with(&weak, |inner| inner.view.set_follow(follow.is_active()));
        });
        let weak = Rc::downgrade(&inner);
        clear.connect_clicked(move |_| with(&weak, |inner| inner.view.clear()));
        let weak = Rc::downgrade(&inner);
        adjustment.connect_value_changed(move |adjustment| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let at_end =
                adjustment.value() + adjustment.page_size() >= adjustment.upper() - END_SLACK;
            let follow = inner.borrow_mut().view.scrolled(adjustment.value(), at_end);
            if let Some(follow) = follow {
                let toggle = inner.borrow().follow.clone();
                toggle.set_active(follow);
            }
        });
        let log_file = log_file.clone();
        let logs = logs.clone();
        let parent = window.clone();
        export.connect_clicked(move |_| save_export(&parent, &log_file, &logs));

        let weak = Rc::downgrade(&inner);
        glib::timeout_add_local(POLL, move || match weak.upgrade() {
            Some(inner) => {
                inner.borrow_mut().drain();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        // The window owns the state; closing drops it and stops the polling
        window.connect_close_request(move |_| {
            let _ = &inner;
            closed();
            glib::Propagation::Proceed
        });
        window.present();
        search.grab_focus();
        Self { window }
    }

    pub fn present(&self) {
        self.window.present();
    }
}

impl Drop for LogWindow {
    fn drop(&mut self) {
        self.window.destroy();
    }
}

/// Run `f` on the window state, if it still exists, and apply its change.
fn with(weak: &Weak<RefCell<Inner>>, f: impl FnOnce(&mut Inner) -> Change) {
    if let Some(inner) = weak.upgrade() {
        let mut inner = inner.borrow_mut();
        let change = f(&mut inner);
        inner.apply(change);
    }
}

impl Inner {
    /// Show the lines logged since the last call.
    fn drain(&mut self) {
        let mut lines = Vec::new();
        loop {
            match self.live.try_recv() {
                Ok(line) => lines.push(ViewLine::from(&line)),
                Err(TryRecvError::Lagged(count)) => lines.push(ViewLine::skipped(count)),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        if !lines.is_empty() {
            let change = self.view.push(lines);
            self.apply(change);
        }
    }

    fn apply(&mut self, change: Change) {
        let buffer = &self.buffer;
        if change.reset {
            buffer.set_text("");
        }
        if change.trim > 0 {
            // The model counts UTF-16 units, GTK counts characters
            let mut start = buffer.start_iter();
            let mut end = buffer.start_iter();
            let mut units = 0;
            while units < change.trim && !end.is_end() {
                units += end.char().len_utf16();
                end.forward_char();
            }
            buffer.delete(&mut start, &mut end);
        }
        for (tone, text) in &change.append {
            let mut end = buffer.end_iter();
            match tone {
                Tone::Normal => buffer.insert(&mut end, text),
                tone => buffer.insert_with_tags_by_name(&mut end, text, &[tag_name(*tone)]),
            }
        }
        if change.scroll_to_end {
            let mark = buffer.create_mark(None, &buffer.end_iter(), false);
            self.text.scroll_mark_onscreen(&mark);
            buffer.delete_mark(&mark);
            let adjustment = self.adjustment.clone();
            // The new lines are laid out after this returns
            glib::idle_add_local_once(move || {
                adjustment.set_value(adjustment.upper() - adjustment.page_size());
            });
        }
    }
}

/// The tones that get a text tag, in the order of the colors in `set_tone_colors`.
const TINTED: [Tone; 3] = [Tone::Error, Tone::Warn, Tone::Dim];

/// Color the tone tags with libadwaita's text colors for a light or dark style.
fn set_tone_colors(buffer: &gtk::TextBuffer, dark: bool) {
    let colors = if dark {
        ["#ff938c", "#ffc252", "rgba(255, 255, 255, 0.55)"]
    } else {
        ["#c30000", "#905400", "rgba(0, 0, 6, 0.44)"]
    };
    let table = buffer.tag_table();
    for (tone, color) in TINTED.into_iter().zip(colors) {
        if let Some(tag) = table.lookup(tag_name(tone)) {
            tag.set_foreground(Some(color));
        }
    }
}

fn tag_name(tone: Tone) -> &'static str {
    match tone {
        Tone::Error => "error",
        Tone::Warn => "warn",
        Tone::Normal => "normal",
        Tone::Dim => "dim",
    }
}

/// Ask where to save the complete log of this session and write it there.
fn save_export(parent: &adw::Window, log_file: &LogFile, logs: &LogBuffer) {
    let content = export_content(log_file, logs);
    let dialog = gtk::FileDialog::builder()
        .title("Export Log")
        .initial_name(export_file_name(Utc::now()))
        .build();
    dialog.save(Some(parent), None::<&gio::Cancellable>, move |result| {
        let Ok(file) = result else {
            return;
        };
        let Some(path) = file.path() else {
            return;
        };
        if let Err(e) = std::fs::write(&path, &content) {
            error!(path = %path.display(), error = %e, "cannot export the log");
        }
    });
}
