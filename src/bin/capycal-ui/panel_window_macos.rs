//! A text panel in AppKit: a header line and a row of buttons above a
//! read-only text. Used by "Problems". Its button
//! target is shared with the dialog window.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSColor, NSControlTextEditingDelegate, NSFont, NSImage, NSImageScaling, NSImageView,
    NSLayoutAttribute, NSLayoutConstraint, NSLayoutConstraintOrientation, NSStackView, NSTextField,
    NSTextFieldDelegate, NSUserInterfaceLayoutOrientation, NSView, NSWindowDelegate,
};
use objc2_foundation::{NSArray, NSNotification, NSSize, NSString};

use crate::dialog::layout;
use crate::text_window_macos::{TextWindow, button, monospaced_font};

/// Size of a panel showing a longer text.
pub const LARGE: NSSize = NSSize::new(layout::TEXT_WINDOW.0, layout::TEXT_WINDOW.1);
const MIN_SIZE: NSSize = NSSize::new(480.0, 240.0);

/// What happened in the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelInput {
    /// The button at this index of `PanelWindow::open`'s `buttons`.
    Button(usize),
    Closed,
}

pub struct Ivars {
    notify: Box<dyn Fn(PanelInput)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and PanelTarget
    // does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CapycalPanelTarget"]
    #[ivars = Ivars]
    /// Target of the buttons and window delegate. It only forwards what
    /// happened; AppKit does not retain it.
    pub struct PanelTarget;

    impl PanelTarget {
        #[unsafe(method(pressed:))]
        fn pressed(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else {
                return;
            };
            // SAFETY: the sender is one of the panel's NSButtons
            let tag: isize = unsafe { msg_send![sender, tag] };
            self.send(PanelInput::Button(tag as usize));
        }

        /// A click gesture on a control: reported as button with the
        /// control's tag.
        #[unsafe(method(clicked:))]
        fn clicked(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else {
                return;
            };
            // SAFETY: the sender is an NSGestureRecognizer on an NSControl
            let tag: isize = unsafe {
                let view: Option<&AnyObject> = msg_send![sender, view];
                match view {
                    Some(view) => msg_send![view, tag],
                    None => return,
                }
            };
            self.send(PanelInput::Button(tag as usize));
        }

        /// A popup button's item was chosen: reported as button
        /// `FIRST_CHOICE` plus its index (see `dialog::View::choice`).
        #[unsafe(method(chose:))]
        fn chose(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else {
                return;
            };
            // SAFETY: the sender is an NSPopUpButton
            let index: isize = unsafe { msg_send![sender, indexOfSelectedItem] };
            if let Ok(index) = usize::try_from(index) {
                self.send(PanelInput::Button(crate::dialog::FIRST_CHOICE + index));
            }
        }
    }

    unsafe impl NSObjectProtocol for PanelTarget {}

    unsafe impl NSControlTextEditingDelegate for PanelTarget {
        /// The text of a dialog's field changed: reported as button
        /// `TYPED` (see `dialog::View::entry`).
        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, _notification: &NSNotification) {
            self.send(PanelInput::Button(crate::dialog::TYPED));
        }
    }

    unsafe impl NSTextFieldDelegate for PanelTarget {}

    unsafe impl NSWindowDelegate for PanelTarget {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            self.send(PanelInput::Closed);
        }
    }
);

impl PanelTarget {
    pub fn new(mtm: MainThreadMarker, notify: impl Fn(PanelInput) + 'static) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            notify: Box::new(notify),
        });
        // SAFETY: NSObject's designated initializer
        unsafe { msg_send![super(this), init] }
    }

    fn send(&self, input: PanelInput) {
        (self.ivars().notify)(input);
    }
}

pub struct PanelWindow {
    window: TextWindow,
    /// Shown instead of the text when there is nothing to show.
    empty: Retained<NSStackView>,
    header: Retained<NSTextField>,
    buttons: Vec<Retained<NSButton>>,
    _target: Retained<PanelTarget>,
    font: Retained<NSFont>,
    content: String,
}

