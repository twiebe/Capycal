//! The About window, laid out like libadwaita's about dialog: app icon,
//! name, developer, version and description, link buttons and the license.
//! The icon is built in, so it also shows without an app bundle.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSCellImagePosition, NSClickGestureRecognizer, NSColor,
    NSControlSize, NSFont, NSImage, NSImageScaling, NSImageView, NSLayoutAttribute,
    NSLayoutConstraint, NSPanel, NSStackView, NSTextAlignment, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSData, NSPoint, NSRect, NSSize, NSString};

use crate::dialog_window_macos::pdf_image;
use crate::panel_window_macos::{PanelInput, PanelTarget};
use crate::text_window_macos::button;

pub const ICON: &[u8] =
    include_bytes!("../../../packaging/macos/icon/AppIcon.iconset/icon_128x128@2x.png");
const ICON_SIZE: f64 = 160.0;
const WIDTH: f64 = 300.0;
const MARGIN: f64 = crate::dialog::layout::MARGIN;

pub struct AboutWindow {
    window: Retained<NSPanel>,
    content: Retained<NSView>,
    description: Retained<NSTextField>,
    click: Retained<NSClickGestureRecognizer>,
    buttons: Vec<Retained<NSButton>>,
    acknowledgements: Retained<NSButton>,
    _target: Retained<PanelTarget>,
}

