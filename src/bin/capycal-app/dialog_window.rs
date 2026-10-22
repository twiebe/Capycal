//! A dialog that changes in place (see `dialog::View`), like the macOS
//! dialog window: icon, heading, spinner, message, progress bar, link
//! buttons, a dropdown, buttons and a footnote.
//! `adw::AlertDialog` closes on every response, so this is an `adw::Dialog`.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::dialog::{Choice, FIRST_CHOICE, FIRST_LINK, View};

/// What happened in the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogInput {
    /// The button at this index of `View::buttons`, `FIRST_LINK` plus the
    /// index of a link or `FIRST_CHOICE` plus the index of a chosen item.
    Button(usize),
    Closed,
}

pub struct DialogWindow {
    alive: Rc<Cell<bool>>,
    dialog: adw::Dialog,
    heading: gtk::Label,
    heading_row: gtk::Box,
    spinner: gtk::Spinner,
    message: gtk::Label,
    warning_box: gtk::Box,
    warning: gtk::Label,
    entry: gtk::Entry,
    progress: gtk::ProgressBar,
    choice: gtk::DropDown,
    shown_choice: Option<Vec<String>>,
    buttons: gtk::Box,
    footnote: gtk::Label,
    shown_buttons: &'static [&'static str],
    shown_destructive: bool,
    shown_entry: bool,
    notify: Rc<dyn Fn(DialogInput)>,
}

