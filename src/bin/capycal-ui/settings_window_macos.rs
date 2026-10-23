//! The Settings window in AppKit: a sidebar of syncs and accounts and an
//! editor drawn from a `SettingsView`. It only renders and reports input;
//! `settings_view` decides everything else.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    AllocAnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class,
    msg_send, sel,
};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor, NSComboBox, NSComboBoxDelegate,
    NSControl, NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate,
    NSCursor, NSEvent, NSFont, NSImage, NSImageScaling, NSImageView, NSLayoutAttribute,
    NSLayoutConstraint, NSLayoutConstraintOrientation, NSMenu, NSMenuItem, NSPopUpButton,
    NSProgressIndicator, NSProgressIndicatorStyle, NSResponder, NSScrollView, NSSecureTextField,
    NSSegmentSwitchTracking, NSSegmentedControl, NSStackView, NSStackViewGravity, NSSwitch,
    NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTitlePosition, NSTrackingArea,
    NSTrackingAreaOptions, NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSData, NSNotification, NSPoint, NSRange, NSRect, NSSize, NSString,
};

use crate::settings_view::{
    ButtonSpec, Choice, ChoiceIcon, ChoiceStyle, Control, EventPreview, FieldId, HeroImage,
    PopupItem, SettingsInput, SettingsScreen, SettingsView, SidebarItem, steps_text,
};
use crate::text_window_macos::{button, focus};

const SIZE: NSSize = NSSize::new(1100.0, 720.0);
/// The column right of the editor that keeps the preview in view.
const ASIDE_WIDTH: f64 = 240.0;
/// The wizard: no sidebar, a smaller window.
const COMPACT_SIZE: NSSize = NSSize::new(560.0, 640.0);
const MIN_SIZE: NSSize = NSSize::new(680.0, 420.0);
const SIDEBAR_WIDTH: f64 = 200.0;
/// Where the window's size and place are kept in the user defaults.
const FRAME_NAME: &str = "CapycalSettings";
/// The column right of the pipeline steps that holds their drag handles.
const GUTTER: f64 = 24.0;
/// A field's label column: the label at its left, the control after it.
const LABEL_WIDTH: f64 = 110.0;
/// The spacing scale, on a 4 point grid.
const GAP_S: f64 = 4.0;
const GAP_M: f64 = 8.0;
const GAP_L: f64 = 12.0;
const GAP_XL: f64 = 20.0;
/// The transparent title bar the content reaches under.
const TITLEBAR: f64 = 28.0;
/// Where the sidebar starts: below the window buttons.
const SIDEBAR_TOP: f64 = 52.0;
const MARGIN: f64 = 16.0;
/// Tags are the build's generation times this plus the index in `tags`.
const TAG_GENERATION: usize = 100_000;

/// What a control reported, before it is mapped to a `SettingsInput`.
#[derive(Debug, Clone, PartialEq)]
pub enum RawInput {
    Pressed {
        tag: isize,
        on: bool,
    },
    Text {
        tag: isize,
        text: String,
    },
    Chose {
        tag: isize,
        index: isize,
    },
    Segment {
        tag: isize,
        index: isize,
    },
    /// An item of a "+" menu: the joint's tag times 100 plus the index.
    MenuItem {
        tag: isize,
    },
    /// A text field started editing.
    Focus {
        tag: isize,
    },
    /// A text field stopped editing.
    Left {
        tag: isize,
    },
    /// A drag handle moved to `y` (window coordinates), or let go.
    Drag {
        tag: isize,
        y: f64,
        done: bool,
    },
    /// A frame of the chevrons turning.
    Tick,
    Shortcut(crate::settings_view::Shortcut),
    CloseRequested,
}

pub struct TargetIvars {
    notify: Box<dyn Fn(RawInput)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and SettingsTarget
    // does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CapycalSettingsTarget"]
    #[ivars = TargetIvars]
    /// Target of every control, delegate of the text fields and the window.
    /// It only forwards what happened; AppKit does not retain it.
    pub struct SettingsTarget;

    impl SettingsTarget {
        #[unsafe(method(pressed:))]
        fn pressed(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else {
                return;
            };
            // SAFETY: the sender is one of the window's NSButtons
            let (tag, state): (isize, isize) =
                unsafe { (msg_send![sender, tag], msg_send![sender, state]) };
            self.send(RawInput::Pressed {
                tag,
                on: state == NSControlStateValueOn,
            });
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _timer: Option<&AnyObject>) {
            self.send(RawInput::Tick);
        }

        #[unsafe(method(edited:))]
        fn edited(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else {
                return;
            };
            self.send_text(sender);
        }

        #[unsafe(method(chose:))]
        fn chose(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else {
                return;
            };
            // SAFETY: the sender is an NSPopUpButton of the window
            let (tag, index): (isize, isize) =
                unsafe { (msg_send![sender, tag], msg_send![sender, indexOfSelectedItem]) };
            self.send(RawInput::Chose { tag, index });
        }

        #[unsafe(method(menuChose:))]
        fn menu_chose(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else {
                return;
            };
            // SAFETY: the sender is an NSMenuItem of a "+" menu
            let tag: isize = unsafe { msg_send![sender, tag] };
            self.send(RawInput::MenuItem { tag });
        }

        #[unsafe(method(segmented:))]
        fn segmented(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else {
                return;
            };
            // SAFETY: the sender is an NSSegmentedControl of the window
            let (tag, index): (isize, isize) =
                unsafe { (msg_send![sender, tag], msg_send![sender, selectedSegment]) };
            self.send(RawInput::Segment { tag, index });
        }
    }

    unsafe impl NSObjectProtocol for SettingsTarget {}

    unsafe impl NSControlTextEditingDelegate for SettingsTarget {
        /// Every keystroke, so errors and Save follow the typing.
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, notification: &NSNotification) {
            if let Some(field) = notification.object() {
                if let Some(combo) = field.downcast_ref::<NSComboBox>() {
                    complete_upper_case(combo);
                }
                self.send_text(&field);
            }
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, notification: &NSNotification) {
            if let Some(field) = notification.object() {
                // SAFETY: the object is one of the window's NSTextFields
                let tag: isize = unsafe { msg_send![&*field, tag] };
                self.send(RawInput::Left { tag });
            }
        }

        #[unsafe(method(controlTextDidBeginEditing:))]
        fn control_text_did_begin_editing(&self, notification: &NSNotification) {
            if let Some(field) = notification.object() {
                // SAFETY: the object is one of the window's NSTextFields
                let tag: isize = unsafe { msg_send![&*field, tag] };
                self.send(RawInput::Focus { tag });
            }
        }

        /// Esc ends editing a field, so the focus can leave it.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(
            &self,
            control: &NSControl,
            _text_view: &AnyObject,
            command: objc2::runtime::Sel,
        ) -> objc2::runtime::Bool {
            if command == sel!(cancelOperation:) {
                if let Some(window) = control.window() {
                    window.makeFirstResponder(None);
                }
                return objc2::runtime::Bool::YES;
            }
            objc2::runtime::Bool::NO
        }
    }

    unsafe impl NSTextFieldDelegate for SettingsTarget {}

    unsafe impl NSComboBoxDelegate for SettingsTarget {
        /// A suggestion picked from the list; the field's text only
        /// changes after this, so the picked item is sent.
        #[unsafe(method(comboBoxSelectionDidChange:))]
        fn combo_box_selection_did_change(&self, notification: &NSNotification) {
            let Some(combo) = notification.object() else {
                return;
            };
            // SAFETY: the object is one of the window's NSComboBoxes, the
            // index one of its items
            unsafe {
                let tag: isize = msg_send![&*combo, tag];
                let index: isize = msg_send![&*combo, indexOfSelectedItem];
                if index < 0 {
                    return;
                }
                let item: Retained<NSString> = msg_send![&*combo, itemObjectValueAtIndex: index];
                self.send(RawInput::Text {
                    tag,
                    text: item.to_string(),
                });
            }
        }
    }

    unsafe impl NSWindowDelegate for SettingsTarget {
        /// The screen decides (it may ask about unsaved changes); the app
        /// drops the window when it should close.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            self.send(RawInput::CloseRequested);
            false
        }
    }
);

impl SettingsTarget {
    fn new(mtm: MainThreadMarker, notify: impl Fn(RawInput) + 'static) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TargetIvars {
            notify: Box::new(notify),
        });
        // SAFETY: NSObject's designated initializer
        unsafe { msg_send![super(this), init] }
    }

    fn send(&self, input: RawInput) {
        (self.ivars().notify)(input);
    }

    fn send_text(&self, sender: &AnyObject) {
        // SAFETY: the sender is one of the window's NSTextFields
        let (tag, text): (isize, Retained<NSString>) =
            unsafe { (msg_send![sender, tag], msg_send![sender, stringValue]) };
        self.send(RawInput::Text {
            tag,
            text: text.to_string(),
        });
    }
}

/// Field names are upper case: while typing at the end, the text turns
/// upper case and is completed with the first suggestion it starts, the
/// rest selected, as NSComboBox does for text typed in the case of its
/// items.
fn complete_upper_case(combo: &NSComboBox) {
    let Some(editor) = combo.currentEditor() else {
        return;
    };
    let text = editor.string().to_string();
    let upper = text.to_uppercase();
    let typed = upper.encode_utf16().count();
    let range = editor.selectedRange();
    if upper == text || range.length != 0 || range.location != text.encode_utf16().count() {
        return;
    }
    let completed = (0..combo.numberOfItems())
        .map(|i| {
            // SAFETY: the items are the NSStrings of the suggestions
            let item: Retained<NSString> = unsafe { msg_send![combo, itemObjectValueAtIndex: i] };
            item.to_string()
        })
        .find(|item| item.starts_with(&upper))
        .unwrap_or_else(|| upper.clone());
    editor.setString(&NSString::from_str(&completed));
    editor.setSelectedRange(NSRange::new(
        typed,
        completed.encode_utf16().count() - typed,
    ));
}

define_class!(
    // SAFETY: NSView has no subclassing requirements and FlippedView does
    // not implement Drop.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CapycalFlippedView"]
    #[ivars = std::cell::RefCell<Option<Retained<SettingsTarget>>>]
    /// The editor's document view: flipped, so its content starts at the
    /// top of the scroll view. It also takes the window's shortcuts.
    struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// A click beside the controls ends editing a field.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if let Some(window) = self.window() {
                window.makeFirstResponder(None);
            }
        }

        /// Cmd+S, Cmd+N, Cmd+Delete and, outside a field, Cmd+Z.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> objc2::runtime::Bool {
            use crate::settings_view::Shortcut;
            let flags = event.modifierFlags();
            let command = objc2_app_kit::NSEventModifierFlags::Command;
            let others = objc2_app_kit::NSEventModifierFlags::Shift
                | objc2_app_kit::NSEventModifierFlags::Option
                | objc2_app_kit::NSEventModifierFlags::Control;
            let option = flags.contains(objc2_app_kit::NSEventModifierFlags::Option);
            let others = if option {
                others.difference(objc2_app_kit::NSEventModifierFlags::Option)
            } else {
                others
            };
            if !flags.contains(command) || flags.intersects(others) {
                return objc2::runtime::Bool::NO;
            }
            // A field being typed in (not a selectable label) keeps its
            // own undo and delete
            let typing = self.window().and_then(|w| w.firstResponder()).is_some_and(|r| {
                // SAFETY: isEditable is asked of an NSTextView only
                r.isKindOfClass(objc2::class!(NSTextView))
                    && unsafe { msg_send![&*r, isEditable] }
            });
            let key = event
                .charactersIgnoringModifiers()
                .map(|c| c.to_string())
                .unwrap_or_default();
            let shortcut = match key.as_str() {
                // NSUpArrowFunctionKey, NSDownArrowFunctionKey
                "\u{f700}" if option => Shortcut::MoveUp,
                "\u{f701}" if option => Shortcut::MoveDown,
                _ if option => return objc2::runtime::Bool::NO,
                "s" => Shortcut::Save,
                "n" => Shortcut::NewSync,
                // Before the menu's: Settings asks about a draft itself
                "w" => Shortcut::Close,
                "q" => Shortcut::Quit,
                "\u{7f}" | "\u{8}" if !typing => Shortcut::Delete,
                "z" if !typing => Shortcut::Undo,
                _ => return objc2::runtime::Bool::NO,
            };
            match &*self.ivars().borrow() {
                Some(target) => {
                    target.send(RawInput::Shortcut(shortcut));
                    objc2::runtime::Bool::YES
                }
                None => objc2::runtime::Bool::NO,
            }
        }
    }
);

impl FlippedView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(std::cell::RefCell::new(None));
        // SAFETY: NSView's designated initializer
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

/// What a card or row looks like: its background, style and state.
#[derive(Default)]
pub struct CardIvars {
    background: std::cell::RefCell<Option<Retained<NSBox>>>,
    style: std::cell::Cell<Option<ChoiceStyle>>,
    selected: std::cell::Cell<bool>,
    hovered: std::cell::Cell<bool>,
    /// A sidebar row: its label and icon follow the selection.
    sidebar: std::cell::RefCell<Option<SidebarRow>>,
}

/// The parts of a sidebar row that change with its state.
pub struct SidebarRow {
    label: Retained<NSTextField>,
    icon: Retained<NSImageView>,
    tint: Retained<NSColor>,
    /// Shown while the entry has changes not saved yet.
    dot: Retained<NSTextField>,
}

define_class!(
    // SAFETY: NSButton has no subclassing requirements and CardButton does
    // not implement Drop.
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CapycalCardButton"]
    #[ivars = CardIvars]
    /// A button drawn by its subviews (a choice card or row): it takes
    /// every click inside it, so its labels do not swallow them.
    struct CardButton;

    impl CardButton {
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let frame = self.frame();
            let inside = point.x >= frame.origin.x
                && point.x < frame.origin.x + frame.size.width
                && point.y >= frame.origin.y
                && point.y < frame.origin.y + frame.size.height;
            if inside && !self.isHidden() {
                (self as *const Self).cast::<NSView>().cast_mut()
            } else {
                std::ptr::null_mut()
            }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().hovered.set(self.isEnabled());
            self.repaint();
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hovered.set(false);
            self.repaint();
        }
    }
);

