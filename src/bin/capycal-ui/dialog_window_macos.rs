//! A dialog window in AppKit, centered like the GTK app's dialog: a large
//! icon, a bold heading with an optional spinner, a wrapping message, an
//! optional progress bar, optional link buttons, an optional popup button,
//! a row of equally wide buttons and an optional footnote. Its contents change in place (see `dialog::View`).

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSButton, NSCellImagePosition, NSColor, NSControlSize,
    NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSImage, NSImageScaling,
    NSImageView, NSLayoutAttribute, NSLayoutConstraint, NSMutableParagraphStyle, NSPanel,
    NSParagraphStyleAttributeName, NSPopUpButton, NSProgressIndicator, NSProgressIndicatorStyle,
    NSStackView, NSStackViewDistribution, NSTextAlignment, NSTextField, NSTitlePosition,
    NSUserInterfaceLayoutOrientation, NSView, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSData, NSDictionary, NSMutableAttributedString, NSPoint, NSRect,
    NSSize, NSString,
};

use crate::dialog::{Choice, FIRST_LINK, View, layout};
use crate::panel_window_macos::{PanelInput, PanelTarget};
use crate::text_window_macos::button;

const MARGIN: f64 = layout::MARGIN;
const MESSAGE_WIDTH: f64 = layout::DIALOG_WIDTH - 2.0 * MARGIN;
const ICON_SIZE: f64 = 64.0;

pub struct DialogWindow {
    window: Retained<NSPanel>,
    content: Retained<NSView>,
    heading: Retained<NSTextField>,
    spinner: Retained<NSProgressIndicator>,
    message: Retained<NSTextField>,
    warning_box: Retained<NSBox>,
    warning: Retained<NSTextField>,
    entry: Retained<NSTextField>,
    progress: Retained<NSProgressIndicator>,
    choice: Retained<NSPopUpButton>,
    shown_choice: Option<Vec<String>>,
    button_row: Retained<NSStackView>,
    footnote: Retained<NSTextField>,
    buttons: Vec<Retained<NSButton>>,
    links: Vec<Retained<NSButton>>,
    shown_buttons: &'static [&'static str],
    shown_destructive: bool,
    shown_entry: bool,
    target: Retained<PanelTarget>,
}

