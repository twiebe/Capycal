//! The log window in AppKit: a toolbar row above a read-only text view. It
//! renders the `Change`s of `logview::LogView` and reports user input through
//! a callback; the logic lives in the model.

use capycal::{LogBuffer, LogLine};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSButton, NSClipView, NSColor, NSControl, NSControlStateValueOff, NSControlStateValueOn,
    NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSLayoutConstraintOrientation,
    NSPopUpButton, NSSearchField, NSTextField, NSView, NSViewBoundsDidChangeNotification,
    NSWindowDelegate,
};
use objc2_foundation::{
    NSAttributedString, NSAttributedStringKey, NSDictionary, NSNotification, NSNotificationCenter,
    NSRange, NSRect, NSSize, NSString,
};
use tokio::sync::broadcast::{self, error::TryRecvError};

use crate::logview::{Change, DEFAULT_LEVEL, Input, LEVELS, LogView, Tone, ViewLine};
use crate::text_window_macos::{TextWindow, button, monospaced_font};

const SIZE: NSSize = NSSize::new(
    crate::dialog::layout::LOG_WINDOW.0,
    crate::dialog::layout::LOG_WINDOW.1,
);
const SEARCH_MIN_WIDTH: f64 = 240.0;
/// Fits the toolbar with the search field at its minimum width.
const MIN_SIZE: NSSize = NSSize::new(760.0, 240.0);
/// Distance from the end that still counts as scrolled to the end.
const END_SLACK: f64 = 4.0;

struct Ivars {
    notify: Box<dyn Fn(Input)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and LogTarget does
    // not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CapycalLogTarget"]
    #[ivars = Ivars]
    /// Target of the controls, observer of the scroll position and window
    /// delegate. It only forwards what happened; AppKit does not retain it.
    struct LogTarget;

    impl LogTarget {
        #[unsafe(method(search:))]
        fn search(&self, _sender: Option<&AnyObject>) {
            self.send(Input::Search);
        }

        #[unsafe(method(level:))]
        fn level(&self, _sender: Option<&AnyObject>) {
            self.send(Input::Level);
        }

        #[unsafe(method(follow:))]
        fn follow(&self, _sender: Option<&AnyObject>) {
            self.send(Input::Follow);
        }

        #[unsafe(method(clear:))]
        fn clear(&self, _sender: Option<&AnyObject>) {
            self.send(Input::Clear);
        }

        #[unsafe(method(export:))]
        fn export(&self, _sender: Option<&AnyObject>) {
            self.send(Input::Export);
        }

        #[unsafe(method(boundsChanged:))]
        fn bounds_changed(&self, _notification: &NSNotification) {
            self.send(Input::Scrolled);
        }
    }

    unsafe impl NSObjectProtocol for LogTarget {}

    unsafe impl NSWindowDelegate for LogTarget {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            self.send(Input::Closed);
        }
    }
);

impl LogTarget {
    fn new(mtm: MainThreadMarker, notify: impl Fn(Input) + 'static) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            notify: Box::new(notify),
        });
        // SAFETY: NSObject's designated initializer
        unsafe { msg_send![super(this), init] }
    }

    fn send(&self, input: Input) {
        (self.ivars().notify)(input);
    }
}

pub struct LogWindow {
    window: TextWindow,
    clip: Retained<NSClipView>,
    search: Retained<NSSearchField>,
    level: Retained<NSPopUpButton>,
    follow: Retained<NSButton>,
    buttons: [Retained<NSButton>; 2],
    target: Retained<LogTarget>,
    font: Retained<NSFont>,
    view: LogView,
    live: broadcast::Receiver<LogLine>,
}

impl LogWindow {
    /// Open the window with the backlog of `logs` and follow new lines.
    /// `notify` receives the input to pass to `handle` on the event loop.
    pub fn open(
        mtm: MainThreadMarker,
        logs: &LogBuffer,
        capacity: usize,
        notify: impl Fn(Input) + 'static,
    ) -> Result<Self, String> {
        let target = LogTarget::new(mtm, notify);
        let target_obj: &AnyObject = &target;

        let search = NSSearchField::new(mtm);
        search.setPlaceholderString(Some(&NSString::from_str("Search")));
        let level =
            NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        for (name, _) in LEVELS {
            level.addItemWithTitle(&NSString::from_str(name));
        }
        level.selectItemAtIndex(DEFAULT_LEVEL as isize);
        let clear = button(mtm, "Clear View", target_obj, sel!(clear:));
        let export = button(mtm, "Export", target_obj, sel!(export:));
        // SAFETY: the target outlives the controls' use of it: Drop clears it
        let follow = unsafe {
            search.setTarget(Some(target_obj));
            search.setAction(Some(sel!(search:)));
            level.setTarget(Some(target_obj));
            level.setAction(Some(sel!(level:)));
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str("Follow"),
                Some(target_obj),
                Some(sel!(follow:)),
                mtm,
            )
        };
        follow.setState(NSControlStateValueOn);
        let label = NSTextField::labelWithString(&NSString::from_str("Level:"), mtm);