impl CardButton {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CardIvars::default());
        // SAFETY: NSButton's designated initializer
        let b: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        b.setBordered(false);
        b.setTitle(&NSString::from_str(""));
        // Hover follows the mouse over the whole button, whatever its size
        // SAFETY: the button owns the area; AppKit keeps the owner unretained
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                NSTrackingAreaOptions::MouseEnteredAndExited
                    | NSTrackingAreaOptions::ActiveInKeyWindow
                    | NSTrackingAreaOptions::InVisibleRect,
                Some(&b),
                None,
            )
        };
        b.addTrackingArea(&area);
        b
    }

    /// Draw `background` for this card, in `style`.
    fn set_background(&self, background: &Retained<NSBox>, style: ChoiceStyle) {
        *self.ivars().background.borrow_mut() = Some(background.clone());
        self.ivars().style.set(Some(style));
    }

    fn set_selected(&self, selected: bool) {
        self.ivars().selected.set(selected);
        self.repaint();
    }

    fn repaint(&self) {
        let ivars = self.ivars();
        let (selected, hovered) = (ivars.selected.get(), ivars.hovered.get());
        if let Some(row) = &*ivars.sidebar.borrow() {
            if let Some(background) = &*ivars.background.borrow() {
                background.setFillColor(&*match (selected, hovered) {
                    (true, _) => NSColor::selectedContentBackgroundColor(),
                    (false, true) => NSColor::quaternarySystemFillColor(),
                    (false, false) => NSColor::clearColor(),
                });
            }
            let (text, icon) = if selected {
                (
                    NSColor::alternateSelectedControlTextColor(),
                    NSColor::alternateSelectedControlTextColor(),
                )
            } else {
                (NSColor::labelColor(), row.tint.clone())
            };
            row.label.setTextColor(Some(&text));
            row.icon.setContentTintColor(Some(&icon));
            return;
        }
        if let (Some(background), Some(style)) = (&*ivars.background.borrow(), ivars.style.get()) {
            paint_choice(background, style, selected, hovered);
        }
    }

    /// The icon's color while the row is not selected.
    fn set_tint(&self, tint: Retained<NSColor>) {
        if let Some(row) = &mut *self.ivars().sidebar.borrow_mut() {
            row.tint = tint;
        }
        self.repaint();
    }

    fn set_draft(&self, draft: bool) {
        if let Some(row) = &*self.ivars().sidebar.borrow() {
            row.dot.setHidden(!draft);
        }
    }
}

pub struct HandleIvars {
    target: Retained<SettingsTarget>,
    tag: std::cell::Cell<isize>,
}

define_class!(
    // SAFETY: NSImageView has no subclassing requirements and DragHandle
    // does not implement Drop.
    #[unsafe(super(NSImageView, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CapycalDragHandle"]
    #[ivars = HandleIvars]
    /// The grip of a transform step: reports where the mouse is while it
    /// is dragged, the window works out the gap it points at.
    struct DragHandle;

    impl DragHandle {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            NSCursor::closedHandCursor().push();
            self.report(event, false);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.report(event, false);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            NSCursor::closedHandCursor().pop();
            self.report(event, true);
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &NSCursor::openHandCursor());
        }
    }
);

impl DragHandle {
    fn new(mtm: MainThreadMarker, target: &Retained<SettingsTarget>, tag: isize) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(HandleIvars {
            target: target.clone(),
            tag: std::cell::Cell::new(tag),
        });
        // SAFETY: NSImageView's designated initializer
        let handle: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("line.3.horizontal"),
            Some(&NSString::from_str("Drag to move")),
        ) {
            handle.setImage(Some(&image));
        }
        handle.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
        handle.setToolTip(Some(&NSString::from_str("Drag to move")));
        handle
    }

    fn report(&self, event: &NSEvent, done: bool) {
        let ivars = self.ivars();
        ivars.target.send(RawInput::Drag {
            tag: ivars.tag.get(),
            y: event.locationInWindow().y,
            done,
        });
    }
}

/// A sidebar section: title, entries, the id of an entry, whether they are
/// syncs.
type SidebarSection<'a> = (&'a str, &'a [SidebarItem], fn(String) -> FieldId, bool);

/// What a tag stands for.
#[derive(Debug, Clone, PartialEq)]
enum Tagged {
    Button(FieldId),
    Toggle(FieldId),
    Text(FieldId),
    Popup(FieldId),
    /// A row of a list or an item of choices.
    Row(FieldId, usize),
    /// The drag handle of the transform at this index.
    Drag(usize),
    /// The drag handle of the condition at this index.
    ConditionDrag(usize),
    /// The "+" of a joint, by its index in `joint_menus`.
    Joint(usize),
    /// Move the transform at this index to the gap.
    MoveTo(usize, usize),
}

/// The views of one drawn control, for updates in place.
enum Drawn {
    Label(Retained<NSTextField>),
    Text {
        field: Retained<NSTextField>,
        error: Warning,
    },
    Warning(Warning),
    Check(Retained<NSButton>),
    Popup(Retained<NSPopUpButton>),
    None,
    Header {
        title: Retained<NSTextField>,
        error: Option<Warning>,
        subtitle: Retained<NSTextField>,
        status: Warning,
        switch: Option<Retained<NSSwitch>>,
    },
    Segments(Retained<NSSegmentedControl>),
    Preview {
        mode: Retained<NSSegmentedControl>,
        source: EventLabels,
        copy: EventLabels,
    },
    TransformRow {
        summary: Retained<NSTextField>,
    },
    Footer {
        label: Retained<NSTextField>,
        sign: Retained<NSImageView>,
        progress: Retained<NSProgressIndicator>,
        buttons: Vec<Retained<NSButton>>,
    },
    Hero(Retained<NSTextField>, Retained<NSTextField>),
    /// The background of each item; None for a header.
    Choices(Vec<Option<Retained<CardButton>>>),
    Buttons(Vec<Retained<NSButton>>),
    Disclosure(Retained<NSButton>),
}

pub struct SettingsWindow {
    mtm: MainThreadMarker,
    window: Retained<NSWindow>,
    target: Retained<SettingsTarget>,
    sidebar: Retained<NSStackView>,
    sidebar_material: Retained<objc2_app_kit::NSVisualEffectView>,
    sidebar_width: Retained<NSLayoutConstraint>,
    /// The app's version, at the bottom of the sidebar.
    version: Retained<NSTextField>,
    editor: Retained<NSStackView>,
    aside: Retained<NSStackView>,
    aside_width: Retained<NSLayoutConstraint>,
    /// Where new controls go: the editor or an open group.
    container: Retained<NSStackView>,
    /// The containers around an open `AnyOf` box or condition card.
    outer: Vec<Retained<NSStackView>>,
    /// The open row the next text fields go into, side by side.
    row: Option<Retained<NSStackView>>,
    /// The box of the stage being drawn and the column right of it.
    stage: Option<(Retained<NSView>, Retained<NSStackView>)>,
    /// The background of each step and the step being drawn, so the step
    /// worked in can show a ring.
    stage_boxes: Vec<Retained<NSBox>>,
    tag_stages: Vec<Option<usize>>,
    current_stage: Option<usize>,
    active_stage: std::cell::Cell<Option<usize>>,
    /// The "+" of each joint and its menu.
    joint_menus: Vec<(Retained<NSButton>, Retained<NSMenu>)>,
    /// The fields of a transform opened in this build, to fade in.
    reveal: Vec<Retained<NSView>>,
    revealing: bool,
    open_transform: Option<usize>,
    /// The chevron of each transform row, turned down while it is open.
    chevrons: Vec<Retained<NSImageView>>,
    /// The chevrons turning (index, from, to angle), since when, and the
    /// timer that moves them.
    turning: std::cell::RefCell<Vec<(usize, f64, f64)>>,
    turn_start: std::cell::Cell<Option<std::time::Instant>>,
    turn_timer: std::cell::RefCell<Option<Retained<AnyObject>>>,
    /// The step of each transform, top to bottom, for dragging.
    transform_stages: Vec<Retained<NSView>>,
    /// The drop mark of the joint before each transform (and after the
    /// last).
    drop_marks: Vec<Retained<NSBox>>,
    /// The card of each condition, and the drop mark of each place a
    /// dragged one can land with the conditions it would leave as they are.
    condition_cards: Vec<Retained<NSView>>,
    condition_marks: Vec<(Retained<NSBox>, Vec<usize>)>,
    /// The bar at the bottom: buttons left, a text, buttons right.
    footer: Retained<NSStackView>,
    /// Active while there is no footer.
    footer_height: Retained<NSLayoutConstraint>,
    footer_line: Retained<NSBox>,
    footer_bottom: Retained<NSLayoutConstraint>,
    footer_gap: Retained<NSLayoutConstraint>,
    tags: Vec<Tagged>,
    /// Counts rebuilds, so input of an earlier build is dropped.
    generation: usize,
    drawn: Vec<Drawn>,
    sidebar_buttons: Vec<Retained<CardButton>>,
    general_button: Option<Retained<CardButton>>,
    banner: Option<Retained<NSTextField>>,
    /// Every control with the target set, to clear on drop.
    controls: Vec<Retained<NSView>>,
    last: Option<SettingsView>,
}

impl SettingsWindow {
    pub fn open(
        mtm: MainThreadMarker,
        view: &SettingsView,
        notify: impl Fn(RawInput) + 'static,
    ) -> Result<Self, String> {
        let settings = Self::create(mtm, view, notify)?;
        settings.focus();
        if !view.compact {
            // Becoming key would focus the first field and select its text
            settings.window.makeFirstResponder(None);
        }
        Ok(settings)
    }

    /// `view` (the wizard) as a sheet on `parent`, which takes no input
    /// meanwhile.
    pub fn open_sheet(
        mtm: MainThreadMarker,
        view: &SettingsView,
        notify: impl Fn(RawInput) + 'static,
        parent: &SettingsWindow,
    ) -> Result<Self, String> {
        let sheet = Self::create(mtm, view, notify)?;
        parent.focus();
        parent
            .window
            .beginSheet_completionHandler(&sheet.window, None);
        Ok(sheet)
    }