impl PanelWindow {
    /// Open the panel showing `text`. `notify` receives the input to handle
    /// on the event loop.
    pub fn open(
        mtm: MainThreadMarker,
        title: &str,
        size: NSSize,
        header: &str,
        buttons: &[&str],
        text: &str,
        notify: impl Fn(PanelInput) + 'static,
    ) -> Result<Self, String> {
        let target = PanelTarget::new(mtm, notify);
        let label = NSTextField::labelWithString(&NSString::from_str(header), mtm);
        // The header takes the free width, the buttons keep their size
        let horizontal = NSLayoutConstraintOrientation::Horizontal;
        label.setContentHuggingPriority_forOrientation(1.0, horizontal);
        label.setContentCompressionResistancePriority_forOrientation(249.0, horizontal);
        let buttons: Vec<Retained<NSButton>> = buttons
            .iter()
            .enumerate()
            .map(|(i, title)| {
                let button = button(mtm, title, &target, sel!(pressed:));
                button.setTag(i as isize);
                button.setContentHuggingPriority_forOrientation(750.0, horizontal);
                button.setContentCompressionResistancePriority_forOrientation(750.0, horizontal);
                button
            })
            .collect();
        let mut row: Vec<&NSView> = vec![&label];
        row.extend(buttons.iter().map(|b| -> &NSView { b }));
        let window = TextWindow::new(
            mtm,
            title,
            size,
            MIN_SIZE,
            &row,
            ProtocolObject::from_ref(&*target),
        )?;
        let empty = empty_state(
            mtm,
            "checkmark.circle",
            "No problems",
            "Capycal syncs as configured.",
        );
        if let Some(content) = window.window.contentView() {
            empty.setTranslatesAutoresizingMaskIntoConstraints(false);
            empty.setHidden(true);
            content.addSubview(&empty);
            let constraints = [
                empty
                    .centerXAnchor()
                    .constraintEqualToAnchor(&window.scroll.centerXAnchor()),
                empty
                    .centerYAnchor()
                    .constraintEqualToAnchor(&window.scroll.centerYAnchor()),
            ];
            NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
        }
        let mut panel = Self {
            window,
            empty,
            header: label,
            buttons,
            _target: target,
            font: monospaced_font(),
            content: String::new(),
        };
        panel.set_text(text);
        panel.focus();
        // The text view takes Cmd-A and Cmd-C from the Edit menu
        let window = &panel.window;
        window.window.makeFirstResponder(Some(&window.text));
        Ok(panel)
    }

    pub fn focus(&self) {
        self.window.focus();
    }

    pub fn set_header(&self, header: &str) {
        self.header.setStringValue(&NSString::from_str(header));
    }

    /// Show `text`, unless it is shown already (keeps the scroll position).
    pub fn set_text(&mut self, text: &str) {
        if self.content == text {
            return;
        }
        self.content = text.to_string();
        let view = &self.window.text;
        view.setString(&NSString::from_str(text));
        view.setFont(Some(&self.font));
    }

    /// Show the empty state instead of the text.
    pub fn set_empty(&self, empty: bool) {
        self.empty.setHidden(!empty);
        self.window.scroll.setHidden(empty);
    }
}

impl Drop for PanelWindow {
    fn drop(&mut self) {
        // The buttons keep an unretained pointer to the target. The
        // TextWindow clears the delegate when dropped after this.
        for button in &self.buttons {
            // SAFETY: clearing the pointer is always valid
            unsafe { button.setTarget(None) };
        }
    }
}

/// A large symbol with a title and a line below, centered.
fn empty_state(
    mtm: MainThreadMarker,
    symbol: &str,
    title: &str,
    detail: &str,
) -> Retained<NSStackView> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        None,
    );
    let icon = match &image {
        Some(image) => NSImageView::imageViewWithImage(image, mtm),
        None => NSImageView::new(mtm),
    };
    icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    icon.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    icon.widthAnchor()
        .constraintEqualToConstant(56.0)
        .setActive(true);
    icon.heightAnchor()
        .constraintEqualToConstant(56.0)
        .setActive(true);
    let title = NSTextField::labelWithString(&NSString::from_str(title), mtm);
    title.setFont(Some(&NSFont::boldSystemFontOfSize(17.0)));
    let detail = NSTextField::labelWithString(&NSString::from_str(detail), mtm);
    detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let views: [&NSView; 3] = [&icon, &title, &detail];
    let stack = NSStackView::stackViewWithViews(&NSArray::from_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::CenterX);
    stack.setSpacing(8.0);
    stack
}