impl DialogWindow {
    /// Open the dialog showing `view` with the SF Symbol `symbol` as icon,
    /// in `tint` or else the text color. `notify` receives the input to
    /// handle on the event loop.
    /// Like an alert, the window has no title; the heading says it all.
    pub fn open(
        mtm: MainThreadMarker,
        symbol: &str,
        tint: Option<&NSColor>,
        view: &View,
        notify: impl Fn(PanelInput) + 'static,
    ) -> Self {
        let target = PanelTarget::new(mtm, notify);
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
        // A panel closes on Esc, which the owner gets as `PanelInput::Closed`
        let window = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(MESSAGE_WIDTH, 160.0)),
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        // SAFETY: the panel is owned by the Retained, not released on close
        unsafe { window.setReleasedWhenClosed(false) };
        // Stay visible while another app is active
        window.setHidesOnDeactivate(false);
        window.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        let content = NSView::new(mtm);
        window.setContentView(Some(&content));

        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            None,
        );
        let icon = match &image {
            Some(image) => NSImageView::imageViewWithImage(image, mtm),
            None => NSImageView::new(mtm),
        };
        icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        icon.setContentTintColor(tint);

        let spinner = NSProgressIndicator::new(mtm);
        spinner.setStyle(NSProgressIndicatorStyle::Spinning);
        spinner.setControlSize(NSControlSize::Small);
        spinner.setIndeterminate(true);
        let heading = NSTextField::wrappingLabelWithString(&NSString::new(), mtm);
        heading.setFont(Some(&NSFont::boldSystemFontOfSize(17.0)));
        heading.setAlignment(NSTextAlignment::Center);
        heading.setPreferredMaxLayoutWidth(MESSAGE_WIDTH);
        let heading_row = stack(mtm, &[&spinner, &heading], NSLayoutAttribute::CenterY, 8.0);

        let message = NSTextField::wrappingLabelWithString(&NSString::new(), mtm);
        message.setAlignment(NSTextAlignment::Center);
        message.setPreferredMaxLayoutWidth(MESSAGE_WIDTH);
        // A bold warning after the sign, in a light orange box
        let sign = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("exclamationmark.triangle.fill"),
            Some(&NSString::from_str("Warning")),
        )
        .map(|image| NSImageView::imageViewWithImage(&image, mtm))
        .unwrap_or_else(|| NSImageView::new(mtm));
        sign.setContentTintColor(Some(&NSColor::systemOrangeColor()));
        let warning = NSTextField::wrappingLabelWithString(&NSString::new(), mtm);
        warning.setFont(Some(
            &NSFont::boldSystemFontOfSize(NSFont::systemFontSize()),
        ));
        warning.setPreferredMaxLayoutWidth(MESSAGE_WIDTH - 48.0);
        let warning_row = stack(
            mtm,
            &[&sign, &warning],
            NSLayoutAttribute::FirstBaseline,
            8.0,
        );
        warning_row.setTranslatesAutoresizingMaskIntoConstraints(false);
        let warning_box = NSBox::new(mtm);
        warning_box.setBoxType(NSBoxType::Custom);
        warning_box.setTitlePosition(NSTitlePosition::NoTitle);
        warning_box.setBorderWidth(0.0);
        warning_box.setCornerRadius(8.0);
        warning_box.setFillColor(&NSColor::systemOrangeColor().colorWithAlphaComponent(0.15));
        warning_box.setContentViewMargins(NSSize::new(12.0, 10.0));
        if let Some(inside) = warning_box.contentView() {
            inside.addSubview(&warning_row);
            NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
                warning_row
                    .topAnchor()
                    .constraintEqualToAnchor(&inside.topAnchor()),
                warning_row
                    .bottomAnchor()
                    .constraintEqualToAnchor(&inside.bottomAnchor()),
                warning_row
                    .leadingAnchor()
                    .constraintEqualToAnchor(&inside.leadingAnchor()),
                warning_row
                    .trailingAnchor()
                    .constraintLessThanOrEqualToAnchor(&inside.trailingAnchor()),
            ]));
        }
        let entry = NSTextField::new(mtm);
        // SAFETY: the delegate is cleared in Drop, before it is released
        unsafe { entry.setDelegate(Some(ProtocolObject::from_ref(&*target))) };
        let progress = NSProgressIndicator::new(mtm);
        progress.setStyle(NSProgressIndicatorStyle::Bar);
        progress.setIndeterminate(false);
        progress.setMinValue(0.0);
        progress.setMaxValue(1.0);
        let links: Vec<Retained<NSButton>> = view
            .links
            .iter()
            .enumerate()
            .map(|(i, link)| {
                let button = button(mtm, link.title, &target, sel!(pressed:));
                button.setTag((FIRST_LINK + i) as isize);
                button.setControlSize(NSControlSize::Large);
                if let Some(image) = pdf_image(link.image) {
                    // NSButton puts no gap between image and title
                    button.setTitle(&NSString::from_str(&format!(" {}", link.title)));
                    button.setImage(Some(&image));
                    button.setImagePosition(NSCellImagePosition::ImageLeading);
                }
                button
            })
            .collect();
        let link_views: Vec<&NSView> = links.iter().map(|b| -> &NSView { b }).collect();
        let link_row = stack(mtm, &link_views, NSLayoutAttribute::CenterY, 8.0);
        link_row.setHidden(links.is_empty());
        let choice =
            NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        // SAFETY: the target is cleared in Drop, before it is released
        unsafe {
            choice.setTarget(Some(&target));
            choice.setAction(Some(sel!(chose:)));
        }
        let button_row = NSStackView::new(mtm);
        button_row.setDistribution(NSStackViewDistribution::FillEqually);
        button_row.setSpacing(12.0);
        let footnote = NSTextField::wrappingLabelWithString(&NSString::new(), mtm);
        footnote.setAlignment(NSTextAlignment::Center);
        footnote.setPreferredMaxLayoutWidth(MESSAGE_WIDTH);
        footnote.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
        footnote.setTextColor(Some(&NSColor::secondaryLabelColor()));

        let outer = stack(
            mtm,
            &[
                &icon,
                &heading_row,
                &warning_box,
                &message,
                &entry,
                &progress,
                &link_row,
                &choice,
                &button_row,
                &footnote,
            ],
            NSLayoutAttribute::CenterX,
            12.0,
        );
        outer.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        outer.setCustomSpacing_afterView(6.0, &icon);
        outer.setCustomSpacing_afterView(20.0, &message);
        outer.setCustomSpacing_afterView(20.0, &link_row);
        outer.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&outer);

        // Fixed margins instead of the stack's edge insets, which the
        // fitting size of the content view leaves out at the bottom and right
        let constraints = [
            outer
                .topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), MARGIN),
            outer
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&content.bottomAnchor(), -MARGIN),
            outer
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), MARGIN),
            outer
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -MARGIN),
            icon.widthAnchor().constraintEqualToConstant(ICON_SIZE),
            icon.heightAnchor().constraintEqualToConstant(ICON_SIZE),
            message
                .widthAnchor()
                .constraintEqualToConstant(MESSAGE_WIDTH),
            progress
                .widthAnchor()
                .constraintEqualToAnchor(&message.widthAnchor()),
            warning_box
                .widthAnchor()
                .constraintEqualToAnchor(&message.widthAnchor()),
            entry
                .widthAnchor()
                .constraintEqualToAnchor(&message.widthAnchor()),
            button_row
                .widthAnchor()
                .constraintEqualToAnchor(&message.widthAnchor()),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));

        let mut dialog = Self {
            window,
            content,
            heading,
            spinner,
            message,
            warning_box,
            warning,
            entry,
            progress,
            choice,
            shown_choice: None,
            button_row,
            footnote,
            buttons: Vec::new(),
            links,
            shown_buttons: &[],
            shown_destructive: false,
            shown_entry: false,
            target,
        };
        dialog.show(view);
        dialog.window.center();
        dialog.focus();
        dialog
    }

    pub fn focus(&self) {
        crate::text_window_macos::focus(&self.window);
    }

    /// The text typed into the field of `View::entry`.
    pub fn entry_text(&self) -> String {
        self.entry.stringValue().to_string()
    }

    /// Show `view`, resizing the window to fit it.
    pub fn show(&mut self, view: &View) {
        self.heading
            .setStringValue(&NSString::from_str(&view.heading));
        if view.lines.is_empty() {
            self.message
                .setStringValue(&NSString::from_str(&view.message));
        } else {
            self.message
                .setAttributedStringValue(&rich_text(&view.runs()));
        }
        self.warning_box.setHidden(view.warning.is_none());
        self.warning.setStringValue(&NSString::from_str(
            view.warning.as_deref().unwrap_or_default(),
        ));
        self.entry.setHidden(view.entry.is_none());
        self.entry
            .setPlaceholderString(view.entry.as_deref().map(NSString::from_str).as_deref());
        if self.shown_entry != view.entry.is_some() {
            self.entry.setStringValue(&NSString::new());
        }
        self.spinner.setHidden(!view.busy);
        // SAFETY: the sender argument may be nil
        unsafe {
            if view.busy {
                self.spinner.startAnimation(None);
            } else {
                self.spinner.stopAnimation(None);
            }
        }
        self.progress.setHidden(view.progress.is_none());
        self.footnote.setHidden(view.footnote.is_none());
        self.footnote.setStringValue(&NSString::from_str(
            view.footnote.as_deref().unwrap_or_default(),
        ));
        self.progress.setDoubleValue(view.fraction().unwrap_or(0.0));
        self.set_choice(view.choice.as_ref());
        if self.shown_buttons != view.buttons || self.shown_destructive != view.destructive {
            self.set_buttons(view.buttons, view.destructive);
        }
        if let Some(last) = self.buttons.last() {
            last.setEnabled(!view.last_disabled);
        }
        // A field that appears takes the focus, so typing goes there
        if view.entry.is_some() && !self.shown_entry {
            self.window.makeFirstResponder(Some(&self.entry));
        }
        self.shown_entry = view.entry.is_some();
        self.content.layoutSubtreeIfNeeded();
        self.window.setContentSize(self.content.fittingSize());
    }

    fn set_choice(&mut self, choice: Option<&Choice>) {
        self.choice.setHidden(choice.is_none());
        let Some(choice) = choice else {
            return;
        };
        if self.shown_choice.as_ref() != Some(&choice.items) {
            self.choice.removeAllItems();
            for item in &choice.items {
                self.choice.addItemWithTitle(&NSString::from_str(item));
            }
            self.shown_choice = Some(choice.items.clone());
        }
        self.choice.selectItemAtIndex(choice.selected as isize);
    }

    fn set_buttons(&mut self, titles: &'static [&'static str], destructive: bool) {
        for old in self.buttons.drain(..) {
            // SAFETY: clearing the pointer is always valid
            unsafe { old.setTarget(None) };
            self.button_row.removeArrangedSubview(&old);
            old.removeFromSuperview();
        }
        let mtm = self.window.mtm();
        for (i, title) in titles.iter().enumerate() {
            let button = button(mtm, title, &self.target, sel!(pressed:));
            button.setTag(i as isize);
            button.setControlSize(NSControlSize::Large);
            let last = i + 1 == titles.len();
            // Return picks the last button, or the first (Cancel) when the
            // last one deletes something
            if (last && !destructive) || (i == 0 && destructive) {
                button.setKeyEquivalent(&NSString::from_str("\r"));
            }
            if last && destructive {
                button.setHasDestructiveAction(true);
                button.setContentTintColor(Some(&NSColor::systemRedColor()));
            }
            self.button_row.addArrangedSubview(&button);
            self.buttons.push(button);
        }
        self.shown_buttons = titles;
        self.shown_destructive = destructive;
        // The removed buttons took the focus and the Tab order with them
        self.window.recalculateKeyViewLoop();
        if let Some(first) = self.buttons.first() {
            self.window.makeFirstResponder(Some(first));
        }
    }
}