    fn create(
        mtm: MainThreadMarker,
        view: &SettingsView,
        notify: impl Fn(RawInput) + 'static,
    ) -> Result<Self, String> {
        let target = SettingsTarget::new(mtm, notify);
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::FullSizeContentView;
        // SAFETY: a plain window; it is owned by the Retained, not released on close
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, SIZE),
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str("Settings"));
        // The content reaches under a transparent title bar, as in System
        // Settings; the title stays for the Window menu
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(objc2_app_kit::NSWindowTitleVisibility::Hidden);
        window.setContentMinSize(MIN_SIZE);
        window.setAutorecalculatesKeyViewLoop(true);
        window.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        let content = NSView::new(mtm);
        window.setContentView(Some(&content));

        // The sidebar's translucent material, full height
        let sidebar_material = objc2_app_kit::NSVisualEffectView::new(mtm);
        sidebar_material.setMaterial(objc2_app_kit::NSVisualEffectMaterial::Sidebar);
        sidebar_material.setBlendingMode(objc2_app_kit::NSVisualEffectBlendingMode::BehindWindow);
        sidebar_material.setState(objc2_app_kit::NSVisualEffectState::FollowsWindowActiveState);
        sidebar_material.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&sidebar_material);

        let sidebar = NSStackView::new(mtm);
        sidebar.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        sidebar.setAlignment(NSLayoutAttribute::Leading);
        sidebar.setSpacing(GAP_S);
        sidebar.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&sidebar);

        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setDrawsBackground(false);
        scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&scroll);
        let document = FlippedView::new(mtm);
        document.ivars().replace(Some(target.clone()));
        document.setTranslatesAutoresizingMaskIntoConstraints(false);
        scroll.setDocumentView(Some(&document));
        let editor = NSStackView::new(mtm);
        editor.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        editor.setAlignment(NSLayoutAttribute::Leading);
        editor.setSpacing(GAP_L);
        editor.setTranslatesAutoresizingMaskIntoConstraints(false);
        document.addSubview(&editor);

        let aside = NSStackView::new(mtm);
        aside.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        aside.setAlignment(NSLayoutAttribute::Leading);
        aside.setSpacing(8.0);
        aside.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&aside);
        let aside_width = aside.widthAnchor().constraintEqualToConstant(ASIDE_WIDTH);
        // The preview wraps to the column instead of widening the window
        aside.setClippingResistancePriority_forOrientation(
            250.0,
            NSLayoutConstraintOrientation::Horizontal,
        );

        let footer = NSStackView::new(mtm);
        footer.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&footer);
        let footer_height = footer.heightAnchor().constraintEqualToConstant(0.0);
        // A hairline above the footer, from edge to edge
        let footer_line = NSBox::new(mtm);
        footer_line.setBoxType(NSBoxType::Separator);
        footer_line.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&footer_line);
        let footer_bottom = footer
            .bottomAnchor()
            .constraintEqualToAnchor(&content.bottomAnchor());
        let footer_gap = scroll
            .bottomAnchor()
            .constraintEqualToAnchor(&footer.topAnchor());

        let version = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        version.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        version.setTextColor(Some(&NSColor::secondaryLabelColor()));
        version.setTranslatesAutoresizingMaskIntoConstraints(false);
        version.setContentCompressionResistancePriority_forOrientation(
            1000.0,
            NSLayoutConstraintOrientation::Horizontal,
        );
        content.addSubview(&version);

        let clip = scroll.contentView();
        let sidebar_width = sidebar
            .widthAnchor()
            .constraintEqualToConstant(SIDEBAR_WIDTH);
        // Level with the footer's buttons, also on a page without them
        let button_height = button(mtm, "Save", &target, sel!(pressed:))
            .fittingSize()
            .height;
        let level_with_footer = version.centerYAnchor().constraintEqualToAnchor_constant(
            &content.bottomAnchor(),
            -(MARGIN + button_height / 2.0),
        );
        let constraints = [
            sidebar
                .topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), SIDEBAR_TOP),
            sidebar_material
                .topAnchor()
                .constraintEqualToAnchor(&content.topAnchor()),
            sidebar_material
                .bottomAnchor()
                .constraintEqualToAnchor(&content.bottomAnchor()),
            sidebar_material
                .leadingAnchor()
                .constraintEqualToAnchor(&content.leadingAnchor()),
            sidebar_material
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&sidebar.trailingAnchor(), GAP_M),
            sidebar
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), GAP_M),
            sidebar_width.clone(),
            scroll
                .topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), TITLEBAR),
            footer_height.clone(),
            footer_bottom.clone(),
            footer_gap.clone(),
            footer_line
                .leadingAnchor()
                .constraintEqualToAnchor(&sidebar_material.trailingAnchor()),
            footer_line
                .trailingAnchor()
                .constraintEqualToAnchor(&content.trailingAnchor()),
            footer_line
                .bottomAnchor()
                // As far above the buttons as they are above the window's edge
                .constraintEqualToAnchor_constant(&footer.topAnchor(), -MARGIN),
            footer
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&sidebar_material.trailingAnchor(), MARGIN),
            footer
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -MARGIN),
            scroll
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&sidebar_material.trailingAnchor(), MARGIN),
            scroll
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&aside.leadingAnchor(), -MARGIN),
            aside
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -MARGIN),
            aside
                .topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), TITLEBAR + MARGIN),
            aside_width.clone(),
            document
                .widthAnchor()
                .constraintEqualToAnchor(&clip.widthAnchor()),
            document
                .topAnchor()
                .constraintEqualToAnchor(&clip.topAnchor()),
            document
                .leadingAnchor()
                .constraintEqualToAnchor(&clip.leadingAnchor()),
            sidebar
                .bottomAnchor()
                .constraintLessThanOrEqualToAnchor_constant(&version.topAnchor(), -GAP_M),
            version
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&sidebar.leadingAnchor(), GAP_M),
            level_with_footer,
            editor
                .topAnchor()
                .constraintEqualToAnchor_constant(&document.topAnchor(), MARGIN),
            editor
                .leadingAnchor()
                // Room for the focus ring of a field at the left edge
                .constraintEqualToAnchor_constant(&document.leadingAnchor(), 4.0),
            editor
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&document.trailingAnchor(), -MARGIN),
            editor
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&document.bottomAnchor(), -MARGIN),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
        window.center();

        let mut settings = Self {
            mtm,
            window,
            target,
            sidebar,
            sidebar_material,
            sidebar_width,
            version,
            container: editor.clone(),
            outer: Vec::new(),
            row: None,
            stage: None,
            stage_boxes: Vec::new(),
            tag_stages: Vec::new(),
            current_stage: None,
            active_stage: std::cell::Cell::new(None),
            joint_menus: Vec::new(),
            reveal: Vec::new(),
            revealing: false,
            open_transform: None,
            chevrons: Vec::new(),
            turning: Default::default(),
            turn_start: Default::default(),
            turn_timer: Default::default(),
            transform_stages: Vec::new(),
            drop_marks: Vec::new(),
            condition_cards: Vec::new(),
            condition_marks: Vec::new(),
            editor,
            aside,
            aside_width,
            footer,
            footer_height,
            footer_line,
            footer_bottom,
            footer_gap,
            tags: Vec::new(),
            generation: 0,
            drawn: Vec::new(),
            sidebar_buttons: Vec::new(),
            general_button: None,
            banner: None,
            controls: Vec::new(),
            last: None,
        };
        settings.show(view);
        Ok(settings)
    }

    pub fn focus(&self) {
        focus(&self.window);
    }

    /// The input a control reported, or None for a stale tag.
    pub fn translate(&self, raw: RawInput) -> Option<SettingsInput> {
        // A tag of an earlier build (input queued before a rebuild) is stale
        let tagged = |tag: isize| {
            let tag = usize::try_from(tag).ok()?;
            (tag / TAG_GENERATION == self.generation % 1000)
                .then(|| self.tags.get(tag % TAG_GENERATION))
                .flatten()
        };
        // The step a control belongs to shows a ring while it is worked in
        let tag_of = match &raw {
            RawInput::Pressed { tag, .. }
            | RawInput::Text { tag, .. }
            | RawInput::Chose { tag, .. }
            | RawInput::Segment { tag, .. }
            | RawInput::Focus { tag }
            | RawInput::Left { tag }
            | RawInput::Drag { tag, .. } => Some(*tag),
            RawInput::MenuItem { tag } => Some(tag / 100),
            RawInput::CloseRequested | RawInput::Tick | RawInput::Shortcut(_) => None,
        };
        if let Some(tag) = tag_of.and_then(|t| usize::try_from(t).ok())
            && tag / TAG_GENERATION == self.generation % 1000
        {
            self.show_active(self.tag_stages.get(tag % TAG_GENERATION).copied().flatten());
        }
        Some(match raw {
            RawInput::CloseRequested => SettingsInput::CloseRequested,
            RawInput::Shortcut(shortcut) => SettingsInput::Shortcut(shortcut),
            RawInput::Focus { .. } => return None,
            RawInput::Left { tag } => match tagged(tag)? {
                Tagged::Text(id) => SettingsInput::Left(id.clone()),
                _ => return None,
            },
            RawInput::Tick => {
                self.step_chevrons();
                return None;
            }
            RawInput::MenuItem { tag } => match tagged(tag / 100)? {
                Tagged::Popup(id) => {
                    SettingsInput::Chose(id.clone(), usize::try_from(tag % 100).ok()?, None)
                }
                _ => return None,
            },
            RawInput::Pressed { tag, .. } if matches!(tagged(tag), Some(Tagged::Joint(_))) => {
                let Some(Tagged::Joint(i)) = tagged(tag) else {
                    return None;
                };
                let (button, menu) = self.joint_menus.get(*i)?;
                // Below the "+", centered on it
                let at = NSPoint::new(0.0, button.bounds().size.height + 4.0);
                menu.popUpMenuPositioningItem_atLocation_inView(None, at, Some(button));
                return None;
            }
            RawInput::Pressed { tag, on } => match tagged(tag)? {
                Tagged::Button(id) => SettingsInput::Pressed(id.clone()),
                Tagged::MoveTo(from, gap) => {
                    SettingsInput::Moved(FieldId::TransformRow(*from), *gap)
                }
                Tagged::Toggle(id) => SettingsInput::Toggled(id.clone(), on),
                Tagged::Row(id, row) => SettingsInput::Selected(id.clone(), *row),
                _ => return None,
            },
            RawInput::Text { tag, text } => match tagged(tag)? {
                Tagged::Text(id) => SettingsInput::Text(id.clone(), text),
                _ => return None,
            },
            RawInput::Drag { tag, y, done } => match tagged(tag)? {
                Tagged::Drag(from) => {
                    let gap = drop_gap(&self.transform_stages, y);
                    // Next to itself it stays where it is
                    let shown = (!done)
                        .then_some(gap)
                        .filter(|g| *g != *from && *g != from + 1);
                    show_drag(
                        &self.transform_stages,
                        &self.drop_marks,
                        *from,
                        !done,
                        shown,
                    );
                    if !done {
                        return None;
                    }
                    SettingsInput::Moved(FieldId::TransformRow(*from), gap)
                }
                Tagged::ConditionDrag(from) => {
                    let slot = self.condition_slot(*from, y);
                    let marks: Vec<Retained<NSBox>> = self
                        .condition_marks
                        .iter()
                        .map(|(m, _)| m.clone())
                        .collect();
                    let shown = slot.filter(|_| !done);
                    show_drag(&self.condition_cards, &marks, *from, !done, shown);
                    if !done {
                        return None;
                    }
                    SettingsInput::Moved(FieldId::Condition(*from), slot?)
                }
                _ => return None,
            },
            RawInput::Segment { tag, index } => match tagged(tag)? {
                Tagged::Row(id, _) => {
                    SettingsInput::Selected(id.clone(), usize::try_from(index).ok()?)
                }
                _ => return None,
            },
            RawInput::Chose { tag, index } => match tagged(tag)? {
                Tagged::Popup(id) => {
                    let index = usize::try_from(index).ok()?;
                    // The title the menu showed, from the view it was drawn from
                    let title = self.last.as_ref().and_then(|view| {
                        view.controls.iter().find_map(|c| match c {
                            Control::Popup {
                                id: shown, items, ..
                            } if shown == id => items.get(index).map(|i| i.title.clone()),
                            _ => None,
                        })
                    });
                    SettingsInput::Chose(id.clone(), index, title)
                }
                _ => return None,
            },
        })
    }

    /// Draw `view`: in place while its shape stays the same, so a field
    /// being typed in keeps its focus; else from scratch.
    pub fn show(&mut self, view: &SettingsView) {
        // The wizard shows no sidebar, in a smaller window
        if self.last.as_ref().is_none_or(|l| l.compact != view.compact) {
            // The full window keeps the size and place it was given
            if self.last.as_ref().is_some_and(|l| !l.compact) {
                self.window
                    .saveFrameUsingName(&NSString::from_str(FRAME_NAME));
            }
            self.sidebar.setHidden(view.compact);
            self.sidebar_material.setHidden(view.compact);
            self.version.setHidden(view.compact);
            self.version
                .setStringValue(&NSString::from_str(&view.version));
            self.sidebar_width
                .setConstant(if view.compact { 0.0 } else { SIDEBAR_WIDTH });
            if view.compact
                || !self
                    .window
                    .setFrameUsingName(&NSString::from_str(FRAME_NAME))
            {
                self.window
                    .setContentSize(if view.compact { COMPACT_SIZE } else { SIZE });
                self.window.center();
            }
        }
        self.window.setTitle(&NSString::from_str(&view.title));
        let names = |items: &[SidebarItem]| -> Vec<String> {
            items.iter().map(|i| i.name.clone()).collect()
        };
        let in_place = self.last.as_ref().is_some_and(|last| {
            SettingsScreen::same_shape(last, view)
                && names(&last.syncs) == names(&view.syncs)
                && names(&last.accounts) == names(&view.accounts)
        });
        if in_place {
            self.update(view);
        } else {
            self.rebuild(view);
        }
        self.last = Some(view.clone());
    }

    /// A ring around the step being worked in, none around the others.
    fn show_active(&self, stage: Option<usize>) {
        self.active_stage.set(stage);
        for (i, background) in self.stage_boxes.iter().enumerate() {
            background.setBorderWidth(if stage == Some(i) { 1.5 } else { 0.0 });
        }
    }

    /// Turns each chevron down while its row is open; the ones that
    /// changed turn there from where they pointed before (`was` open).
    /// AppKit animates no rotation of a view, so a timer moves them.
    fn turn_chevrons(&self, was: Option<usize>, now: Option<usize>) {
        let angle = |open: bool| if open { -90.0 } else { 0.0 };
        let reduce = reduce_motion();
        let mut turning = Vec::new();
        for (i, chevron) in self.chevrons.iter().enumerate() {
            let (from, to) = (angle(was == Some(i)), angle(now == Some(i)));
            if from == to || reduce {
                chevron.setFrameCenterRotation(to);
            } else {
                chevron.setFrameCenterRotation(from);
                turning.push((i, from, to));
            }
        }
        if turning.is_empty() {
            return;
        }
        *self.turning.borrow_mut() = turning;
        self.turn_start.set(Some(std::time::Instant::now()));
        // SAFETY: a repeating timer on the main run loop calling the target,
        // invalidated when the turn ends or the window goes
        let timer: Retained<AnyObject> = unsafe {
            msg_send![
                objc2::class!(NSTimer),
                scheduledTimerWithTimeInterval: 1.0f64 / 60.0,
                target: &*self.target,
                selector: sel!(tick:),
                userInfo: None::<&AnyObject>,
                repeats: true
            ]
        };
        *self.turn_timer.borrow_mut() = Some(timer);
    }

    fn step_chevrons(&self) {
        const DURATION: f64 = 0.2;
        let Some(start) = self.turn_start.get() else {
            return;
        };
        let t = (start.elapsed().as_secs_f64() / DURATION).min(1.0);
        // Ease in and out
        let eased = t * t * (3.0 - 2.0 * t);
        for (i, from, to) in self.turning.borrow().iter() {
            if let Some(chevron) = self.chevrons.get(*i) {
                chevron.setFrameCenterRotation(from + (to - from) * eased);
            }
        }
        if t >= 1.0 {
            self.stop_turning();
        }
    }

    fn stop_turning(&self) {
        if let Some(timer) = self.turn_timer.borrow_mut().take() {
            // SAFETY: an NSTimer this window scheduled
            let _: () = unsafe { msg_send![&timer, invalidate] };
        }
        self.turn_start.set(None);
        self.turning.borrow_mut().clear();
    }

    /// Fade in the fields of a transform that was just opened.
    fn fade_in(&self) {
        if reduce_motion() || self.reveal.is_empty() {
            return;
        }
        for view in &self.reveal {
            view.setAlphaValue(0.0);
        }
        // SAFETY: an animation group around animator proxies of our views
        unsafe {
            let _: () = msg_send![objc2::class!(NSAnimationContext), beginGrouping];
            let context: Retained<AnyObject> =
                msg_send![objc2::class!(NSAnimationContext), currentContext];
            let _: () = msg_send![&context, setDuration: 0.2f64];
            for view in &self.reveal {
                let animator: Retained<AnyObject> = msg_send![&**view, animator];
                let _: () = msg_send![&animator, setAlphaValue: 1.0f64];
            }
            let _: () = msg_send![objc2::class!(NSAnimationContext), endGrouping];
        }
    }

    /// `view` with the gutter column right of it; returns the row and the
    /// column.
    fn with_gutter(&self, view: &NSView) -> (Retained<NSStackView>, Retained<NSStackView>) {
        let gutter = NSStackView::new(self.mtm);
        gutter.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        gutter
            .widthAnchor()
            .constraintEqualToConstant(GUTTER)
            .setActive(true);
        let row = NSStackView::new(self.mtm);
        row.setAlignment(NSLayoutAttribute::CenterY);
        row.setSpacing(4.0);
        // The view takes all the width the gutter leaves
        row.setDistribution(objc2_app_kit::NSStackViewDistribution::Fill);
        row.addArrangedSubview(view);
        row.addArrangedSubview(&gutter);
        view.setContentHuggingPriority_forOrientation(
            1.0,
            NSLayoutConstraintOrientation::Horizontal,
        );
        (row, gutter)
    }

    fn tag(&mut self, tagged: Tagged) -> isize {
        self.tags.push(tagged);
        self.tag_stages.push(self.current_stage);
        ((self.generation % 1000) * TAG_GENERATION + self.tags.len() - 1) as isize
    }

    fn label(&self, text: &str) -> Retained<NSTextField> {
        NSTextField::wrappingLabelWithString(&NSString::from_str(text), self.mtm)
    }

    fn push_button(&mut self, title: &str, tagged: Tagged) -> Retained<NSButton> {
        let tag = self.tag(tagged);
        let b = button(self.mtm, title, &self.target, sel!(pressed:));
        b.setTag(tag);
        self.controls
            .push(Retained::into_super(Retained::into_super(b.clone())));
        b
    }

    /// A row with a right-aligned label and `control` filling the rest.
    fn labeled(&self, label: &str, control: &NSView) -> Retained<NSStackView> {
        let label = NSTextField::labelWithString(&NSString::from_str(label), self.mtm);
        label
            .widthAnchor()
            .constraintEqualToConstant(LABEL_WIDTH)
            .setActive(true);
        let row = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[label.as_ref() as &NSView, control]),
            self.mtm,
        );
        row.setAlignment(NSLayoutAttribute::FirstBaseline);
        row.setClippingResistancePriority_forOrientation(
            250.0,
            NSLayoutConstraintOrientation::Horizontal,
        );
        row
    }

    /// The drop mark nearest to `y` (window coordinates) for condition
    /// `from`, unless that one leaves it where it is.
    fn condition_slot(&self, from: usize, y: f64) -> Option<usize> {
        let distance = |mark: &NSBox| {
            let frame = mark.convertRect_toView(mark.bounds(), None);
            (frame.origin.y + frame.size.height / 2.0 - y).abs()
        };
        let (slot, (_, stays)) = self
            .condition_marks
            .iter()
            .enumerate()
            .min_by(|(_, (a, _)), (_, (b, _))| distance(a).total_cmp(&distance(b)))?;
        (!stays.contains(&from)).then_some(slot)
    }

    /// Where a dragged condition would land, clear until then.
    fn condition_mark(&mut self, stays: Vec<usize>) {
        let mark = NSBox::new(self.mtm);
        mark.setBoxType(NSBoxType::Custom);
        mark.setTitlePosition(NSTitlePosition::NoTitle);
        mark.setBorderWidth(0.0);
        mark.setCornerRadius(1.5);
        mark.setFillColor(&NSColor::clearColor());
        mark.heightAnchor()
            .constraintEqualToConstant(3.0)
            .setActive(true);
        self.add(&mark);
        self.condition_marks.push((mark, stays));
    }

    fn add(&mut self, view: &NSView) {
        if self.revealing {
            self.reveal.push(objc2::Message::retain(view));
        }
        self.container.addArrangedSubview(view);
        view.widthAnchor()
            .constraintEqualToAnchor(&self.container.widthAnchor())
            .setActive(true);
    }

    fn clear_targets(&mut self) {
        for control in self.controls.drain(..) {
            // SAFETY: every entry is an NSControl; clearing its target and
            // delegate is always valid
            unsafe {
                let _: () = msg_send![&*control, setTarget: None::<&AnyObject>];
                if control.isKindOfClass(NSTextField::class()) {
                    let _: () = msg_send![&*control, setDelegate: None::<&AnyObject>];
                }
            }
        }
    }

    fn rebuild(&mut self, view: &SettingsView) {
        self.clear_targets();
        self.tags.clear();
        self.generation += 1;
        self.drawn.clear();
        self.sidebar_buttons.clear();
        self.container = self.editor.clone();
        self.row = None;
        self.stage = None;
        self.stage_boxes.clear();
        self.tag_stages.clear();
        self.current_stage = None;
        self.joint_menus.clear();
        self.reveal.clear();
        self.stop_turning();
        self.chevrons.clear();
        self.revealing = false;
        self.transform_stages.clear();
        self.drop_marks.clear();
        self.condition_cards.clear();
        self.condition_marks.clear();
        self.outer.clear();
        for stack in [&self.sidebar, &self.editor, &self.footer, &self.aside] {
            for old in stack.arrangedSubviews().iter() {
                stack.removeArrangedSubview(&old);
                old.removeFromSuperview();
            }
        }
        self.rebuild_sidebar(view);
        let footer = view
            .controls
            .iter()
            .any(|c| matches!(c, Control::Footer { .. }));
        self.footer_height.setActive(!footer);
        self.footer_line.setHidden(!footer);
        let aside = view
            .controls
            .iter()
            .any(|c| matches!(c, Control::Preview { .. }));
        self.aside.setHidden(!aside);
        self.aside_width
            .setConstant(if aside { ASIDE_WIDTH } else { 0.0 });
        self.footer_bottom
            .setConstant(if footer { -MARGIN } else { 0.0 });
        self.footer_gap
            .setConstant(if footer { -MARGIN } else { 0.0 });
        if let Some(banner) = &view.banner {
            let label = self.label(banner);
            label.setDrawsBackground(true);
            label.setBackgroundColor(Some(&NSColor::systemYellowColor()));
            self.add(&label);
            self.banner = Some(label);
        } else {
            self.banner = None;
        }
        for control in &view.controls {
            let drawn = self.draw(control);
            self.drawn.push(drawn);
        }
        let active = self
            .active_stage
            .get()
            .filter(|s| *s < self.stage_boxes.len());
        self.show_active(active);
        let open = view.controls.iter().find_map(|c| match c {
            Control::TransformRow {
                index, open: true, ..
            } => Some(*index),
            _ => None,
        });
        if open.is_some() && open != self.open_transform {
            self.fade_in();
        }
        // Tab follows the new fields in the order they are shown
        if let Some(content) = self.window.contentView() {
            content.layoutSubtreeIfNeeded();
        }
        self.turn_chevrons(self.open_transform, open);
        self.open_transform = open;
        self.window.recalculateKeyViewLoop();
        if view.compact {
            if let Some(Drawn::Text { field, .. }) =
                self.drawn.iter().find(|d| matches!(d, Drawn::Text { .. }))
            {
                self.window.makeFirstResponder(Some(field));
            }
        } else {
            // No field takes the focus by itself, where a key would replace
            // its text
            self.window.makeFirstResponder(None);
        }
    }

    fn rebuild_sidebar(&mut self, view: &SettingsView) {
        // The app's own settings come first
        let general = self.sidebar_row(
            &view.general.name,
            "gearshape",
            general_tint(view.general.problem),
            Tagged::Button(FieldId::SidebarGeneral),
        );
        general.set_selected(view.general.selected);
        self.general_button = Some(general);
        let sections: [SidebarSection; 2] = [
            ("Syncs", &view.syncs, FieldId::SidebarSync, true),
            ("Accounts", &view.accounts, FieldId::SidebarAccount, false),
        ];
        for (title, entries, id, sync) in sections {
            let heading = NSTextField::labelWithString(&NSString::from_str(title), self.mtm);
            heading.setFont(Some(&NSFont::systemFontOfSize_weight(11.0, 0.3)));
            heading.setTextColor(Some(&NSColor::secondaryLabelColor()));
            // Aligned with the rows' text, set off from the section above
            let indented = NSStackView::new(self.mtm);
            indented.setEdgeInsets(objc2_foundation::NSEdgeInsets {
                top: 0.0,
                left: GAP_M,
                bottom: 0.0,
                right: 0.0,
            });
            indented.addArrangedSubview(&heading);
            if let Some(last) = self.sidebar.arrangedSubviews().lastObject() {
                self.sidebar.setCustomSpacing_afterView(GAP_XL, &last);
            }
            self.sidebar.addArrangedSubview(&indented);
            for item in entries {
                let row = self.sidebar_row(
                    &item.name,
                    sidebar_symbol(sync),
                    icon_tint(sync, item.active, item.problem),
                    Tagged::Button(id(item.name.clone())),
                );
                row.set_selected(item.selected);
                row.set_draft(item.draft);
                if sync {
                    // Right click
                    let menu = NSMenu::new(self.mtm);
                    let tag = self.tag(Tagged::Button(FieldId::DuplicateSync(item.name.clone())));
                    // SAFETY: the target outlives the menu; cleared on drop
                    let duplicate = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            NSMenuItem::alloc(self.mtm),
                            &NSString::from_str("Duplicate"),
                            Some(sel!(pressed:)),
                            &NSString::from_str(""),
                        )
                    };
                    // SAFETY: the target outlives the menu item
                    unsafe { duplicate.setTarget(Some(&self.target)) };
                    duplicate.setTag(tag);
                    menu.addItem(&duplicate);
                    // SAFETY: the row keeps the menu; its item's target is
                    // cleared on drop
                    unsafe { row.setMenu(Some(&menu)) };
                }
                self.sidebar_buttons.push(row);
            }
            let (label, add) = if sync {
                ("Add Sync\u{2026}", FieldId::AddSync)
            } else {
                ("Add Account\u{2026}", FieldId::AddAccount)
            };
            let accent = NSColor::controlAccentColor();
            self.sidebar_row(label, "plus.circle", accent, Tagged::Button(add));
        }
    }

    /// A source list row: an icon and a title, a rounded fill when
    /// selected or under the mouse.
    fn sidebar_row(
        &mut self,
        title: &str,
        symbol: &str,
        tint: Retained<NSColor>,
        tagged: Tagged,
    ) -> Retained<CardButton> {
        let mtm = self.mtm;
        let row = CardButton::new(mtm);
        let tag = self.tag(tagged);
        row.setTag(tag);
        // SAFETY: the target outlives the button; cleared on drop
        unsafe {
            row.setTarget(Some(&self.target));
            row.setAction(Some(sel!(pressed:)));
        }
        self.controls
            .push(Retained::into_super(Retained::into_super(
                Retained::into_super(row.clone()),
            )));
        let icon = symbol_view(mtm, symbol);
        for anchor in [icon.widthAnchor(), icon.heightAnchor()] {
            anchor.constraintEqualToConstant(16.0).setActive(true);
        }
        let label = NSTextField::labelWithString(&NSString::from_str(title), mtm);
        label.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
        label.setContentCompressionResistancePriority_forOrientation(
            250.0,
            NSLayoutConstraintOrientation::Horizontal,
        );
        let dot = NSTextField::labelWithString(&NSString::from_str("\u{25cf}"), mtm);
        dot.setFont(Some(&NSFont::systemFontOfSize(8.0)));
        dot.setTextColor(Some(&NSColor::secondaryLabelColor()));
        dot.setToolTip(Some(&NSString::from_str("Not saved yet")));
        dot.setHidden(true);
        let line = NSStackView::new(mtm);
        line.setSpacing(GAP_M);
        line.addArrangedSubview(&icon);
        line.addArrangedSubview(&label);
        line.addArrangedSubview(&dot);
        let (inner, background) = backed(mtm, &line, GAP_M, 5.0);
        background.setCornerRadius(6.0);
        background.setBorderWidth(0.0);
        inner.setTranslatesAutoresizingMaskIntoConstraints(false);
        row.addSubview(&inner);
        let constraints = [
            inner.topAnchor().constraintEqualToAnchor(&row.topAnchor()),
            inner
                .bottomAnchor()
                .constraintEqualToAnchor(&row.bottomAnchor()),
            inner
                .leadingAnchor()
                .constraintEqualToAnchor(&row.leadingAnchor()),
            inner
                .trailingAnchor()
                .constraintEqualToAnchor(&row.trailingAnchor()),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
        *row.ivars().background.borrow_mut() = Some(background);
        *row.ivars().sidebar.borrow_mut() = Some(SidebarRow {
            label,
            icon,
            tint,
            dot,
        });
        row.repaint();
        self.sidebar.addArrangedSubview(&row);
        row.widthAnchor()
            .constraintEqualToAnchor(&self.sidebar.widthAnchor())
            .setActive(true);
        row
    }

    fn draw(&mut self, control: &Control) -> Drawn {
        match control {
            Control::Note(text) => {
                let label = self.label(text);
                label.setTextColor(Some(&NSColor::secondaryLabelColor()));
                label.setHidden(text.is_empty());
                self.add(&label);
                Drawn::Label(label)
            }
            Control::Error(text) => {
                let warning = Warning::new(self.mtm, 0.0);
                warning.show(Some(text.as_str()).filter(|t| !t.is_empty()));
                self.add(&warning.row);
                Drawn::Warning(warning)
            }
            Control::Text {
                id,
                label,
                value,
                placeholder,
                error,
                secure,
                enabled,
                suggestions,
            } => {
                let field: Retained<NSTextField> = if *secure {
                    Retained::into_super(NSSecureTextField::new(self.mtm))
                } else if !suggestions.is_empty() {
                    // Offers the suggestions, takes any other text too
                    let combo = NSComboBox::new(self.mtm);
                    for suggestion in suggestions {
                        // SAFETY: an NSString is a valid object value
                        unsafe { combo.addItemWithObjectValue(&NSString::from_str(suggestion)) };
                    }
                    combo.setCompletes(true);
                    combo.setNumberOfVisibleItems(suggestions.len() as isize);
                    Retained::into_super(combo)
                } else {
                    NSTextField::new(self.mtm)
                };
                field.setStringValue(&NSString::from_str(value));
                field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
                field.setEnabled(*enabled);
                // One line that scrolls: a long address must not wrap
                field.setUsesSingleLineMode(true);
                if let Some(cell) = field.cell() {
                    cell.setWraps(false);
                    cell.setScrollable(true);
                }
                let tag = self.tag(Tagged::Text(id.clone()));
                field.setTag(tag);
                // SAFETY: the target outlives the field; both are cleared on drop
                unsafe {
                    field.setTarget(Some(&self.target));
                    field.setAction(Some(sel!(edited:)));
                    field.setDelegate(Some(ProtocolObject::from_ref(&*self.target)));
                }
                self.controls
                    .push(Retained::into_super(Retained::into_super(field.clone())));
                if let Some(row) = &self.row {
                    // A short field with its label after it
                    field
                        .widthAnchor()
                        .constraintEqualToConstant(64.0)
                        .setActive(true);
                    let after = NSTextField::labelWithString(&NSString::from_str(label), self.mtm);
                    row.addArrangedSubview(&field);
                    row.addArrangedSubview(&after);
                    row.setCustomSpacing_afterView(GAP_S + 2.0, &field);
                    row.setCustomSpacing_afterView(GAP_XL, &after);
                } else {
                    let row = self.labeled(label, &field);
                    self.add(&row);
                }
                // Under the field it is about
                let indent = if self.row.is_some() {
                    0.0
                } else {
                    LABEL_WIDTH + GAP_M
                };
                let warning = Warning::new(self.mtm, indent);
                warning.show(error.as_deref());
                self.add(&warning.row);
                Drawn::Text {
                    field,
                    error: warning,
                }
            }
            Control::Check {
                id,
                label,
                on,
                hint,
            } => {
                let tag = self.tag(Tagged::Toggle(id.clone()));
                // SAFETY: the target outlives the button; cleared on drop
                let check = unsafe {
                    NSButton::checkboxWithTitle_target_action(
                        &NSString::from_str(label),
                        Some(&self.target),
                        Some(sel!(pressed:)),
                        self.mtm,
                    )
                };
                check.setTag(tag);
                check.setState(if *on {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                if let Some(hint) = hint {
                    check.setToolTip(Some(&NSString::from_str(hint)));
                }
                self.controls
                    .push(Retained::into_super(Retained::into_super(check.clone())));
                self.add(&check);
                Drawn::Check(check)
            }
            Control::Popup {
                id,
                label,
                items,
                selected,
                buttons,
            } => {
                let popup = NSPopUpButton::new(self.mtm);
                let tag = self.tag(Tagged::Popup(id.clone()));
                popup.setTag(tag);
                // SAFETY: the target outlives the popup; cleared on drop
                unsafe {
                    popup.setTarget(Some(&self.target));
                    popup.setAction(Some(sel!(chose:)));
                }
                self.fill_popup(&popup, items, *selected);
                self.controls
                    .push(Retained::into_super(Retained::into_super(
                        Retained::into_super(popup.clone()),
                    )));
                let line = NSStackView::new(self.mtm);
                line.addArrangedSubview(&popup);
                popup.setContentHuggingPriority_forOrientation(
                    1.0,
                    NSLayoutConstraintOrientation::Horizontal,
                );
                // A long item (an error, a long calendar name) shortens the
                // popup's title instead of widening the window
                popup.setContentCompressionResistancePriority_forOrientation(
                    100.0,
                    NSLayoutConstraintOrientation::Horizontal,
                );
                line.setClippingResistancePriority_forOrientation(
                    250.0,
                    NSLayoutConstraintOrientation::Horizontal,
                );
                for spec in buttons {
                    let b = self.spec_button(spec);
                    line.addArrangedSubview(&b);
                }
                if label.is_empty() {
                    self.add(&line);
                } else {
                    let row = self.labeled(label, &line);
                    self.add(&row);
                }
                Drawn::Popup(popup)
            }
            Control::Buttons(buttons) => {
                let row = NSStackView::new(self.mtm);
                let mut drawn = Vec::new();
                let gravity = if self.container == self.editor {
                    NSStackViewGravity::Leading
                } else {
                    NSStackViewGravity::Trailing
                };
                for spec in buttons {
                    let b = self.pill_button(spec);
                    row.addView_inGravity(&b, gravity);
                    drawn.push(b);
                }
                self.add(&row);
                Drawn::Buttons(drawn)
            }
            Control::Footer {
                left,
                text,
                warning,
                progress,
                right,
            } => {
                let mut drawn = Vec::new();
                for spec in left {
                    let b = self.pill_button(spec);
                    if spec.0 == FieldId::WizardCancel {
                        b.setKeyEquivalent(&NSString::from_str("\u{1b}"));
                    }
                    self.footer
                        .addView_inGravity(&b, NSStackViewGravity::Leading);
                    drawn.push(b);
                }
                let label = NSTextField::labelWithString(&NSString::from_str(text), self.mtm);
                label.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
                label.setContentCompressionResistancePriority_forOrientation(
                    100.0,
                    NSLayoutConstraintOrientation::Horizontal,
                );
                // What keeps Save off: a sign before it, in the text color
                let sign = symbol_view(self.mtm, "exclamationmark.triangle.fill");
                sign.setContentTintColor(Some(&NSColor::systemOrangeColor()));
                show_footer_text(&label, &sign, text, *warning);
                // Next to the buttons it is about
                self.footer
                    .addView_inGravity(&sign, NSStackViewGravity::Trailing);
                self.footer.setCustomSpacing_afterView(GAP_S + 2.0, &sign);
                self.footer
                    .addView_inGravity(&label, NSStackViewGravity::Trailing);
                // How far a removal of synced events is
                let bar = NSProgressIndicator::new(self.mtm);
                bar.setStyle(NSProgressIndicatorStyle::Bar);
                bar.setIndeterminate(false);
                bar.setMinValue(0.0);
                bar.setMaxValue(1.0);
                bar.setControlSize(objc2_app_kit::NSControlSize::Small);
                bar.widthAnchor()
                    .constraintEqualToConstant(100.0)
                    .setActive(true);
                accessibility_label(&bar, "Removing synced events");
                show_progress(&bar, *progress);
                self.footer
                    .addView_inGravity(&bar, NSStackViewGravity::Trailing);
                for (n, spec) in right.iter().enumerate() {
                    let b = self.pill_button(spec);
                    if n + 1 == right.len() {
                        // The default button: blue, and Return presses it
                        b.setKeyEquivalent(&NSString::from_str("\r"));
                    }
                    self.footer
                        .addView_inGravity(&b, NSStackViewGravity::Trailing);
                    drawn.push(b);
                }
                Drawn::Footer {
                    label,
                    sign,
                    progress: bar,
                    buttons: drawn,
                }
            }
            Control::Group(title) => {
                if !title.is_empty() {
                    let label = self.label(title);
                    label.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
                    self.add(&label);
                }
                let rows = NSStackView::new(self.mtm);
                rows.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
                rows.setAlignment(NSLayoutAttribute::Leading);
                rows.setSpacing(GAP_M);
                let (group, background) = backed(self.mtm, &rows, GAP_L + GAP_S, GAP_L);
                background.setCornerRadius(10.0);
                background.setBorderWidth(0.0);
                background.setFillColor(&NSColor::quaternarySystemFillColor());
                // Below a sync's steps, as wide as they are
                if self.stage_boxes.is_empty() {
                    self.add(&group);
                } else {
                    let (row, _) = self.with_gutter(&group);
                    self.add(&row);
                }
                self.container = rows;
                Drawn::None
            }
            Control::Stage { label, .. } => {
                let rows = NSStackView::new(self.mtm);
                rows.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
                rows.setAlignment(NSLayoutAttribute::Leading);
                rows.setSpacing(GAP_M);
                let kind = NSTextField::labelWithString(&NSString::from_str(label), self.mtm);
                kind.setFont(Some(&NSFont::systemFontOfSize_weight(11.0, 0.3)));
                kind.setTextColor(Some(&NSColor::secondaryLabelColor()));
                rows.addArrangedSubview(&kind);
                rows.setCustomSpacing_afterView(GAP_S, &kind);
                let (stage, background) = backed(self.mtm, &rows, GAP_L + GAP_S, GAP_L);
                background.setCornerRadius(10.0);
                background.setBorderWidth(0.0);
                background.setBorderColor(&NSColor::controlAccentColor());
                background.setFillColor(&NSColor::quaternarySystemFillColor());
                let (row, gutter) = self.with_gutter(&stage);
                self.add(&row);
                self.current_stage = Some(self.stage_boxes.len());
                self.stage_boxes.push(background);
                self.stage = Some((stage, gutter));
                self.container = rows;
                Drawn::None
            }
            Control::Row(label) => {
                let lead = NSTextField::labelWithString(&NSString::from_str(label), self.mtm);
                lead.widthAnchor()
                    .constraintEqualToConstant(LABEL_WIDTH)
                    .setActive(true);
                let row = NSStackView::stackViewWithViews(
                    &NSArray::from_slice(&[lead.as_ref() as &NSView]),
                    self.mtm,
                );
                row.setAlignment(NSLayoutAttribute::FirstBaseline);
                self.add(&row);
                self.row = Some(row);
                Drawn::None
            }
            Control::RowEnd => {
                self.row = None;
                Drawn::None
            }
            Control::AnyOf(label) => {
                let rows = NSStackView::new(self.mtm);
                rows.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
                rows.setAlignment(NSLayoutAttribute::Leading);
                rows.setSpacing(GAP_M);
                let kind = NSTextField::labelWithString(&NSString::from_str(label), self.mtm);
                kind.setFont(Some(&NSFont::systemFontOfSize_weight(11.0, 0.3)));
                kind.setTextColor(Some(&NSColor::secondaryLabelColor()));
                rows.addArrangedSubview(&kind);
                rows.setCustomSpacing_afterView(GAP_S, &kind);
                let (group, background) = backed(self.mtm, &rows, GAP_M, GAP_M);
                background.setCornerRadius(8.0);
                background.setBorderWidth(1.0);
                background.setBorderColor(&NSColor::separatorColor());
                background.setFillColor(&NSColor::clearColor());
                self.add(&group);
                self.outer
                    .push(std::mem::replace(&mut self.container, rows));
                Drawn::None
            }
            Control::AnyOfEnd | Control::ConditionEnd => {
                if let Some(outer) = self.outer.pop() {
                    self.container = outer;
                }
                Drawn::None
            }
            Control::DropMark { stays, .. } => {
                self.condition_mark(stays.clone());
                Drawn::None
            }
            Control::Condition(id) => {
                let FieldId::Condition(index) = *id else {
                    return Drawn::None;
                };
                let rows = NSStackView::new(self.mtm);
                rows.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
                rows.setAlignment(NSLayoutAttribute::Leading);
                rows.setSpacing(GAP_M);
                let tag = self.tag(Tagged::ConditionDrag(index));
                let handle = DragHandle::new(self.mtm, &self.target, tag);
                let card = NSStackView::new(self.mtm);
                card.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
                card.setAlignment(NSLayoutAttribute::CenterY);
                card.setSpacing(GAP_S);
                card.addArrangedSubview(&rows);
                card.addArrangedSubview(&handle);
                // The fields take all the room left of the handle
                handle
                    .widthAnchor()
                    .constraintEqualToConstant(16.0)
                    .setActive(true);
                rows.widthAnchor()
                    .constraintEqualToAnchor_constant(&card.widthAnchor(), -(16.0 + GAP_S))
                    .setActive(true);
                let (group, background) = backed(self.mtm, &card, GAP_M, GAP_M);
                background.setCornerRadius(8.0);
                background.setBorderWidth(0.0);
                background.setFillColor(&NSColor::labelColor().colorWithAlphaComponent(0.04));
                self.add(&group);
                self.condition_cards.push(group);
                self.outer
                    .push(std::mem::replace(&mut self.container, rows));
                Drawn::None
            }
            Control::GroupEnd => {
                self.container = self.editor.clone();
                self.current_stage = None;
                self.revealing = false;
                Drawn::None
            }
            Control::Joint { id, items } => {
                let mtm = self.mtm;
                let line = || {
                    let line = NSBox::new(mtm);
                    line.setBoxType(NSBoxType::Custom);
                    line.setTitlePosition(NSTitlePosition::NoTitle);
                    line.setBorderWidth(0.0);
                    line.setFillColor(&NSColor::separatorColor());
                    line.widthAnchor()
                        .constraintEqualToConstant(2.0)
                        .setActive(true);
                    line.heightAnchor()
                        .constraintEqualToConstant(GAP_L)
                        .setActive(true);
                    line
                };
                let joint = NSStackView::new(self.mtm);
                joint.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
                joint.setAlignment(NSLayoutAttribute::CenterX);
                joint.setSpacing(0.0);
                joint.addArrangedSubview(&line());
                if let Some(id) = id {
                    let tag = self.tag(Tagged::Popup(id.clone()));
                    let menu = NSMenu::new(mtm);
                    menu.setAutoenablesItems(false);
                    for (i, item) in items.iter().enumerate().skip(1) {
                        // A header starts a section of its own
                        if item.header {
                            menu.addItem(&NSMenuItem::separatorItem(mtm));
                        }
                        // SAFETY: the target outlives the menu; cleared on drop
                        let entry = unsafe {
                            NSMenuItem::initWithTitle_action_keyEquivalent(
                                NSMenuItem::alloc(mtm),
                                &NSString::from_str(&item.title),
                                Some(sel!(menuChose:)),
                                &NSString::from_str(""),
                            )
                        };
                        // SAFETY: the target outlives the menu item
                        unsafe { entry.setTarget(Some(&self.target)) };
                        entry.setTag(tag * 100 + i as isize);
                        entry.setEnabled(item.enabled);
                        if let Some(hint) = &item.hint {
                            let hint = NSString::from_str(hint);
                            // Subtitles came with macOS 14.4, before it a tooltip
                            if entry.respondsToSelector(sel!(setSubtitle:)) {
                                entry.setSubtitle(Some(&hint));
                            } else {
                                entry.setToolTip(Some(&hint));
                            }
                        }
                        menu.addItem(&entry);
                    }
                    let joint_index = self.joint_menus.len();
                    let add = self.push_button("", Tagged::Joint(joint_index));
                    add.setBordered(false);
                    add.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
                    if let Some(plus) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                        &NSString::from_str("plus.circle.fill"),
                        Some(&NSString::from_str("Add a transform here")),
                    ) {
                        plus.setSize(NSSize::new(22.0, 22.0));
                        add.setImage(Some(&plus));
                    }
                    add.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                    for anchor in [add.widthAnchor(), add.heightAnchor()] {
                        anchor.constraintEqualToConstant(24.0).setActive(true);
                    }
                    add.setContentTintColor(Some(&NSColor::controlAccentColor()));
                    add.setToolTip(Some(&NSString::from_str("Add a transform here")));
                    self.joint_menus.push((add.clone(), menu));
                    // Where a dragged step would land: a line through the
                    // "+", which sits centered between the joint's lines                    joint.addArrangedSubview(&add);
                    // Where a dragged step would land
                    let mark = NSBox::new(mtm);
                    mark.setBoxType(NSBoxType::Custom);
                    mark.setTitlePosition(NSTitlePosition::NoTitle);
                    mark.setBorderWidth(0.0);
                    mark.setCornerRadius(1.5);
                    mark.setFillColor(&NSColor::clearColor());
                    mark.heightAnchor()
                        .constraintEqualToConstant(3.0)
                        .setActive(true);
                    let holder = NSView::new(mtm);
                    holder
                        .heightAnchor()
                        .constraintEqualToConstant(24.0)
                        .setActive(true);
                    for view in [&*mark as &NSView, &add] {
                        view.setTranslatesAutoresizingMaskIntoConstraints(false);
                        holder.addSubview(view);
                        view.centerYAnchor()
                            .constraintEqualToAnchor(&holder.centerYAnchor())
                            .setActive(true);
                    }
                    mark.leadingAnchor()
                        .constraintEqualToAnchor(&holder.leadingAnchor())
                        .setActive(true);
                    mark.trailingAnchor()
                        .constraintEqualToAnchor(&holder.trailingAnchor())
                        .setActive(true);
                    add.centerXAnchor()
                        .constraintEqualToAnchor(&holder.centerXAnchor())
                        .setActive(true);
                    joint.addArrangedSubview(&holder);
                    holder
                        .widthAnchor()
                        .constraintEqualToAnchor(&joint.widthAnchor())
                        .setActive(true);
                    joint.addArrangedSubview(&line());
                    self.drop_marks.push(mark);
                }
                joint.setSpacing(2.0);
                let (row, _) = self.with_gutter(&joint);
                // The joint's own lines are the gap between two steps
                if let Some(last) = self.editor.arrangedSubviews().lastObject() {
                    self.editor.setCustomSpacing_afterView(0.0, &last);
                }
                self.add(&row);
                self.editor.setCustomSpacing_afterView(0.0, &row);
                Drawn::None
            }
            Control::Header {
                icon,
                title,
                name,
                subtitle,
                switch,
                status,
            } => {
                let mtm = self.mtm;
                let image = symbol_view(mtm, "server.rack");
                image.setContentTintColor(Some(&NSColor::whiteColor()));
                image.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                for anchor in [image.widthAnchor(), image.heightAnchor()] {
                    anchor.constraintEqualToConstant(26.0).setActive(true);
                }
                let (badge, background) = backed(mtm, &image, 9.0, 9.0);
                background.setCornerRadius(10.0);
                background.setBorderWidth(0.0);
                background.setFillColor(&NSColor::controlAccentColor());
                let text = NSStackView::new(mtm);
                text.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
                text.setAlignment(NSLayoutAttribute::Leading);
                text.setSpacing(GAP_M);
                let (title_field, error) = match name {
                    Some((id, error)) => {
                        let field = NSTextField::new(mtm);
                        field.setStringValue(&NSString::from_str(title));
                        // A title edited in place, like a name in the Finder
                        field.setBordered(false);
                        field.setDrawsBackground(false);
                        field.setToolTip(Some(&NSString::from_str("Click to rename")));
                        // One line that scrolls, at least wide enough for a
                        // long name
                        field.setUsesSingleLineMode(true);
                        if let Some(cell) = field.cell() {
                            cell.setWraps(false);
                            cell.setScrollable(true);
                        }
                        field
                            .widthAnchor()
                            .constraintGreaterThanOrEqualToConstant(360.0)
                            .setActive(true);
                        let tag = self.tag(Tagged::Text(id.clone()));
                        field.setTag(tag);
                        // SAFETY: the target outlives the field; both are cleared on drop
                        unsafe {
                            field.setTarget(Some(&self.target));
                            field.setAction(Some(sel!(edited:)));
                            field.setDelegate(Some(ProtocolObject::from_ref(&*self.target)));
                        }
                        self.controls
                            .push(Retained::into_super(Retained::into_super(field.clone())));
                        let warning = Warning::new(mtm, 0.0);
                        warning.show(error.as_deref());
                        (field, Some(warning))
                    }
                    None => (self.label(title), None),
                };
                title_field.setFont(Some(&NSFont::boldSystemFontOfSize(22.0)));
                // The title and, at its right, the switch
                let title_row = NSStackView::new(mtm);
                title_row.setAlignment(NSLayoutAttribute::CenterY);
                title_row.setDistribution(objc2_app_kit::NSStackViewDistribution::Fill);
                title_row.setSpacing(GAP_L);
                title_row.addArrangedSubview(&title_field);
                title_field.setContentHuggingPriority_forOrientation(
                    1.0,
                    NSLayoutConstraintOrientation::Horizontal,
                );
                text.addArrangedSubview(&title_row);
                title_row
                    .widthAnchor()
                    .constraintEqualToAnchor(&text.widthAnchor())
                    .setActive(true);
                if let Some(error) = &error {
                    text.addArrangedSubview(&error.row);
                }
                let subtitle_label = self.label(subtitle);
                subtitle_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
                text.addArrangedSubview(&subtitle_label);
                subtitle_label
                    .widthAnchor()
                    .constraintEqualToAnchor(&text.widthAnchor())
                    .setActive(true);
                // How the saved sync is doing
                let status_line = Warning::new(mtm, 0.0);
                // One line, so the steps below do not move when it changes
                status_line.label.setMaximumNumberOfLines(1);
                status_line
                    .label
                    .setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
                status_line.show_status(status.as_ref());
                text.addArrangedSubview(&status_line.row);
                text.setContentHuggingPriority_forOrientation(
                    1.0,
                    NSLayoutConstraintOrientation::Horizontal,
                );
                let row = NSStackView::new(mtm);
                row.setAlignment(NSLayoutAttribute::CenterY);
                row.setSpacing(12.0);
                // The title (the name field) takes the free width
                row.setDistribution(objc2_app_kit::NSStackViewDistribution::Fill);
                if *icon {
                    row.addArrangedSubview(&badge);
                }
                row.addArrangedSubview(&text);
                let switch_control = switch.as_ref().map(|(id, on)| {
                    let control = NSSwitch::new(mtm);
                    control.setState(if *on {
                        NSControlStateValueOn
                    } else {
                        NSControlStateValueOff
                    });
                    let tag = self.tag(Tagged::Toggle(id.clone()));
                    control.setTag(tag);
                    // SAFETY: the target outlives the switch; cleared on drop
                    unsafe {
                        control.setTarget(Some(&self.target));
                        control.setAction(Some(sel!(pressed:)));
                    }
                    control.setToolTip(Some(&NSString::from_str("Turn the sync on or off")));
                    self.controls
                        .push(Retained::into_super(Retained::into_super(control.clone())));
                    title_row.addArrangedSubview(&control);
                    control
                });
                // A sync's header lines up with its steps, the gutter aside
                let row = if name.is_some() {
                    self.with_gutter(&row).0
                } else {
                    row
                };
                self.add(&row);
                self.container.setCustomSpacing_afterView(GAP_XL, &row);
                Drawn::Header {
                    title: title_field,
                    error,
                    subtitle: subtitle_label,
                    switch: switch_control,
                    status: status_line,
                }
            }
            Control::Segments {
                id,
                label,
                items,
                selected,
                centered,
            } => {
                let labels: Vec<Retained<NSString>> =
                    items.iter().map(|i| NSString::from_str(i)).collect();
                // SAFETY: the target outlives the control; cleared on drop
                let segments = unsafe {
                    NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                        &NSArray::from_retained_slice(&labels),
                        NSSegmentSwitchTracking::SelectOne,
                        Some(&self.target),
                        Some(sel!(segmented:)),
                        self.mtm,
                    )
                };
                let tag = self.tag(Tagged::Row(id.clone(), 0));
                segments.setTag(tag);
                segments.setSelectedSegment(selected.map_or(-1, |s| s as isize));
                if *centered {
                    // A join between conditions: a pill like the dialog buttons
                    segments.setControlSize(objc2_app_kit::NSControlSize::Large);
                }
                self.controls
                    .push(Retained::into_super(Retained::into_super(segments.clone())));
                let row = NSStackView::new(self.mtm);
                let gravity = if *centered {
                    NSStackViewGravity::Center
                } else {
                    NSStackViewGravity::Leading
                };
                row.addView_inGravity(&segments, gravity);
                if label.is_empty() {
                    self.add(&row);
                } else {
                    let labeled = self.labeled(label, &row);
                    self.add(&labeled);
                }
                Drawn::Segments(segments)
            }
            Control::Preview {
                mode,
                source_label,
                source,
                copy_label,
                copy,
            } => {
                // A sample or the source's next event
                let labels = [
                    NSString::from_str("Sample"),
                    NSString::from_str("Next event"),
                ];
                // SAFETY: the target outlives the control; cleared on drop
                let modes = unsafe {
                    NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                        &NSArray::from_retained_slice(&labels),
                        NSSegmentSwitchTracking::SelectOne,
                        Some(&self.target),
                        Some(sel!(segmented:)),
                        self.mtm,
                    )
                };
                // Standing on its own above the preview: a pill
                modes.setControlSize(objc2_app_kit::NSControlSize::Large);
                let tag = self.tag(Tagged::Row(FieldId::PreviewMode, 0));
                modes.setTag(tag);
                modes.setSelectedSegment(mode.map_or(0, |m| m as isize));
                modes.setHidden(mode.is_none());
                self.controls
                    .push(Retained::into_super(Retained::into_super(modes.clone())));
                let (source_view, source_labels) = event_view(self.mtm, source_label, Some(source));
                let (copy_view, copy_labels) = event_view(self.mtm, copy_label, copy.as_ref());
                let heading = NSTextField::labelWithString(
                    &NSString::from_str("One event on its way"),
                    self.mtm,
                );
                heading.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
                let arrow = symbol_view(self.mtm, "arrow.down");
                arrow.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
                for view in [
                    heading.as_ref() as &NSView,
                    modes.as_ref(),
                    &source_view,
                    arrow.as_ref(),
                    &copy_view,
                ] {
                    self.aside.addArrangedSubview(view);
                    view.widthAnchor()
                        .constraintEqualToAnchor(&self.aside.widthAnchor())
                        .setActive(true);
                }
                Drawn::Preview {
                    mode: modes,
                    source: source_labels,
                    copy: copy_labels,
                }
            }
            Control::TransformRow {
                index,
                summary,
                open,
            } => {
                let mtm = self.mtm;
                let tag = self.tag(Tagged::Drag(*index));
                let handle = DragHandle::new(mtm, &self.target, tag);
                // The fields that follow an open row belong to it
                let reveal_after = *open;
                // The handle sits right of the step's box: it moves the box
                if let Some((stage, gutter)) = &self.stage {
                    self.transform_stages.push(stage.clone());
                    gutter.addArrangedSubview(&handle);
                }
                let number = NSTextField::labelWithString(
                    &NSString::from_str(&(index + 1).to_string()),
                    mtm,
                );
                number.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
                number.setTextColor(Some(&NSColor::controlAccentColor()));
                let summary_label = self.label(summary);
                // One glyph, turned down while open, so nothing beside it
                // moves when it changes
                let chevron = symbol_view(mtm, "chevron.right");
                chevron.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
                // Placed by its frame in a fixed box: Auto Layout would undo
                // the turn
                let chevron_box = NSView::new(mtm);
                for anchor in [chevron_box.widthAnchor(), chevron_box.heightAnchor()] {
                    anchor.constraintEqualToConstant(20.0).setActive(true);
                }
                chevron.setImageScaling(NSImageScaling::ScaleNone);
                chevron.setFrame(NSRect::new(NSPoint::new(2.0, 2.0), NSSize::new(16.0, 16.0)));
                chevron_box.addSubview(&chevron);
                chevron_box.setWantsLayer(true);
                chevron_box.setClipsToBounds(false);
                self.chevrons.push(chevron.clone());
                let row = NSStackView::new(mtm);
                row.setSpacing(10.0);
                row.setAlignment(NSLayoutAttribute::CenterY);
                for view in [
                    number.as_ref() as &NSView,
                    summary_label.as_ref(),
                    chevron_box.as_ref(),
                ] {
                    row.addArrangedSubview(view);
                }
                summary_label.setContentHuggingPriority_forOrientation(
                    1.0,
                    NSLayoutConstraintOrientation::Horizontal,
                );
                let toggle = CardButton::new(mtm);
                let tag = self.tag(Tagged::Button(FieldId::TransformRow(*index)));
                toggle.setTag(tag);
                // SAFETY: the target outlives the button; cleared on drop
                unsafe {
                    toggle.setTarget(Some(&self.target));
                    toggle.setAction(Some(sel!(pressed:)));
                }
                self.controls
                    .push(Retained::into_super(Retained::into_super(
                        Retained::into_super(toggle.clone()),
                    )));
                row.setTranslatesAutoresizingMaskIntoConstraints(false);
                toggle.addSubview(&row);
                let constraints = [
                    row.topAnchor()
                        .constraintEqualToAnchor_constant(&toggle.topAnchor(), 4.0),
                    row.bottomAnchor()
                        .constraintEqualToAnchor_constant(&toggle.bottomAnchor(), -4.0),
                    row.leadingAnchor()
                        .constraintEqualToAnchor(&toggle.leadingAnchor()),
                    row.trailingAnchor()
                        .constraintEqualToAnchor(&toggle.trailingAnchor()),
                ];
                NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(
                    &constraints,
                ));
                toggle.setContentHuggingPriority_forOrientation(
                    1.0,
                    NSLayoutConstraintOrientation::Horizontal,
                );
                let line = NSStackView::new(mtm);
                line.setSpacing(4.0);
                line.addArrangedSubview(&toggle);
                let step = index + 1;
                let mut tool = |symbol: &str, tip: &str, id: FieldId| {
                    let b = self.push_button("", Tagged::Button(id));
                    b.setBordered(false);
                    if let Some(image) = symbol_view(mtm, symbol).image() {
                        b.setImage(Some(&image));
                    }
                    b.setToolTip(Some(&NSString::from_str(tip)));
                    accessibility_label(&b, &format!("{tip} step {step}"));
                    line.addArrangedSubview(&b);
                };
                tool("trash", "Remove", FieldId::TransformRowRemove(*index));
                // For VoiceOver and the keyboard: what the row is, and a
                // menu that moves it without dragging
                accessibility_label(
                    &toggle,
                    &format!(
                        "Step {step}: {summary}, {}",
                        if *open { "open" } else { "closed" }
                    ),
                );
                accessibility_label(&handle, &format!("Move step {step}"));
                let menu = NSMenu::new(mtm);
                let items = [
                    ("Move Up", Tagged::MoveTo(*index, index.saturating_sub(1))),
                    ("Move Down", Tagged::MoveTo(*index, index + 2)),
                    (
                        "Remove",
                        Tagged::Button(FieldId::TransformRowRemove(*index)),
                    ),
                ];
                for (title, tagged) in items {
                    let tag = self.tag(tagged);
                    // SAFETY: the target outlives the menu; cleared on drop
                    let item = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            NSMenuItem::alloc(mtm),
                            &NSString::from_str(title),
                            Some(sel!(pressed:)),
                            &NSString::from_str(""),
                        )
                    };
                    // SAFETY: the target outlives the menu item
                    unsafe { item.setTarget(Some(&self.target)) };
                    item.setTag(tag);
                    menu.addItem(&item);
                }
                // SAFETY: the row keeps the menu; its items' target is
                // cleared on drop
                unsafe { toggle.setMenu(Some(&menu)) };
                self.add(&line);
                self.revealing = reveal_after;
                Drawn::TransformRow {
                    summary: summary_label,
                }
            }
            Control::Hero { image, title, text } => {
                if let Some(image) = image {
                    let (view, size) = match image {
                        HeroImage::App => (
                            match NSImage::initWithData(
                                NSImage::alloc(),
                                &NSData::with_bytes(crate::about_window_macos::ICON),
                            ) {
                                Some(image) => NSImageView::imageViewWithImage(&image, self.mtm),
                                None => NSImageView::new(self.mtm),
                            },
                            128.0,
                        ),
                        HeroImage::Done => {
                            let view = symbol_view(self.mtm, "checkmark.circle.fill");
                            view.setContentTintColor(Some(&NSColor::systemGreenColor()));
                            (view, 64.0)
                        }
                    };
                    view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                    view.heightAnchor()
                        .constraintEqualToConstant(size)
                        .setActive(true);
                    self.add(&view);
                }
                let title = self.label(title);
                title.setFont(Some(&NSFont::boldSystemFontOfSize(22.0)));
                title.setAlignment(NSTextAlignment::Center);
                self.add(&title);
                let text = self.label(text);
                text.setTextColor(Some(&NSColor::secondaryLabelColor()));
                text.setAlignment(NSTextAlignment::Center);
                self.add(&text);
                Drawn::Hero(title, text)
            }
            Control::Steps { current, count } => {
                let label = self.label(&steps_text(*current, *count));
                label.setTextColor(Some(&NSColor::controlAccentColor()));
                label.setAlignment(NSTextAlignment::Center);
                self.add(&label);
                Drawn::Label(label)
            }
            Control::Choices {
                id,
                style,
                items,
                selected,
            } => {
                let mut drawn = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    if item.header {
                        let label = self.label(&item.title);
                        label.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
                        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
                        self.add(&label);
                        drawn.push(None);
                        continue;
                    }
                    let card = self.choice(id, i, item, *style);
                    card.set_selected(*selected == Some(i));
                    drawn.push(Some(card));
                }
                Drawn::Choices(drawn)
            }
            Control::Disclosure { id, label, open } => {
                let b =
                    self.push_button(&disclosure_title(label, *open), Tagged::Button(id.clone()));
                b.setBezelStyle(objc2_app_kit::NSBezelStyle::Push);
                // Ends the form: a pill like the footer buttons
                b.setControlSize(objc2_app_kit::NSControlSize::Large);
                set_chevron(&b, *open);
                let hairline = || {
                    let line = NSBox::new(self.mtm);
                    line.setBoxType(NSBoxType::Separator);
                    line
                };
                let (left, right) = (hairline(), hairline());
                let row = NSStackView::new(self.mtm);
                row.setAlignment(NSLayoutAttribute::CenterY);
                row.setSpacing(GAP_M);
                row.setDistribution(objc2_app_kit::NSStackViewDistribution::Fill);
                row.addArrangedSubview(&left);
                row.addArrangedSubview(&b);
                row.addArrangedSubview(&right);
                left.widthAnchor()
                    .constraintEqualToAnchor(&right.widthAnchor())
                    .setActive(true);
                for line in [&left, &right] {
                    line.setContentHuggingPriority_forOrientation(
                        1.0,
                        NSLayoutConstraintOrientation::Horizontal,
                    );
                }
                let (row, _) = self.with_gutter(&row);
                if let Some(last) = self.container.arrangedSubviews().lastObject() {
                    self.container.setCustomSpacing_afterView(GAP_XL, &last);
                }
                self.add(&row);
                Drawn::Disclosure(b)
            }
        }
    }

    /// A push button for `spec`; Delete looks like any other button (the
    /// question before deleting is its warning) but tells accessibility
    /// that it destroys.
    fn spec_button(&mut self, (id, title, enabled): &ButtonSpec) -> Retained<NSButton> {
        let b = self.push_button(title, Tagged::Button(id.clone()));
        b.setEnabled(*enabled);
        b.setHasDestructiveAction(*id == FieldId::Delete);
        if let FieldId::Sponsor(i) = id
            && let Some(image) = crate::sponsor::OPTIONS
                .get(*i)
                .and_then(|o| crate::dialog_window_macos::pdf_image(o.image))
        {
            // NSButton puts no gap between image and title
            b.setTitle(&NSString::from_str(&format!(" {title}")));
            b.setImage(Some(&image));
            b.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageLeading);
        }
        // Moving a condition is mostly a drag: its buttons are small arrows,
        // the title stays their accessibility label and tooltip
        let arrow = match id {
            FieldId::ConditionMoveUp(_) => Some("arrow.up"),
            FieldId::ConditionMoveDown(_) => Some("arrow.down"),
            _ => None,
        };
        if let Some(image) = arrow.and_then(|name| {
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(name),
                Some(&NSString::from_str(title)),
            )
        }) {
            b.setImage(Some(&image));
            b.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
            b.setToolTip(Some(&NSString::from_str(title)));
        }
        b
    }

    /// A button standing on its own or ending a part of the window: a pill
    /// like the buttons of a dialog.
    fn pill_button(&mut self, spec: &ButtonSpec) -> Retained<NSButton> {
        let b = self.spec_button(spec);
        b.setControlSize(objc2_app_kit::NSControlSize::Large);
        b
    }

    /// Draw a choice as a card or a row that reports `Selected(id, index)`;
    /// returns its background, which shows whether it is selected.
    fn choice(
        &mut self,
        id: &FieldId,
        index: usize,
        item: &Choice,
        style: ChoiceStyle,
    ) -> Retained<CardButton> {
        let mtm = self.mtm;
        let card = CardButton::new(mtm);
        let tag = self.tag(Tagged::Row(id.clone(), index));
        card.setTag(tag);
        // SAFETY: the target outlives the button; cleared on drop
        unsafe {
            card.setTarget(Some(&self.target));
            card.setAction(Some(sel!(pressed:)));
        }
        card.setEnabled(item.enabled);
        self.controls
            .push(Retained::into_super(Retained::into_super(
                Retained::into_super(card.clone()),
            )));

        let background = NSBox::new(mtm);
        background.setBoxType(NSBoxType::Custom);
        background.setTitlePosition(NSTitlePosition::NoTitle);
        background.setCornerRadius(if style == ChoiceStyle::Cards {
            10.0
        } else {
            6.0
        });

        let text = NSStackView::new(mtm);
        text.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        text.setAlignment(NSLayoutAttribute::Leading);
        text.setSpacing(2.0);
        let mut lines: Vec<Retained<NSView>> = Vec::new();
        let title = self.label(&item.title);
        if style == ChoiceStyle::Cards {
            title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        }
        if !item.enabled {
            title.setTextColor(Some(&NSColor::secondaryLabelColor()));
        }
        lines.push(Retained::into_super(Retained::into_super(title)));
        if !item.detail.is_empty() {
            let detail = self.label(&item.detail);
            detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
            lines.push(Retained::into_super(Retained::into_super(detail)));
        }
        if let Some(preview) = &item.preview {
            lines.push(event_view(mtm, "", Some(preview)).0);
        }
        for line in &lines {
            text.addArrangedSubview(line);
            line.widthAnchor()
                .constraintEqualToAnchor(&text.widthAnchor())
                .setActive(true);
        }
        text.setContentHuggingPriority_forOrientation(
            1.0,
            NSLayoutConstraintOrientation::Horizontal,
        );

        let row = NSStackView::new(mtm);
        // A card's icon sits at its top, a row's hint in its middle
        row.setAlignment(match style {
            ChoiceStyle::Cards => NSLayoutAttribute::Top,
            ChoiceStyle::Rows => NSLayoutAttribute::CenterY,
        });
        row.setSpacing(12.0);
        if let Some(icon) = item.icon {
            let symbol = match icon {
                ChoiceIcon::CalendarApp => "calendar",
                ChoiceIcon::CalDav => "network",
            };
            let image = symbol_view(mtm, symbol);
            image.setContentTintColor(Some(&NSColor::controlAccentColor()));
            for anchor in [image.widthAnchor(), image.heightAnchor()] {
                anchor.constraintEqualToConstant(28.0).setActive(true);
            }
            row.addArrangedSubview(&image);
        }
        row.addArrangedSubview(&text);
        // The text takes the card's width, or its lines wrap at the
        // preview's width
        if item.hint.is_none() {
            text.trailingAnchor()
                .constraintEqualToAnchor(&row.trailingAnchor())
                .setActive(true);
        }
        if let Some(hint) = &item.hint {
            let hint = NSTextField::labelWithString(&NSString::from_str(hint), mtm);
            hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
            hint.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            // A pill, like the Advanced settings one
            let (pill, background) = backed(mtm, &hint, GAP_M, 2.0);
            background.setCornerRadius(8.0);
            background.setBorderWidth(0.0);
            background.setFillColor(&NSColor::quaternarySystemFillColor());
            row.addArrangedSubview(&pill);
        }

        let (dx, dy) = match style {
            ChoiceStyle::Cards => (14.0, 10.0),
            ChoiceStyle::Rows => (10.0, 5.0),
        };
        for view in [background.as_ref() as &NSView, row.as_ref()] {
            view.setTranslatesAutoresizingMaskIntoConstraints(false);
            card.addSubview(view);
        }
        let constraints = [
            background
                .topAnchor()
                .constraintEqualToAnchor(&card.topAnchor()),
            background
                .bottomAnchor()
                .constraintEqualToAnchor(&card.bottomAnchor()),
            background
                .leadingAnchor()
                .constraintEqualToAnchor(&card.leadingAnchor()),
            background
                .trailingAnchor()
                .constraintEqualToAnchor(&card.trailingAnchor()),
            row.topAnchor()
                .constraintEqualToAnchor_constant(&card.topAnchor(), dy),
            row.bottomAnchor()
                .constraintEqualToAnchor_constant(&card.bottomAnchor(), -dy),
            row.leadingAnchor()
                .constraintEqualToAnchor_constant(&card.leadingAnchor(), dx),
            row.trailingAnchor()
                .constraintEqualToAnchor_constant(&card.trailingAnchor(), -dx),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
        self.add(&card);
        card.set_background(&background, style);
        card
    }

    fn fill_popup(&self, popup: &NSPopUpButton, items: &[PopupItem], selected: Option<usize>) {
        let menu = NSMenu::new(self.mtm);
        menu.setAutoenablesItems(false);
        for item in items {
            // SAFETY: a plain menu item without action; the popup sends chose:
            let entry = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(self.mtm),
                    &NSString::from_str(&item.title),
                    None,
                    &NSString::from_str(""),
                )
            };
            entry.setEnabled(item.enabled);
            if !item.header {
                entry.setIndentationLevel(1);
            }
            if let Some(hint) = &item.hint {
                entry.setToolTip(Some(&NSString::from_str(hint)));
            }
            menu.addItem(&entry);
        }
        popup.setMenu(Some(&menu));
        if let Some(selected) = selected {
            popup.selectItemAtIndex(selected as isize);
        }
    }

    fn update(&mut self, view: &SettingsView) {
        let moved = self
            .last
            .as_ref()
            .is_some_and(|last| last.moves != view.moves);
        if let (Some(label), Some(text)) = (&self.banner, &view.banner) {
            label.setStringValue(&NSString::from_str(text));
        }
        let items = view.syncs.iter().map(|i| (i, true));
        let items = items.chain(view.accounts.iter().map(|i| (i, false)));
        if let Some(general) = &self.general_button {
            general.set_selected(view.general.selected);
            general.set_tint(general_tint(view.general.problem));
        }
        for (row, (item, sync)) in self.sidebar_buttons.iter().zip(items) {
            row.ivars().selected.set(item.selected);
            row.set_tint(icon_tint(sync, item.active, item.problem));
            row.set_draft(item.draft);
        }
        let last = self.last.clone();
        let old_controls = last
            .as_ref()
            .map(|l| l.controls.as_slice())
            .unwrap_or_default();
        for (i, control) in view.controls.iter().enumerate() {
            let Some(drawn) = self.drawn.get(i) else {
                continue;
            };
            match (control, drawn) {
                (Control::Error(t), Drawn::Warning(w)) => {
                    w.show(Some(t.as_str()).filter(|t| !t.is_empty()))
                }
                (Control::Note(t), Drawn::Label(l)) => {
                    l.setStringValue(&NSString::from_str(t));
                    l.setHidden(t.is_empty());
                }
                (
                    Control::Text {
                        value,
                        placeholder,
                        error,
                        enabled,
                        ..
                    },
                    Drawn::Text { field, error: e },
                ) => {
                    field.setEnabled(*enabled);
                    // The field being typed in keeps its text and cursor,
                    // unless a move put another condition there
                    if field.stringValue().to_string() != *value {
                        match field.currentEditor() {
                            None => field.setStringValue(&NSString::from_str(value)),
                            Some(editor) if moved => {
                                field.setStringValue(&NSString::from_str(value));
                                editor.setString(&NSString::from_str(value));
                            }
                            Some(_) => {}
                        }
                    }
                    field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
                    e.show(error.as_deref());
                }
                (Control::Check { on, label, .. }, Drawn::Check(c)) => {
                    c.setTitle(&NSString::from_str(label));
                    c.setState(if *on {
                        NSControlStateValueOn
                    } else {
                        NSControlStateValueOff
                    });
                }
                (
                    Control::Popup {
                        items, selected, ..
                    },
                    Drawn::Popup(p),
                ) => {
                    let changed = match old_controls.get(i) {
                        Some(Control::Popup { items: old, .. }) => old != items,
                        _ => true,
                    };
                    if changed {
                        self.fill_popup(p, items, *selected);
                    } else {
                        p.selectItemAtIndex(selected.map_or(-1, |s| s as isize));
                    }
                }
                (Control::Buttons(buttons), Drawn::Buttons(drawn)) => {
                    for (b, (id, title, enabled)) in drawn.iter().zip(buttons) {
                        // A sponsor button keeps its title next to its logo
                        if !matches!(id, FieldId::Sponsor(_)) {
                            b.setTitle(&NSString::from_str(title));
                        }
                        b.setEnabled(*enabled);
                    }
                }
                (
                    Control::Footer {
                        left,
                        text,
                        warning,
                        progress: fraction,
                        right,
                    },
                    Drawn::Footer {
                        label,
                        sign,
                        progress,
                        buttons,
                    },
                ) => {
                    show_footer_text(label, sign, text, *warning);
                    show_progress(progress, *fraction);
                    for (b, (_, title, enabled)) in buttons.iter().zip(left.iter().chain(right)) {
                        b.setTitle(&NSString::from_str(title));
                        b.setEnabled(*enabled);
                    }
                }
                (
                    Control::Header {
                        title: text,
                        name,
                        subtitle: sub,
                        switch: switch_spec,
                        status: line,
                        ..
                    },
                    Drawn::Header {
                        title,
                        error,
                        subtitle,
                        switch,
                        status,
                    },
                ) => {
                    status.show_status(line.as_ref());
                    // The name being typed keeps its text and cursor
                    if title.currentEditor().is_none() && title.stringValue().to_string() != *text {
                        title.setStringValue(&NSString::from_str(text));
                    }
                    if let (Some(error), Some((_, message))) = (error, name) {
                        error.show(message.as_deref());
                    }
                    subtitle.setStringValue(&NSString::from_str(sub));
                    if let (Some(switch), Some((_, on))) = (switch, switch_spec) {
                        switch.setState(if *on {
                            NSControlStateValueOn
                        } else {
                            NSControlStateValueOff
                        });
                    }
                }
                (Control::Segments { selected, .. }, Drawn::Segments(segments)) => {
                    segments.setSelectedSegment(selected.map_or(-1, |s| s as isize));
                }
                (
                    Control::Preview {
                        mode: m,
                        source_label,
                        source: s,
                        copy_label,
                        copy: c,
                    },
                    Drawn::Preview { mode, source, copy },
                ) => {
                    mode.setSelectedSegment(m.map_or(0, |m| m as isize));
                    mode.setHidden(m.is_none());
                    fill_event(source, source_label, Some(s));
                    fill_event(copy, copy_label, c.as_ref());
                }
                (Control::TransformRow { summary: text, .. }, Drawn::TransformRow { summary }) => {
                    summary.setStringValue(&NSString::from_str(text));
                }
                (Control::Hero { title, text, .. }, Drawn::Hero(t, l)) => {
                    t.setStringValue(&NSString::from_str(title));
                    l.setStringValue(&NSString::from_str(text));
                }
                (Control::Steps { current, count }, Drawn::Label(l)) => {
                    l.setStringValue(&NSString::from_str(&steps_text(*current, *count)))
                }
                (Control::Choices { selected, .. }, Drawn::Choices(drawn)) => {
                    for (j, card) in drawn.iter().enumerate() {
                        if let Some(card) = card {
                            card.set_selected(*selected == Some(j));
                        }
                    }
                }
                (Control::Disclosure { label, open, .. }, Drawn::Disclosure(b)) => {
                    b.setTitle(&NSString::from_str(&disclosure_title(label, *open)));
                    set_chevron(b, *open);
                }
                _ => {}
            }
        }
    }
}