impl DialogWindow {
    /// Present a dialog showing `view` with the themed icon `icon`, in the
    /// CSS color `tint` if given. `notify` receives the input.
    pub fn open(
        title: &str,
        icon: &str,
        tint: Option<&str>,
        view: &View,
        notify: impl Fn(DialogInput) + 'static,
    ) -> Self {
        // Cleared when the window is dropped, so closing it from here
        // reports nothing
        let alive = Rc::new(Cell::new(true));
        let open = alive.clone();
        let notify: Rc<dyn Fn(DialogInput)> = Rc::new(move |input| {
            if open.get() {
                notify(input)
            }
        });
        let image = gtk::Image::builder()
            .icon_name(icon)
            .pixel_size(64)
            .margin_bottom(6)
            .build();
        if let Some(tint) = tint {
            tint_widget(&image, tint);
        }
        let spinner = gtk::Spinner::new();
        let heading = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["title-2"])
            .build();
        let heading_row = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        heading_row.append(&spinner);
        heading_row.append(&heading);
        let message = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(52)
            .build();
        // A bold warning after the sign, in a light orange box
        let warning_box = gtk::Box::builder()
            .spacing(8)
            .css_classes(["capycal-warning-box"])
            .visible(false)
            .build();
        let sign = gtk::Image::builder()
            .icon_name("dialog-warning-symbolic")
            .valign(gtk::Align::Start)
            .build();
        tint_widget(&sign, "#e66100");
        let warning = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        warning_box.append(&sign);
        warning_box.append(&warning);
        add_css(
            ".capycal-warning-box { background: alpha(#e66100, 0.15); border-radius: 8px; padding: 10px 12px; }",
        );
        let entry = gtk::Entry::builder().visible(false).build();
        let typed = notify.clone();
        entry.connect_changed(move |_| typed(DialogInput::Button(crate::dialog::TYPED)));
        let progress = gtk::ProgressBar::builder().visible(false).build();
        let links = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::Center)
            .visible(!view.links.is_empty())
            .build();
        for (index, link) in view.links.iter().enumerate() {
            let content = adw::ButtonContent::builder()
                .label(link.title)
                .icon_name(crate::icons::symbolic(link.title, link.svg))
                .build();
            let button = gtk::Button::builder()
                .child(&content)
                .css_classes(["pill"])
                .build();
            let notify = notify.clone();
            button.connect_clicked(move |_| notify(DialogInput::Button(FIRST_LINK + index)));
            links.append(&button);
        }
        let choice = gtk::DropDown::builder()
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        let chosen = notify.clone();
        choice.connect_selected_notify(move |choice| {
            // Rebuilding the items selects nothing for a moment
            if let Ok(index) = usize::try_from(choice.selected()) {
                chosen(DialogInput::Button(FIRST_CHOICE + index));
            }
        });
        let buttons = gtk::Box::builder()
            .spacing(12)
            .homogeneous(true)
            .margin_top(6)
            .build();
        let footnote = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(52)
            .css_classes(["caption", "dim-label"])
            .build();

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        for widget in [
            image.upcast_ref::<gtk::Widget>(),
            heading_row.upcast_ref(),
            warning_box.upcast_ref(),
            message.upcast_ref(),
            entry.upcast_ref(),
            progress.upcast_ref(),
            links.upcast_ref(),
            choice.upcast_ref(),
            buttons.upcast_ref(),
            footnote.upcast_ref(),
        ] {
            body.append(widget);
        }
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(crate::dialog::layout::DIALOG_WIDTH as i32)
            .child(&body)
            .build();
        let closed = notify.clone();
        dialog.connect_closed(move |_| closed(DialogInput::Closed));

        let mut window = Self {
            alive,
            dialog,
            heading,
            heading_row,
            spinner,
            message,
            warning_box,
            warning,
            entry,
            progress,
            choice,
            shown_choice: None,
            buttons,
            footnote,
            shown_buttons: &[],
            shown_destructive: false,
            shown_entry: false,
            notify,
        };
        window.show(view);
        window.dialog.present(None::<&gtk::Widget>);
        window
    }

    /// Show `view`; the links stay as they were opened.
    pub fn show(&mut self, view: &View) {
        self.heading.set_text(&view.heading);
        if view.lines.is_empty() {
            self.message.set_text(&view.message);
        } else {
            let markup: String = view
                .runs()
                .iter()
                .map(|(run, bold)| {
                    let run = glib::markup_escape_text(run);
                    if *bold {
                        format!("<b>{run}</b>")
                    } else {
                        run.to_string()
                    }
                })
                .collect();
            self.message.set_markup(&markup);
        }
        self.warning_box.set_visible(view.warning.is_some());
        self.warning
            .set_text(view.warning.as_deref().unwrap_or_default());
        self.entry.set_visible(view.entry.is_some());
        self.entry.set_placeholder_text(view.entry.as_deref());
        self.entry
            .update_property(&[gtk::accessible::Property::Label(
                view.entry.as_deref().unwrap_or_default(),
            )]);
        if view.busy && !self.spinner.is_spinning() {
            // A spinner hidden and shown again may stay still (seen on
            // macOS), so busy again gets a fresh one
            let spinner = gtk::Spinner::new();
            self.heading_row.remove(&self.spinner);
            self.heading_row.prepend(&spinner);
            self.spinner = spinner;
        }
        self.spinner.set_visible(view.busy);
        self.spinner.set_spinning(view.busy);
        self.progress.set_visible(view.progress.is_some());
        self.progress.set_fraction(view.fraction().unwrap_or(0.0));
        self.footnote.set_visible(view.footnote.is_some());
        self.footnote
            .set_text(view.footnote.as_deref().unwrap_or_default());
        self.set_choice(view.choice.as_ref());
        if self.shown_buttons != view.buttons || self.shown_destructive != view.destructive {
            self.set_buttons(view.buttons, view.destructive);
        }
        if let Some(last) = self.buttons.last_child() {
            last.set_sensitive(!view.last_disabled);
        }
        // A field that appears takes the focus, so typing goes there
        if view.entry.is_some() && !self.shown_entry {
            self.entry.set_text("");
            self.entry.grab_focus();
        }
        self.shown_entry = view.entry.is_some();
    }

    /// The text typed into the field of `View::entry`.
    pub fn entry_text(&self) -> String {
        self.entry.text().to_string()
    }

    fn set_choice(&mut self, choice: Option<&Choice>) {
        self.choice.set_visible(choice.is_some());
        let Some(choice) = choice else {
            return;
        };
        if self.shown_choice.as_ref() != Some(&choice.items) {
            let items: Vec<&str> = choice.items.iter().map(String::as_str).collect();
            self.choice.set_model(Some(&gtk::StringList::new(&items)));
            self.shown_choice = Some(choice.items.clone());
        }
        // Setting the selection it already has reports nothing
        self.choice.set_selected(choice.selected as u32);
    }

    fn set_buttons(&mut self, titles: &'static [&'static str], destructive: bool) {
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
        }
        for (index, title) in titles.iter().enumerate() {
            let button = gtk::Button::with_label(title);
            let last = index + 1 == titles.len();
            if last {
                button.add_css_class(if destructive {
                    "destructive-action"
                } else {
                    "suggested-action"
                });
            }
            let notify = self.notify.clone();
            button.connect_clicked(move |_| notify(DialogInput::Button(index)));
            self.buttons.append(&button);
            // Return picks the last button, or the first (Cancel) when the
            // last one deletes something; it also takes the focus a link
            // button before it would otherwise have
            if (last && !destructive) || (index == 0 && destructive) {
                self.dialog.set_default_widget(Some(&button));
                self.dialog.set_focus(Some(&button));
            }
        }
        self.shown_buttons = titles;
        self.shown_destructive = destructive;
    }
}

impl Drop for DialogWindow {
    fn drop(&mut self) {
        self.alive.set(false);
        let dialog = self.dialog.clone();
        glib::idle_add_local_once(move || {
            dialog.force_close();
        });
    }
}

fn add_css(css: &str) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// Color a widget's foreground (and so a symbolic icon) with `color`.
fn tint_widget(widget: &impl IsA<gtk::Widget>, color: &str) {
    let class = "capycal-tint";
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(".{class} {{ color: {color}; }}"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    widget.add_css_class(class);
}