impl Drop for DialogWindow {
    fn drop(&mut self) {
        // Buttons and window keep unretained pointers to the target
        for button in self.buttons.iter().chain(&self.links) {
            // SAFETY: clearing the pointer is always valid
            unsafe { button.setTarget(None) };
        }
        // SAFETY: clearing the pointer is always valid
        unsafe { self.choice.setTarget(None) };
        // SAFETY: clearing the pointer is always valid
        unsafe { self.entry.setDelegate(None) };
        self.window.setDelegate(None);
        self.window.close();
    }
}

/// Runs of text, each bold or not, centered in the label color.
fn rich_text(runs: &[(String, bool)]) -> Retained<NSAttributedString> {
    let size = NSFont::systemFontSize();
    let regular = NSFont::systemFontOfSize(size);
    let bold = NSFont::boldSystemFontOfSize(size);
    let color = NSColor::labelColor();
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setAlignment(NSTextAlignment::Center);
    let text = NSMutableAttributedString::new();
    for (run, is_bold) in runs {
        let font = if *is_bold { &bold } else { &regular };
        // SAFETY: immutable AppKit constants
        let keys = unsafe {
            [
                NSFontAttributeName,
                NSForegroundColorAttributeName,
                NSParagraphStyleAttributeName,
            ]
        };
        let values: [&AnyObject; 3] = [font, &color, &paragraph];
        let attributes = NSDictionary::from_slices(&keys, &values);
        // SAFETY: the attributes map the keys to an NSFont, an NSColor and an
        // NSParagraphStyle
        let run = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &NSString::from_str(run),
                Some(&attributes),
            )
        };
        text.appendAttributedString(&run);
    }
    Retained::into_super(text)
}

/// A template image from a monochrome PDF, so it takes the text color.
pub fn pdf_image(pdf: &[u8]) -> Option<Retained<NSImage>> {
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(pdf))?;
    image.setTemplate(true);
    image.setSize(NSSize::new(16.0, 16.0));
    Some(image)
}

fn stack(
    mtm: MainThreadMarker,
    views: &[&NSView],
    alignment: NSLayoutAttribute,
    spacing: f64,
) -> Retained<NSStackView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_slice(views), mtm);
    stack.setAlignment(alignment);
    stack.setSpacing(spacing);
    stack
}