/// A red title for a button that removes something; a plain bordered
/// button ignores its tint color. Disabled, it is grey like the others.
/// Show whether a choice is selected or under the mouse: an accent border
/// on a card, an accent background on a row.
fn paint_choice(background: &NSBox, style: ChoiceStyle, selected: bool, hovered: bool) {
    let accent = NSColor::controlAccentColor();
    match style {
        ChoiceStyle::Cards => {
            background.setBorderWidth(if selected { 2.0 } else { 1.0 });
            background.setBorderColor(&*match (selected, hovered) {
                (true, _) => accent.clone(),
                (false, true) => accent.colorWithAlphaComponent(0.5),
                (false, false) => NSColor::separatorColor(),
            });
            background.setFillColor(&*match (selected, hovered) {
                (true, _) => accent.colorWithAlphaComponent(0.08),
                (false, true) => accent.colorWithAlphaComponent(0.04),
                (false, false) => NSColor::controlBackgroundColor(),
            });
        }
        ChoiceStyle::Rows => {
            background.setBorderWidth(0.0);
            background.setFillColor(&*match (selected, hovered) {
                (true, _) => accent.colorWithAlphaComponent(0.2),
                (false, true) => accent.colorWithAlphaComponent(0.08),
                (false, false) => NSColor::clearColor(),
            });
        }
    }
}

