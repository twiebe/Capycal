//! A window with a heading, buttons and a read-only, selectable text, like
//! the macOS panel window. Used by "Problems".

use adw::prelude::*;
use gtk::glib;

use crate::dialog::layout;

pub struct TextWindow {
    window: adw::Window,
    heading: gtk::Label,
    text: gtk::TextView,
    scroll: gtk::ScrolledWindow,
    /// Shown instead of heading and text when there is nothing to show.
    empty: adw::StatusPage,
    content: String,
}

impl TextWindow {
    /// A window titled `title`. The first of `buttons` goes into the header
    /// bar, the others into a bar below the text (GNOME HIG: few header
    /// buttons). `pressed` gets the index of a clicked button, `closed` runs
    /// when the window closes.
    pub fn new(
        application: &adw::Application,
        title: &str,
        buttons: &[&str],
        pressed: impl Fn(usize) + Clone + 'static,
        closed: impl Fn() + 'static,
    ) -> Self {
        let header = adw::HeaderBar::new();
        let actions = gtk::ActionBar::new();
        for (index, label) in buttons.iter().enumerate() {
            let button = gtk::Button::with_label(label);
            let pressed = pressed.clone();
            button.connect_clicked(move |_| pressed(index));
            if index == 0 {
                header.pack_end(&button);
            } else {
                actions.pack_end(&button);
            }
        }
        let heading = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .margin_top(12)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .css_classes(["title-4"])
            .build();
        let text = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(6)
            .bottom_margin(12)
            .left_margin(12)
            .right_margin(12)
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&text)
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name(crate::icons::CHECK)
            .title("No problems")
            .description("Capycal syncs as configured.")
            .vexpand(true)
            .visible(false)
            .build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&heading);
        body.append(&scroll);
        body.append(&empty);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&body));
        if buttons.len() > 1 {
            view.add_bottom_bar(&actions);
        }
        let window = adw::Window::builder()
            .application(application)
            .title(title)
            .default_width(layout::TEXT_WINDOW.0 as i32)
            .default_height(layout::TEXT_WINDOW.1 as i32)
            .content(&view)
            .build();
        window.connect_close_request(move |_| {
            closed();
            glib::Propagation::Proceed
        });
        window.present();
        Self {
            window,
            heading,
            text,
            scroll,
            empty,
            content: String::new(),
        }
    }

    pub fn present(&self) {
        self.window.present();
    }

    pub fn set_heading(&self, heading: &str) {
        self.heading.set_text(heading);
    }

    /// Show `text`, unless it is shown already (keeps the scroll position).
    pub fn set_text(&mut self, text: &str) {
        if self.content != text {
            self.text.buffer().set_text(text);
            self.content = text.to_string();
        }
    }

    /// Show the empty state ("No problems") instead of heading and text.
    pub fn set_empty(&self, empty: bool) {
        self.empty.set_visible(empty);
        self.scroll.set_visible(!empty);
        self.heading.set_visible(!empty);
    }
}

impl Drop for TextWindow {
    fn drop(&mut self) {
        self.window.destroy();
    }
}