impl AboutWindow {
    /// `notify` gets `PanelInput::Button(i)` for the link at index `i` of
    /// `dialog::about_links()`, `Button(dialog::ABOUT_DESCRIPTION)` for a
    /// click on the description, `Button(dialog::ABOUT_ACKNOWLEDGEMENTS)`
    /// for Acknowledgements, and `Closed`.
    pub fn open(mtm: MainThreadMarker, notify: impl Fn(PanelInput) + 'static) -> Self {
        let target = PanelTarget::new(mtm, notify);
        // A panel closes on Esc
        let window = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, 400.0)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        );
        // SAFETY: the panel is owned by the Retained, not released on close
        unsafe { window.setReleasedWhenClosed(false) };
        window.setHidesOnDeactivate(false);
        window.setTitle(&NSString::from_str("About Capycal"));
        window.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        let content = NSView::new(mtm);
        window.setContentView(Some(&content));

        let icon = match NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(ICON)) {
            Some(image) => NSImageView::imageViewWithImage(&image, mtm),
            None => NSImageView::new(mtm),
        };
        icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        let label = |text: &str, font: Retained<NSFont>, secondary: bool| {
            let label = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
            label.setFont(Some(&font));
            label.setAlignment(NSTextAlignment::Center);
            label.setPreferredMaxLayoutWidth(WIDTH);
            if secondary {
                label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            }
            label
        };
        let name = label("Capycal", NSFont::boldSystemFontOfSize(24.0), false);
        let developer = label(
            crate::dialog::ABOUT_DEVELOPER,
            NSFont::systemFontOfSize(13.0),
            true,
        );
        let version = label(
            &format!("Version {}", capycal::VERSION),
            NSFont::systemFontOfSize(12.0),
            true,
        );
        let description = label(
            &crate::dialog::about_description(),
            NSFont::systemFontOfSize(13.0),
            false,
        );
        description.setTag(crate::dialog::ABOUT_DESCRIPTION as isize);
        // SAFETY: the target outlives the recognizer's use of it, Drop clears it
        let click = unsafe {
            NSClickGestureRecognizer::initWithTarget_action(
                NSClickGestureRecognizer::alloc(mtm),
                Some(&target),
                Some(sel!(clicked:)),
            )
        };
        description.addGestureRecognizer(&click);
        let license = label(
            crate::dialog::ABOUT_LICENSE,
            NSFont::systemFontOfSize(11.0),
            true,
        );
        let acknowledgements = button(
            mtm,
            &format!("{}…", crate::dialog::ACKNOWLEDGEMENTS.0),
            &target,
            sel!(pressed:),
        );
        acknowledgements.setTag(crate::dialog::ABOUT_ACKNOWLEDGEMENTS as isize);
        acknowledgements.setControlSize(NSControlSize::Small);
        acknowledgements.setFont(Some(&NSFont::systemFontOfSize(11.0)));

        let buttons: Vec<Retained<NSButton>> = crate::dialog::about_links()
            .into_iter()
            .enumerate()
            .map(|(i, (title, _, option))| {
                let button = button(mtm, title, &target, sel!(pressed:));
                button.setTag(i as isize);
                button.setControlSize(NSControlSize::Large);
                if let Some(image) = option.and_then(|o| pdf_image(o.image)) {
                    // NSButton puts no gap between title and image
                    button.setTitle(&NSString::from_str(&format!("{title} ")));
                    button.setImage(Some(&image));
                    // Right after the title instead of at the edge of the
                    // wide button
                    button.setImagePosition(NSCellImagePosition::ImageTrailing);
                    button.setImageHugsTitle(true);
                }
                button
            })
            .collect();
        let link_views: Vec<&NSView> = buttons.iter().map(|b| -> &NSView { b }).collect();
        let link_column = NSStackView::stackViewWithViews(&NSArray::from_slice(&link_views), mtm);
        link_column.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        link_column.setSpacing(8.0);

        let views: [&NSView; 8] = [
            &icon,
            &name,
            &developer,
            &version,
            &description,
            &link_column,
            &license,
            &acknowledgements,
        ];
        let column = NSStackView::stackViewWithViews(&NSArray::from_slice(&views), mtm);
        column.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        column.setAlignment(NSLayoutAttribute::CenterX);
        column.setSpacing(6.0);
        column.setCustomSpacing_afterView(12.0, &icon);
        column.setCustomSpacing_afterView(16.0, &version);
        column.setCustomSpacing_afterView(20.0, &description);
        column.setCustomSpacing_afterView(20.0, &link_column);
        column.setCustomSpacing_afterView(8.0, &license);
        column.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&column);

        let mut constraints = vec![
            column
                .topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), MARGIN),
            column
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&content.bottomAnchor(), -MARGIN),
            column
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), MARGIN),
            column
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -MARGIN),
            icon.widthAnchor().constraintEqualToConstant(ICON_SIZE),
            icon.heightAnchor().constraintEqualToConstant(ICON_SIZE),
            description.widthAnchor().constraintEqualToConstant(WIDTH),
            link_column.widthAnchor().constraintEqualToConstant(WIDTH),
        ];
        // The link buttons fill the column
        constraints.extend(buttons.iter().map(|button| {
            button
                .widthAnchor()
                .constraintEqualToAnchor(&link_column.widthAnchor())
        }));
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));

        content.layoutSubtreeIfNeeded();
        window.setContentSize(content.fittingSize());
        window.center();
        let about = Self {
            window,
            content,
            description,
            click,
            buttons,
            acknowledgements,
            _target: target,
        };
        about.focus();
        // Without this AppKit focuses the first link, which then shows a
        // focus ring; Tab still reaches every button
        about.window.makeFirstResponder(None);
        about
    }

    pub fn focus(&self) {
        crate::text_window_macos::focus(&self.window);
    }

    /// Show the next quip; the window grows or shrinks with its lines.
    pub fn next_quip(&self) {
        self.description
            .setStringValue(&NSString::from_str(&crate::dialog::about_description()));
        self.content.layoutSubtreeIfNeeded();
        self.window.setContentSize(self.content.fittingSize());
    }
}

impl Drop for AboutWindow {
    fn drop(&mut self) {
        // Buttons, recognizer and window keep unretained pointers to the target
        // SAFETY: clearing the pointer is always valid
        unsafe { self.click.setTarget(None) };
        for button in self.buttons.iter().chain([&self.acknowledgements]) {
            // SAFETY: clearing the pointer is always valid
            unsafe { button.setTarget(None) };
        }
        self.window.setDelegate(None);
        self.window.close();
    }
}