/// The caption, time, title and details of a drawn event.
type EventLabels = [Retained<NSTextField>; 4];

/// A caption above an event drawn like one in a calendar: a tinted box with
/// time, title and details.
fn event_view(
    mtm: MainThreadMarker,
    caption: &str,
    event: Option<&EventPreview>,
) -> (Retained<NSView>, EventLabels) {
    let label = |font: Retained<NSFont>| {
        let l = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        l.setFont(Some(&font));
        l
    };
    let labels: EventLabels = [
        label(NSFont::systemFontOfSize(11.0)),
        label(NSFont::monospacedDigitSystemFontOfSize_weight(11.0, 0.0)),
        label(NSFont::boldSystemFontOfSize(12.0)),
        label(NSFont::systemFontOfSize(11.0)),
    ];
    labels[0].setTextColor(Some(&NSColor::secondaryLabelColor()));
    labels[1].setTextColor(Some(&NSColor::secondaryLabelColor()));
    let lines = NSStackView::new(mtm);
    lines.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    lines.setAlignment(NSLayoutAttribute::Leading);
    lines.setSpacing(1.0);
    for l in &labels[1..] {
        lines.addArrangedSubview(l);
    }
    let (event_box, background) = backed(mtm, &lines, 8.0, 5.0);
    background.setCornerRadius(6.0);
    background.setBorderWidth(0.0);
    background.setFillColor(&NSColor::controlAccentColor().colorWithAlphaComponent(0.15));
    let outer = NSStackView::new(mtm);
    outer.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    outer.setAlignment(NSLayoutAttribute::Leading);
    outer.setSpacing(3.0);
    outer.addArrangedSubview(&labels[0]);
    outer.addArrangedSubview(&event_box);
    event_box
        .widthAnchor()
        .constraintEqualToAnchor(&outer.widthAnchor())
        .setActive(true);
    fill_event(&labels, caption, event);
    (Retained::into_super(outer), labels)
}

