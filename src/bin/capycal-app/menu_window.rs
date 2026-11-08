//! The tray menu as a window: for desktops without a tray, for starting the
//! app a second time, and on macOS, where the GTK app has no tray.

use adw::prelude::*;
use gtk::glib;

use crate::menu::{Action, Item};

pub struct MenuWindow {
    window: adw::Window,
    banner: adw::Banner,
    page: adw::PreferencesPage,
    groups: Vec<adw::PreferencesGroup>,
    shown: Vec<Item>,
    activate: std::rc::Rc<dyn Fn(Action)>,
}

impl MenuWindow {
    /// `activate` gets the action of a clicked item, `closed` runs when the
    /// window closes.
    pub fn open(
        application: &adw::Application,
        activate: impl Fn(Action) + 'static,
        closed: impl Fn() + 'static,
    ) -> Self {
        let banner = adw::Banner::builder()
            .title(
                "Your desktop shows no tray icons. On GNOME, enable the \"AppIndicator and \
                 KStatusNotifierItem Support\" extension to get the Capycal icon.",
            )
            .revealed(false)
            .build();
        let page = adw::PreferencesPage::new();
        page.add(&hero());
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&banner);
        content.append(&page);
        let view = adw::ToolbarView::new();
        // The logo below names the window
        view.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
        view.set_content(Some(&content));
        let window = adw::Window::builder()
            .application(application)
            .title("Capycal")
            .default_width(420)
            .default_height(640)
            .content(&view)
            .build();
        crate::window_keys(&window);
        window.connect_close_request(move |_| {
            closed();
            glib::Propagation::Proceed
        });
        window.present();
        Self {
            window,
            banner,
            page,
            groups: Vec::new(),
            shown: Vec::new(),
            activate: std::rc::Rc::new(activate),
        }
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// Explain the missing tray icon above the items.
    pub fn show_tray_hint(&self, show: bool) {
        self.banner.set_revealed(show);
    }

    /// Show `items`, one group per section between separators.
    pub fn update(&mut self, items: &[Item]) {
        if self.shown == items {
            return;
        }
        for group in self.groups.drain(..) {
            self.page.remove(&group);
        }
        let mut group = adw::PreferencesGroup::new();
        for item in items {
            if *item == Item::Separator {
                self.page.add(&group);
                self.groups.push(group);
                group = adw::PreferencesGroup::new();
                continue;
            }
            group.add(&self.row(item));
        }
        self.page.add(&group);
        self.groups.push(group);
        self.shown = items.to_vec();
    }

    fn row(&self, item: &Item) -> gtk::Widget {
        let on_click = |action: &Action| {
            let action = action.clone();
            let activate = self.activate.clone();
            move || activate(action.clone())
        };
        match item {
            Item::Status(text) => adw::ActionRow::builder()
                .title(glib::markup_escape_text(text))
                .css_classes(["dim-label"])
                .build()
                .upcast(),
            Item::Action {
                action,
                label,
                enabled,
            } => {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(label))
                    .activatable(true)
                    .sensitive(*enabled)
                    .build();
                let (icon, classes): (&str, &[&str]) = match action {
                    Action::SyncNow => (crate::icons::SYNC, &[]),
                    Action::Settings => ("emblem-system-symbolic", &[]),
                    Action::ShowProblems => ("dialog-warning-symbolic", &["warning"]),
                    Action::About => ("help-about-symbolic", &[]),
                    Action::Quit => ("application-exit-symbolic", &[]),
                };
                row.add_prefix(
                    &gtk::Image::builder()
                        .icon_name(icon)
                        .css_classes(classes.to_vec())
                        .build(),
                );
                let click = on_click(action);
                row.connect_activated(move |_| click());
                row.upcast()
            }
            Item::Separator => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
        }
    }
}

/// The app icon and name above the items.
fn hero() -> adw::PreferencesGroup {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_bottom(8)
        .build();
    content.append(
        &gtk::Image::builder()
            .icon_name(crate::icons::APP)
            .pixel_size(96)
            .build(),
    );
    content.append(
        &gtk::Label::builder()
            .label("Capycal")
            .css_classes(["title-2"])
            .build(),
    );
    content.append(
        &gtk::Label::builder()
            .label(format!("Version {}", capycal::VERSION))
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    let group = adw::PreferencesGroup::new();
    group.add(&content);
    group
}

impl Drop for MenuWindow {
    fn drop(&mut self) {
        self.window.destroy();
    }
}