        // The search field takes the free width, the other controls keep their size
        let horizontal = NSLayoutConstraintOrientation::Horizontal;
        search.setContentHuggingPriority_forOrientation(1.0, horizontal);
        search.setContentCompressionResistancePriority_forOrientation(749.0, horizontal);
        let fixed: [&NSView; 5] = [&label, &level, &follow, &clear, &export];
        for view in fixed {
            view.setContentHuggingPriority_forOrientation(750.0, horizontal);
            view.setContentCompressionResistancePriority_forOrientation(750.0, horizontal);
        }
        let row: [&NSView; 6] = [&search, &label, &level, &follow, &clear, &export];
        let window = TextWindow::new(
            mtm,
            "Log",
            SIZE,
            MIN_SIZE,
            &row,
            ProtocolObject::from_ref(&*target),
        )?;
        search
            .widthAnchor()
            .constraintGreaterThanOrEqualToConstant(SEARCH_MIN_WIDTH)
            .setActive(true);

        let clip = window.scroll.contentView();
        clip.setPostsBoundsChangedNotifications(true);
        // SAFETY: the target has `boundsChanged:` and Drop removes the observer
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                target_obj,
                sel!(boundsChanged:),
                Some(NSViewBoundsDidChangeNotification),
                Some(&clip),
            );
        }

        let (backlog, live) = logs.subscribe();
        let mut log = Self {
            window,
            clip,
            search,
            level,
            follow,
            buttons: [clear, export],
            target,
            font: monospaced_font(),
            view: LogView::new(capacity),
            live,
        };
        let change = log.view.reset(backlog.iter().map(ViewLine::from));
        log.apply(change);
        log.window
            .window
            .setInitialFirstResponder(Some(&log.search));
        log.focus();
        log.window.window.makeFirstResponder(Some(&log.search));
        Ok(log)
    }

    pub fn focus(&self) {
        self.window.focus();
    }

    /// React to input from the window, reading the current control values.
    pub fn handle(&mut self, input: Input) {
        let change = match input {
            Input::Search => self.view.set_query(&self.search.stringValue().to_string()),
            Input::Level => {
                let index = usize::try_from(self.level.indexOfSelectedItem());
                self.view.set_level(index.unwrap_or(DEFAULT_LEVEL))
            }
            Input::Follow => self
                .view
                .set_follow(self.follow.state() == NSControlStateValueOn),
            Input::Clear => self.view.clear(),
            Input::Scrolled => return self.scrolled(),
            Input::Export | Input::Closed => return,
        };
        self.apply(change);
    }

    /// Show the lines logged since the last call.
    pub fn drain(&mut self) {
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

    fn scrolled(&mut self) {
        let visible = self.clip.bounds();
        // The clip view is flipped like the text view: y grows downwards
        let end = self.window.text.frame().size.height;
        let at_end = visible.origin.y + visible.size.height >= end - END_SLACK;
        if let Some(follow) = self.view.scrolled(visible.origin.y, at_end) {
            self.follow.setState(if follow {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
    }

    fn apply(&self, change: Change) {
        // SAFETY: only the text storage is used, not the TextKit 1 layout manager
        let Some(storage) = (unsafe { self.window.text.textStorage() }) else {
            return;
        };
        storage.beginEditing();
        if change.reset {
            storage.deleteCharactersInRange(NSRange::new(0, storage.length()));
        }
        if change.trim > 0 {
            storage.deleteCharactersInRange(NSRange::new(0, change.trim.min(storage.length())));
        }
        for (tone, text) in &change.append {
            let attributes = self.attributes(*tone);
            // SAFETY: the attributes map the font and color keys to an NSFont and an NSColor
            let run = unsafe {
                NSAttributedString::initWithString_attributes(
                    NSAttributedString::alloc(),
                    &NSString::from_str(text),
                    Some(&attributes),
                )
            };
            storage.appendAttributedString(&run);
        }
        storage.endEditing();
        if change.scroll_to_end {
            self.window
                .text
                .scrollRangeToVisible(NSRange::new(storage.length(), 0));
        }
    }

    fn attributes(&self, tone: Tone) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
        // Semantic colors adapt to dark mode
        let color = match tone {
            Tone::Error => NSColor::systemRedColor(),
            Tone::Warn => NSColor::systemOrangeColor(),
            Tone::Normal => NSColor::labelColor(),
            Tone::Dim => NSColor::secondaryLabelColor(),
        };
        // SAFETY: immutable AppKit constants
        let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
        let values: [&AnyObject; 2] = [&self.font, &color];
        NSDictionary::from_slices(&keys, &values)
    }
}

impl Drop for LogWindow {
    fn drop(&mut self) {
        // Controls and notification center keep unretained pointers to the
        // target, and the search field may still send a delayed action. The
        // TextWindow clears the delegate when dropped after this.
        let controls: [&NSControl; 5] = [
            &self.search,
            &self.level,
            &self.follow,
            &self.buttons[0],
            &self.buttons[1],
        ];
        // SAFETY: clearing the pointers is always valid
        unsafe {
            NSNotificationCenter::defaultCenter().removeObserver(&self.target);
            for control in controls {
                control.setTarget(None);
            }
        }
    }
}