fn fill_event(labels: &EventLabels, caption: &str, event: Option<&EventPreview>) {
    let (time, title, detail) = match event {
        Some(e) => (e.time.as_str(), e.title.as_str(), e.detail.as_str()),
        None => ("", "No copy", ""),
    };
    for (label, text) in labels.iter().zip([caption, time, title, detail]) {
        label.setStringValue(&NSString::from_str(text));
    }
    labels[0].setHidden(caption.is_empty());
    labels[1].setHidden(time.is_empty());
    labels[3].setHidden(detail.is_empty());
}

/// `inner` on a custom NSBox background, inset by `dx` and `dy`.
/// The gap between dragged `views` (steps or conditions) at `y` (window
/// coordinates): the number of views whose middle is above it.
fn drop_gap(views: &[Retained<NSView>], y: f64) -> usize {
    views
        .iter()
        .filter(|view| {
            let frame = view.convertRect_toView(view.bounds(), None);
            frame.origin.y + frame.size.height / 2.0 > y
        })
        .count()
}

/// While view `from` is `dragging` it fades, and the mark `gap` shows
/// where it would land.
fn show_drag(
    views: &[Retained<NSView>],
    marks: &[Retained<NSBox>],
    from: usize,
    dragging: bool,
    gap: Option<usize>,
) {
    for (i, view) in views.iter().enumerate() {
        view.setAlphaValue(if dragging && i == from { 0.5 } else { 1.0 });
    }
    for (i, mark) in marks.iter().enumerate() {
        // The mark always takes its room, so showing it moves nothing
        mark.setFillColor(&*if gap == Some(i) {
            NSColor::controlAccentColor()
        } else {
            NSColor::clearColor()
        });
    }
}

