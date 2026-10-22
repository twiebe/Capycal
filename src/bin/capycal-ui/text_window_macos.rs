//! Shared AppKit glue for the app's windows: a row of controls above a
//! read-only, selectable text view, brought to the front as key window.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSFont, NSFontWeightRegular, NSLayoutAttribute,
    NSLayoutConstraint, NSLayoutConstraintOrientation, NSScrollView, NSStackView,
    NSStackViewDistribution, NSTextView, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};

/// Space around the toolbar row.
const MARGIN: f64 = 10.0;

pub struct TextWindow {
    pub window: Retained<NSWindow>,
    pub scroll: Retained<NSScrollView>,
    pub text: Retained<NSTextView>,
}

impl TextWindow {
    /// A centered window with `row` pinned to the top and the text view
    /// filling the rest. The window is not shown yet; call `focus`.
    /// `delegate` is held unretained until the window is dropped.
    pub fn new(
        mtm: MainThreadMarker,
        title: &str,
        size: NSSize,
        min_size: NSSize,
        row: &[&NSView],
        delegate: &ProtocolObject<dyn NSWindowDelegate>,
    ) -> Result<Self, String> {
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        // SAFETY: a plain window; it is owned by the Retained, not released on close
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, size),
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str(title));
        window.setContentMinSize(min_size);
        window.setDelegate(Some(delegate));
        let content = NSView::new(mtm);
        window.setContentView(Some(&content));

        let row = NSStackView::stackViewWithViews(&NSArray::from_slice(row), mtm);
        row.setDistribution(NSStackViewDistribution::Fill);
        // Text baselines line up across labels, buttons and fields
        row.setAlignment(NSLayoutAttribute::FirstBaseline);
        row.setHuggingPriority_forOrientation(750.0, NSLayoutConstraintOrientation::Vertical);
        row.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&row);

        let scroll = NSTextView::scrollableTextView(mtm);
        scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&scroll);

        let constraints = [
            row.topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), MARGIN),
            row.leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), MARGIN),
            row.trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -MARGIN),
            scroll
                .topAnchor()
                .constraintEqualToAnchor_constant(&row.bottomAnchor(), MARGIN),
            scroll
                .leadingAnchor()
                .constraintEqualToAnchor(&content.leadingAnchor()),
            scroll
                .trailingAnchor()
                .constraintEqualToAnchor(&content.trailingAnchor()),
            scroll
                .bottomAnchor()
                .constraintEqualToAnchor(&content.bottomAnchor()),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));

        let text = scroll
            .documentView()
            .and_then(|view| view.downcast::<NSTextView>().ok())
            .ok_or("the scroll view has no text view")?;
        text.setEditable(false);
        text.setSelectable(true);
        text.setTextContainerInset(NSSize::new(4.0, 6.0));
        window.center();
        Ok(Self {
            window,
            scroll,
            text,
        })
    }

    /// Bring the window to the front as key window (see `focus`).
    pub fn focus(&self) {
        focus(&self.window);
    }
}

impl Drop for TextWindow {
    fn drop(&mut self) {
        // The window keeps an unretained pointer to its delegate
        self.window.setDelegate(None);
        self.window.close();
    }
}

/// Bring `window` to the front as key window. Opening a window does not
/// activate an accessory app, and the window of an inactive app never
/// becomes key, so its text fields would take no input. `activate()` may
/// decline to activate (cooperative activation since macOS 14);
/// `activateIgnoringOtherApps` does not.
pub fn focus(window: &NSWindow) {
    #[allow(deprecated)]
    NSApplication::sharedApplication(window.mtm()).activateIgnoringOtherApps(true);
    window.makeKeyAndOrderFront(None);
}

/// A push button sending `action` to `target`. The owner must clear the
/// target before `target` is released.
pub fn button(
    mtm: MainThreadMarker,
    title: &str,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSButton> {
    // SAFETY: the caller keeps the target alive while it is set
    unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(action),
            mtm,
        )
    }
}

pub fn monospaced_font() -> Retained<NSFont> {
    // SAFETY: an immutable AppKit constant
    let weight = unsafe { NSFontWeightRegular };
    NSFont::monospacedSystemFontOfSize_weight(12.0, weight)
}
