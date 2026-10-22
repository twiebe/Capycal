//! The About dialog, laid out like the macOS app's: app icon, name,
//! developer, version, a random tagline, link buttons, the license and
//! Acknowledgements on one page. `adw::AboutDialog` would hide the tagline
//! and links on a Details page.

use adw::prelude::*;

/// `acknowledgements` opens the Acknowledgements window.
pub fn show(acknowledgements: impl Fn() + 'static) {
    let label = |text: &str, classes: &[&str]| {
        gtk::Label::builder()
            .label(text)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(36)
            .css_classes(classes.to_vec())
            .build()
    };
    let icon = gtk::Image::builder()
        .icon_name(crate::icons::APP)
        .pixel_size(160)
        .margin_bottom(6)
        .build();
    let name = label("Capycal", &["title-1"]);
    let developer = label(crate::dialog::ABOUT_DEVELOPER, &[]);
    let version = label(&format!("Version {}", capycal::VERSION), &["dim-label"]);
    let quip = label(&crate::dialog::about_description(), &[]);
    let click = gtk::GestureClick::new();
    let weak = quip.downgrade();
    click.connect_released(move |_, _, _, _| {
        if let Some(quip) = weak.upgrade() {
            quip.set_label(&crate::dialog::about_description());
        }
    });
    quip.add_controller(click);
    quip.set_margin_top(12);
    quip.set_margin_bottom(12);

    let links = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    for (title, url, option) in crate::dialog::about_links() {
        let button = gtk::Button::builder().css_classes(["pill"]).build();
        // ButtonContent always shows an image, a missing one without a logo
        match option {
            Some(option) => button.set_child(Some(
                &adw::ButtonContent::builder()
                    .label(title)
                    .icon_name(crate::icons::symbolic(option.title, option.svg))
                    .build(),
            )),
            None => button.set_label(title),
        }
        button.connect_clicked(move |_| crate::app::open_uri(&url));
        links.append(&button);
    }
    let license = label(crate::dialog::ABOUT_LICENSE, &["dim-label", "caption"]);
    license.set_margin_top(12);
    let more = gtk::Button::builder()
        .label(format!("{}…", crate::dialog::ACKNOWLEDGEMENTS.0))
        .halign(gtk::Align::Center)
        .css_classes(["flat", "caption"])
        .build();
    more.connect_clicked(move |_| acknowledgements());

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    for widget in [
        icon.upcast_ref::<gtk::Widget>(),
        name.upcast_ref(),
        developer.upcast_ref(),
        version.upcast_ref(),
        quip.upcast_ref(),
        links.upcast_ref(),
        license.upcast_ref(),
        more.upcast_ref(),
    ] {
        column.append(widget);
    }
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
    view.set_content(Some(&column));
    adw::Dialog::builder()
        .title("About Capycal")
        .content_width(360)
        .child(&view)
        .build()
        .present(None::<&gtk::Widget>);
}