fn backed(
    mtm: MainThreadMarker,
    inner: &NSView,
    dx: f64,
    dy: f64,
) -> (Retained<NSView>, Retained<NSBox>) {
    let background = NSBox::new(mtm);
    background.setBoxType(NSBoxType::Custom);
    background.setTitlePosition(NSTitlePosition::NoTitle);
    let holder = NSView::new(mtm);
    for view in [background.as_ref() as &NSView, inner] {
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        holder.addSubview(view);
    }
    let constraints = [
        background
            .topAnchor()
            .constraintEqualToAnchor(&holder.topAnchor()),
        background
            .bottomAnchor()
            .constraintEqualToAnchor(&holder.bottomAnchor()),
        background
            .leadingAnchor()
            .constraintEqualToAnchor(&holder.leadingAnchor()),
        background
            .trailingAnchor()
            .constraintEqualToAnchor(&holder.trailingAnchor()),
        inner
            .topAnchor()
            .constraintEqualToAnchor_constant(&holder.topAnchor(), dy),
        inner
            .bottomAnchor()
            .constraintEqualToAnchor_constant(&holder.bottomAnchor(), -dy),
        inner
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&holder.leadingAnchor(), dx),
        inner
            .trailingAnchor()
            .constraintEqualToAnchor_constant(&holder.trailingAnchor(), -dx),
    ];
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    (holder, background)
}

fn sidebar_symbol(sync: bool) -> &'static str {
    if sync {
        "arrow.triangle.2.circlepath"
    } else {
        "server.rack"
    }
}

/// Sidebar icons: accent for a running sync, orange for one with a
/// problem, grey for a disabled one.
/// General turns orange while the config has a problem.
fn general_tint(problem: bool) -> Retained<NSColor> {
    if problem {
        NSColor::systemOrangeColor()
    } else {
        NSColor::controlAccentColor()
    }
}

fn icon_tint(sync: bool, active: bool, problem: bool) -> Retained<NSColor> {
    match (sync, active) {
        (true, true) if problem => NSColor::systemOrangeColor(),
        (true, true) => NSColor::controlAccentColor(),
        (true, false) => NSColor::systemGrayColor(),
        (false, _) => NSColor::controlAccentColor(),
    }
}

/// What is wrong with a field: a warning sign and the message in the
/// text color, hidden while there is none.
pub struct Warning {
    row: Retained<NSStackView>,
    sign: Retained<NSImageView>,
    label: Retained<NSTextField>,
}

impl Warning {
    /// `indent` moves it right, under a field after its label.
    fn new(mtm: MainThreadMarker, indent: f64) -> Self {
        let sign = symbol_view(mtm, "exclamationmark.triangle.fill");
        sign.setContentTintColor(Some(&NSColor::systemOrangeColor()));
        let label = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        let row = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[sign.as_ref() as &NSView, label.as_ref()]),
            mtm,
        );
        row.setSpacing(GAP_S + 2.0);
        row.setAlignment(NSLayoutAttribute::FirstBaseline);
        row.setEdgeInsets(objc2_foundation::NSEdgeInsets {
            top: 0.0,
            left: indent,
            bottom: 0.0,
            right: 0.0,
        });
        Warning { row, sign, label }
    }

    /// A status line: quiet, with the sign and in the text color only
    /// for a problem.
    fn show_status(&self, status: Option<&(String, bool)>) {
        self.show(status.map(|(text, _)| text.as_str()));
        self.label
            .setToolTip(status.map(|(text, _)| NSString::from_str(text)).as_deref());
        let problem = status.is_some_and(|(_, problem)| *problem);
        self.sign.setHidden(!problem);
        let color = if problem {
            NSColor::labelColor()
        } else {
            NSColor::secondaryLabelColor()
        };
        self.label.setTextColor(Some(&color));
    }

    fn show(&self, message: Option<&str>) {
        self.label
            .setStringValue(&NSString::from_str(message.unwrap_or("")));
        self.row.setHidden(message.is_none());
    }
}

/// The footer's text: a warning in the text color after an orange sign,
/// anything else quieter; the tooltip holds a text cut off.
fn show_footer_text(label: &NSTextField, sign: &NSImageView, text: &str, warning: bool) {
    label.setStringValue(&NSString::from_str(text));
    label.setToolTip(Some(&NSString::from_str(text)));
    let color = if warning {
        NSColor::labelColor()
    } else {
        NSColor::secondaryLabelColor()
    };
    label.setTextColor(Some(&color));

    sign.setHidden(!warning);
}

/// A bar for (done, total); hidden without one.
fn show_progress(bar: &NSProgressIndicator, progress: Option<(usize, usize)>) {
    if let Some((done, total)) = progress {
        bar.setDoubleValue(done as f64 / total.max(1) as f64);
    }
    bar.setHidden(progress.is_none());
}

/// What VoiceOver says for a control without a title.
fn accessibility_label(view: &NSView, label: &str) {
    // SAFETY: every NSView takes an accessibility label
    let _: () = unsafe { msg_send![view, setAccessibilityLabel: &*NSString::from_str(label)] };
}

fn reduce_motion() -> bool {
    // SAFETY: class methods of AppKit classes that every macOS has
    unsafe {
        let workspace: Retained<AnyObject> = msg_send![objc2::class!(NSWorkspace), sharedWorkspace];
        msg_send![&workspace, accessibilityDisplayShouldReduceMotion]
    }
}

/// An image view with an SF Symbol, empty if the system lacks it.
fn symbol_view(mtm: MainThreadMarker, name: &str) -> Retained<NSImageView> {
    match NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    ) {
        Some(image) => NSImageView::imageViewWithImage(&image, mtm),
        None => NSImageView::new(mtm),
    }
}

fn disclosure_title(label: &str, open: bool) -> String {
    if open {
        format!("Hide {} settings", label.to_lowercase())
    } else {
        format!("{label} settings")
    }
}

/// The chevron after a disclosure's title: down to open, up to close.
fn set_chevron(button: &NSButton, open: bool) {
    let symbol = if open { "chevron.up" } else { "chevron.down" };
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        None,
    ) {
        button.setImage(Some(&image));
        button.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageTrailing);
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        // Controls and the window keep unretained pointers to the target
        if self.last.as_ref().is_some_and(|l| !l.compact) {
            self.window
                .saveFrameUsingName(&NSString::from_str(FRAME_NAME));
        }
        self.stop_turning();
        self.clear_targets();
        self.window.setDelegate(None);
        if let Some(parent) = self.window.sheetParent() {
            parent.endSheet(&self.window);
        }
        self.window.close();
    }
}
