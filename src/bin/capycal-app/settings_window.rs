//! The Settings window in GTK: a sidebar of syncs and accounts and an editor
//! drawn from a `SettingsView`, like the macOS window. It only renders and
//! reports input; `settings_view` decides everything else.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::settings_view::{
    ButtonSpec, Choice, ChoiceIcon, ChoiceStyle, Control, EventPreview, FieldId, HeroImage,
    PopupItem, SettingsInput, SettingsScreen, SettingsView, SidebarItem, steps_text,
};

const DRAG_SVG: &[u8] = include_bytes!("icons/capycal-drag-symbolic.svg");
const SIDEBAR_WIDTH: i32 = 200;
/// The spacing scale of the UI guidelines.
const GAP_S: i32 = 4;
const GAP_M: i32 = 8;
const GAP_L: i32 = 12;
/// The column right of the pipeline steps that holds their drag handles.
const GUTTER: i32 = 28;
const CSS: &str = "
.capycal-choice { padding: 10px 14px; }
.capycal-choice:checked { box-shadow: inset 0 0 0 2px @accent_bg_color; background: alpha(@accent_bg_color, 0.1); color: @window_fg_color; }
.capycal-event { background: alpha(@accent_bg_color, 0.15); border-left: 3px solid @accent_bg_color; border-radius: 6px; padding: 4px 8px; }
.capycal-steps { color: @accent_color; }
.capycal-icon { background: @accent_bg_color; color: @accent_fg_color; border-radius: 10px; padding: 8px; }
.capycal-icon.off { background: alpha(@window_fg_color, 0.35); }
.capycal-side-icon { background: @accent_bg_color; color: @accent_fg_color; border-radius: 6px; padding: 3px; }
.capycal-side-icon.off { background: alpha(@window_fg_color, 0.35); }
.capycal-side-icon.problem { background: #e66100; }
.capycal-stage:drop(active) { box-shadow: none; }
.capycal-drop-mark { background: @accent_bg_color; min-height: 3px; border-radius: 2px; }
row.capycal-plain { padding: 8px 12px; }
/* The plus cut out of the circle, like macOS's plus.circle.fill */
menubutton.capycal-add-step > button { color: @window_bg_color; }
row.capycal-hinted .edit-icon { -gtk-icon-size: 0; min-width: 0; min-height: 0; margin: 0; padding: 0; }
row.expander list.nested > row.capycal-drop-row, list.boxed-list > row.capycal-drop-row { min-height: 0; padding: 0 12px; border-bottom: none; }
row.capycal-any { box-shadow: inset 4px 0 alpha(currentColor, 0.3); }
.capycal-stage > list { transition: outline-color 150ms ease-in-out; outline: 2px solid transparent; outline-offset: 0; }
.capycal-stage.active > list { outline-color: @accent_bg_color; }
.capycal-add-icon { color: @accent_color; }
.capycal-round { border-radius: 999px; padding-left: 14px; padding-right: 14px; }
.capycal-tag { border-radius: 999px; padding: 1px 8px; background: alpha(currentColor, 0.1); }
.capycal-name { font-size: 22pt; font-weight: bold; padding: 0; min-height: 0; }
.capycal-joint { min-height: 14px; }
.capycal-number { background: alpha(@accent_bg_color, 0.15); color: @accent_color; border-radius: 999px; min-width: 20px; min-height: 20px; font-size: smaller; font-weight: bold; }
";

thread_local! {
    /// The size the last full Settings window had.
    static LAST_SIZE: Cell<Option<(i32, i32)>> = const { Cell::new(None) };
}

/// A sidebar section: title, entries, the id of an entry.
type SidebarSection<'a> = (&'a str, &'a [SidebarItem], fn(String) -> FieldId, bool);

/// The widgets of one drawn control, for updates in place.
enum Drawn {
    None,
    Label(gtk::Label),
    /// A note shown as the subtitle of the row above it.
    Subtitle(adw::ActionRow),
    /// A message with a warning sign, hidden while empty.
    Warning(gtk::Label),
    Text {
        entry: gtk::Editable,
        error: gtk::Label,
        /// A row's placeholder, at its end while it is empty.
        hint: Option<gtk::Label>,
    },
    /// A switch row, or a row with a check box.
    Check(adw::ActionRow, Option<gtk::CheckButton>),
    Popup(adw::ComboRow, Vec<PopupItem>),
    Hero(gtk::Label, gtk::Label),
    /// The toggle (card) or check button (row) of each item; None for a
    /// header.
    Choices(Vec<Option<gtk::Widget>>),
    Header {
        title: Option<gtk::Label>,
        name: Option<(gtk::Entry, gtk::Label)>,
        subtitle: gtk::Label,
        switch: Option<gtk::Switch>,
        status: gtk::Label,
    },
    Segments(adw::ToggleGroup),
    Preview {
        modes: adw::ToggleGroup,
        source: [gtk::Label; 4],
        copy: [gtk::Label; 4],
        copy_box: gtk::Box,
    },
    TransformRow(adw::ExpanderRow),
    Footer(gtk::Label, gtk::Image, gtk::ProgressBar, Vec<gtk::Button>),
    Buttons(Vec<gtk::Button>),
    ButtonRows(Vec<adw::ButtonRow>),
    Disclosure(gtk::Button),
}

/// A drop mark and the conditions it would leave where they are.
type DropMark = (gtk::Widget, Vec<usize>);

/// A part of the editor: its controls stacked, rows in boxed lists between
/// the other widgets.
struct Section {
    outer: gtk::Box,
    /// The list the next row goes into; another widget ends it.
    list: Option<gtk::ListBox>,
    /// The row a note after it describes.
    last_row: Option<adw::ActionRow>,
    /// In a group of the app's own settings, not a sync's step.
    switches: bool,
}

impl Section {
    fn new(outer: gtk::Box) -> Self {
        Self {
            outer,
            list: None,
            last_row: None,
            switches: false,
        }
    }
}

pub struct SettingsWindow {
    window: adw::Window,
    /// The sidebar with the app's version below it.
    sidebar_pane: gtk::Box,
    version: gtk::Label,
    sidebar: gtk::Box,
    editor: gtk::Box,
    editor_scroll: gtk::ScrolledWindow,
    /// Where new controls go: the editor, an open group or step.
    section: Section,
    /// The open transform the next controls go into, as rows.
    expander: Option<adw::ExpanderRow>,
    /// The condition being drawn: its first row still takes the handle.
    condition: Option<(usize, bool)>,
    /// True inside an `AnyOf` box.
    any_of: bool,
    /// The column right of the stage being drawn, for its drag handle.
    gutter: Option<gtk::Box>,
    /// The open row the next text fields go into, side by side.
    text_row: Option<gtk::Box>,
    /// The drop mark of each joint between transform steps.
    marks: Rc<RefCell<Vec<gtk::Widget>>>,
    /// The drop mark of each place a dragged condition can land, and the
    /// conditions it would leave where they are.
    condition_marks: Rc<RefCell<Vec<DropMark>>>,
    /// The box of each step, and the one worked in (it shows a ring).
    stages: Rc<RefCell<Vec<gtk::Box>>>,
    active_stage: Rc<Cell<Option<usize>>>,
    /// The transform open in the last view: one opened since slides open.
    open_transform: Option<usize>,
    /// The column right of the editor that keeps the preview in view.
    aside: gtk::Box,
    banner: adw::Banner,
    /// The bar at the bottom: buttons left, a text, buttons right.
    footer: gtk::Box,
    footer_start: gtk::Box,
    footer_text: gtk::Label,
    /// The warning sign before a footer text that keeps Save off.
    footer_sign: gtk::Image,
    /// How far a removal of synced events is, after the text.
    footer_progress: gtk::ProgressBar,
    footer_end: gtk::Box,
    notify: Rc<dyn Fn(SettingsInput)>,
    /// True while the window sets values, so their signals are not input.
    updating: Rc<Cell<bool>>,
    drawn: Vec<Drawn>,
    /// The widgets the last rebuild took out, kept alive until the next
    /// one: the button that was clicked may still be finishing its click.
    retired: Vec<gtk::Widget>,
    /// Each entry's button, icon and "not saved yet" dot.
    sidebar_entries: Vec<(gtk::ToggleButton, gtk::Image, gtk::Label)>,
    general_entry: Option<(gtk::ToggleButton, gtk::Image)>,
    last: Option<SettingsView>,
}

impl SettingsWindow {
    /// `notify` gets the input, also `CloseRequested` when the user closes
    /// the window (the screen decides whether it closes). With a `parent`
    /// (the wizard over Settings) the window is modal on it.
    pub fn open(
        application: &adw::Application,
        view: &SettingsView,
        notify: impl Fn(SettingsInput) + 'static,
        parent: Option<&SettingsWindow>,
    ) -> Self {
        let notify: Rc<dyn Fn(SettingsInput)> = Rc::new(notify);
        let sidebar = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(8)
            .margin_end(8)
            .width_request(SIDEBAR_WIDTH)
            .build();
        let editor = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(22)
            .margin_end(22)
            .build();
        let banner = adw::Banner::new("");
        let editor_scroll = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&editor)
            .build();
        let footer_start = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let footer_text = gtk::Label::builder()
            .css_classes(["dim-label"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let footer_sign = gtk::Image::builder()
            .icon_name("dialog-warning-symbolic")
            .css_classes(["warning"])
            .margin_start(6)
            .visible(false)
            .build();
        // The text next to the buttons it is about
        let footer_end = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let end = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        end.append(&footer_sign);
        end.append(&footer_text);
        footer_text.set_margin_end(6);
        let footer_progress = gtk::ProgressBar::builder()
            .width_request(100)
            .valign(gtk::Align::Center)
            .margin_end(6)
            .visible(false)
            .build();
        footer_progress
            .update_property(&[gtk::accessible::Property::Label("Removing synced events")]);
        end.append(&footer_progress);
        end.append(&footer_end);
        let footer_bar = gtk::CenterBox::builder()
            .start_widget(&footer_start)
            .end_widget(&end)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let footer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        footer.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        footer.append(&footer_bar);
        let aside = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .width_request(240)
            .margin_top(18)
            .margin_end(18)
            .build();
        let middle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        middle.append(&editor_scroll);
        middle.append(&aside);
        let right = gtk::Box::new(gtk::Orientation::Vertical, 0);
        right.append(&banner);
        right.append(&middle);
        right.append(&footer);
        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&sidebar)
            .vexpand(true)
            .build();
        let version = gtk::Label::builder()
            .css_classes(["dim-label", "caption"])
            .halign(gtk::Align::Start)
            .margin_start(16)
            .margin_bottom(12)
            .build();
        let sidebar_pane = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["sidebar-pane"])
            .build();
        sidebar_pane.append(&sidebar_scroll);
        sidebar_pane.append(&version);
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&sidebar_pane)
            .end_child(&right)
            .shrink_start_child(false)
            .position(SIDEBAR_WIDTH + 16)
            .build();
        let view_bar = adw::ToolbarView::new();
        view_bar.add_top_bar(&adw::HeaderBar::new());
        view_bar.set_content(Some(&paned));
        let window = adw::Window::builder()
            .application(application)
            .title("Settings")
            .default_width(1100)
            .default_height(680)
            .content(&view_bar)
            .build();
        let closing = notify.clone();
        window.connect_close_request(move |_| {
            // The screen may ask about unsaved changes; the app drops the
            // window when it should close
            closing(SettingsInput::CloseRequested);
            glib::Propagation::Stop
        });
        if view.compact {
            window.set_default_size(560, 640);
        } else if let Some((width, height)) = LAST_SIZE.get() {
            window.set_default_size(width, height);
        }
        end_editing_on_escape_or_background(&window, &editor);
        add_shortcuts(&window, notify.clone());
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &gtk::prelude::WidgetExt::display(&window),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let mut settings = Self {
            window,
            sidebar_pane,
            version,
            sidebar,
            section: Section::new(editor.clone()),
            expander: None,
            condition: None,
            any_of: false,
            text_row: None,
            gutter: None,
            marks: Rc::new(RefCell::new(Vec::new())),
            condition_marks: Rc::new(RefCell::new(Vec::new())),
            stages: Rc::new(RefCell::new(Vec::new())),
            active_stage: Rc::new(Cell::new(None)),
            open_transform: None,
            editor,
            editor_scroll,
            aside,
            banner,
            footer,
            footer_start,
            footer_text,
            footer_sign,
            footer_progress,
            footer_end,
            notify,
            updating: Rc::new(Cell::new(false)),
            drawn: Vec::new(),
            retired: Vec::new(),
            sidebar_entries: Vec::new(),
            general_entry: None,
            last: None,
        };
        settings.show(view);
        if let Some(parent) = parent {
            settings.window.set_transient_for(Some(&parent.window));
            settings.window.set_modal(true);
        }
        settings.window.present();
        settings
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// Draw `view`: in place while its shape stays the same, so a field
    /// being typed in keeps its focus; else from scratch.
    pub fn show(&mut self, view: &SettingsView) {
        // The wizard shows no sidebar
        self.sidebar_pane.set_visible(!view.compact);
        self.version.set_label(&view.version);
        self.window.set_title(Some(&view.title));
        let names = |items: &[SidebarItem]| -> Vec<String> {
            items.iter().map(|i| i.name.clone()).collect()
        };
        let in_place = self.last.as_ref().is_some_and(|last| {
            SettingsScreen::same_shape(last, view)
                && names(&last.syncs) == names(&view.syncs)
                && names(&last.accounts) == names(&view.accounts)
        });
        self.updating.set(true);
        if in_place {
            self.update(view);
        } else {
            self.rebuild(view);
        }
        self.updating.set(false);
        self.last = Some(view.clone());
    }

    /// A closure that sends `input` unless the window is setting values.
    fn sender(&self) -> impl Fn(SettingsInput) + Clone + 'static {
        let notify = self.notify.clone();
        let updating = self.updating.clone();
        move |input| {
            if !updating.get() {
                notify(input);
            }
        }
    }

    fn rebuild(&mut self, view: &SettingsView) {
        // The same entry stays where it was scrolled to
        let same = self
            .last
            .as_ref()
            .is_some_and(|last| last.generation == view.generation);
        let top = self.editor_scroll.vadjustment().value();
        if same {
            // Else GTK moves the focus of a removed control to the first
            // field and scrolls up to it
            gtk::prelude::GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
        }
        let mut retired = Vec::new();
        for stack in [
            &self.sidebar,
            &self.editor,
            &self.aside,
            &self.footer_start,
            &self.footer_end,
        ] {
            while let Some(child) = stack.first_child() {
                stack.remove(&child);
                retired.push(child);
            }
        }
        self.retired = retired;
        self.section = Section::new(self.editor.clone());
        self.expander = None;
        self.condition = None;
        self.any_of = false;
        self.gutter = None;
        self.text_row = None;
        self.marks.borrow_mut().clear();
        self.condition_marks.borrow_mut().clear();
        self.stages.borrow_mut().clear();
        self.drawn.clear();
        self.sidebar_entries.clear();
        self.banner.set_title(view.banner.as_deref().unwrap_or(""));
        self.banner.set_revealed(view.banner.is_some());
        self.rebuild_sidebar(view);
        self.aside.set_visible(
            view.controls
                .iter()
                .any(|c| matches!(c, Control::Preview { .. })),
        );
        self.footer.set_visible(
            view.controls
                .iter()
                .any(|c| matches!(c, Control::Footer { .. })),
        );
        let open = view.controls.iter().find_map(|c| match c {
            Control::TransformRow {
                index, open: true, ..
            } => Some(*index),
            _ => None,
        });
        let was_open = self.open_transform;
        for control in &view.controls {
            let drawn = self.draw(control, was_open);
            self.drawn.push(drawn);
        }
        self.open_transform = open;
        // GTK would focus the first field and select its text, where a key
        // press replaces the name
        if !view.compact {
            gtk::prelude::GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
            // Also once GTK moved the focus away from a removed widget
            let window = self.window.downgrade();
            let name = self.drawn.iter().find_map(|d| match d {
                Drawn::Header {
                    name: Some((entry, _)),
                    ..
                } => Some(entry.downgrade()),
                _ => None,
            });
            glib::idle_add_local_once(move || {
                if let Some(window) = window.upgrade() {
                    gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
                }
                if let Some(entry) = name.and_then(|n| n.upgrade()) {
                    entry.select_region(0, 0);
                }
            });
        }
        if same {
            // Once the new controls have their size and GTK scrolled to
            // the field it focused
            let adjustment = self.editor_scroll.vadjustment();
            glib::idle_add_local_once(move || adjustment.set_value(top));
        }
    }

    fn rebuild_sidebar(&mut self, view: &SettingsView) {
        let send = self.sender();
        // The app's own settings come first
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let icon = gtk::Image::builder()
            .icon_name("emblem-system-symbolic")
            .pixel_size(14)
            .valign(gtk::Align::Center)
            .css_classes(["capycal-side-icon"])
            .build();
        // General turns orange while the config has a problem
        side_icon_state(&icon, true, true, view.general.problem);
        content.append(&icon);
        content.append(
            &gtk::Label::builder()
                .label(&view.general.name)
                .xalign(0.0)
                .build(),
        );
        let general = gtk::ToggleButton::builder()
            .child(&content)
            .active(view.general.selected)
            .css_classes(["flat"])
            .build();
        let s = send.clone();
        general.connect_clicked(move |_| s(SettingsInput::Pressed(FieldId::SidebarGeneral)));
        self.sidebar.append(&general);
        self.general_entry = Some((general, icon));
        let sections: [SidebarSection; 2] = [
            ("Syncs", &view.syncs, FieldId::SidebarSync, true),
            ("Accounts", &view.accounts, FieldId::SidebarAccount, false),
        ];
        for (title, entries, id, sync) in sections {
            let heading = gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .css_classes(["caption-heading", "dim-label"])
                .margin_top(10)
                .margin_start(4)
                .build();
            self.sidebar.append(&heading);
            let list = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .build();
            for item in entries {
                let icon = gtk::Image::builder()
                    .icon_name(if sync {
                        "emblem-synchronizing-symbolic"
                    } else {
                        "network-server-symbolic"
                    })
                    .pixel_size(14)
                    .valign(gtk::Align::Center)
                    .css_classes(["capycal-side-icon"])
                    .build();
                side_icon_state(&icon, sync, item.active, item.problem);
                let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                content.append(&icon);
                content.append(
                    &gtk::Label::builder()
                        .label(&item.name)
                        .xalign(0.0)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .build(),
                );
                let dot = gtk::Label::builder()
                    .label("\u{25cf}")
                    .css_classes(["dim-label", "caption"])
                    .tooltip_text("Not saved yet")
                    .visible(item.draft)
                    .build();
                content.append(&dot);
                let b = gtk::ToggleButton::builder()
                    .child(&content)
                    .active(item.selected)
                    .css_classes(["flat"])
                    .build();
                if sync {
                    duplicate_menu(&b, &item.name, send.clone());
                }
                let send = send.clone();
                let id = id(item.name.clone());
                b.connect_clicked(move |_| send(SettingsInput::Pressed(id.clone())));
                list.append(&b);
                self.sidebar_entries.push((b, icon, dot));
            }
            self.sidebar.append(&list);
            let (label, add) = if sync {
                ("Add Sync\u{2026}", FieldId::AddSync)
            } else {
                ("Add Account\u{2026}", FieldId::AddAccount)
            };
            // A row like the entries, with an accent "+"
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            content.append(
                &gtk::Image::builder()
                    .icon_name("list-add-symbolic")
                    .pixel_size(14)
                    .css_classes(["capycal-add-icon"])
                    .margin_start(3)
                    .margin_end(3)
                    .build(),
            );
            content.append(&gtk::Label::builder().label(label).xalign(0.0).build());
            let b = gtk::Button::builder()
                .child(&content)
                .css_classes(["flat"])
                .build();
            let s = send.clone();
            b.connect_clicked(move |_| s(SettingsInput::Pressed(add.clone())));
            self.sidebar.append(&b);
        }
    }

    /// Take dropped conditions on the open container: each lands at the
    /// drop mark nearest to the pointer, unless that one keeps it where it
    /// is.
    fn condition_drop_target(&self) {
        let target = gtk::DropTarget::new(u64::static_type(), gtk::gdk::DragAction::MOVE);
        target.set_preload(true);
        let marks = self.condition_marks.clone();
        let slot = move |target: &gtk::DropTarget, from: u64, y: f64| {
            let widget = target.widget()?;
            let marks = marks.borrow();
            let (slot, (_, stays)) = marks.iter().enumerate().min_by_key(|(_, (mark, _))| {
                let middle = gtk::graphene::Point::new(0.0, mark.height() as f32 / 2.0);
                mark.compute_point(&widget, &middle)
                    .map_or(i64::MAX, |p| (f64::from(p.y()) - y).abs() as i64)
            })?;
            (!stays.contains(&(from as usize))).then_some(slot)
        };
        let marks = self.condition_marks.clone();
        let show = move |shown: Option<usize>| {
            for (i, (mark, _)) in marks.borrow().iter().enumerate() {
                mark.set_opacity(if shown == Some(i) { 1.0 } else { 0.0 });
            }
        };
        let (at, shown) = (slot.clone(), show.clone());
        target.connect_motion(move |target, _, y| {
            let from = target.value().and_then(|v| v.get::<u64>().ok());
            shown(from.and_then(|from| at(target, from, y)));
            gtk::gdk::DragAction::MOVE
        });
        let shown = show.clone();
        target.connect_leave(move |_| shown(None));
        let send = self.sender();
        target.connect_drop(move |target, value, _, y| {
            show(None);
            let Ok(from) = value.get::<u64>() else {
                return false;
            };
            if let Some(slot) = slot(target, from, y) {
                send(SettingsInput::Moved(
                    FieldId::Condition(from as usize),
                    slot,
                ));
            }
            true
        });
        self.section.outer.add_controller(target);
    }

    /// Put `row` into the open transform, or else into the section's list,
    /// which starts with it if another widget came last.
    fn add_row<R: IsA<gtk::ListBoxRow> + IsA<gtk::Widget>>(&mut self, row: &R) {
        let row: &gtk::ListBoxRow = row.upcast_ref();
        if self.any_of {
            row.add_css_class("capycal-any");
        }
        // A condition's rows line up behind the handle of its first one
        if let Some((index, first)) = self.condition {
            let prefix = if first {
                self.condition = Some((index, false));
                condition_handle(index, row.upcast_ref::<gtk::Widget>())
            } else {
                gtk::Box::builder().width_request(16).build().upcast()
            };
            if let Some(r) = row.dynamic_cast_ref::<adw::ActionRow>() {
                r.add_prefix(&prefix);
            } else if let Some(r) = row.dynamic_cast_ref::<adw::EntryRow>() {
                r.add_prefix(&prefix);
            }
        }
        if let Some(expander) = &self.expander {
            expander.add_row(row);
        } else {
            let outer = &self.section.outer;
            let list = self.section.list.get_or_insert_with(|| {
                let list = gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::None)
                    .css_classes(["boxed-list"])
                    .build();
                outer.append(&list);
                list
            });
            list.append(row);
        }
        self.section.last_row = row.dynamic_cast_ref::<adw::ActionRow>().cloned();
    }

    /// Put a widget that is no row below the rows before it; in an open
    /// transform it becomes a row of its own.
    fn add_widget(&mut self, widget: &impl IsA<gtk::Widget>) {
        if self.expander.is_some() {
            self.add_row(&plain_row(widget));
        } else {
            self.section.list = None;
            self.section.last_row = None;
            self.section.outer.append(widget);
        }
    }

    /// A widget inside a list as a row of its own, else below it.
    fn add_inline(&mut self, widget: &impl IsA<gtk::Widget>) {
        if self.section.list.is_some() {
            self.add_row(&plain_row(widget));
        } else {
            self.add_widget(widget);
        }
    }

    /// `widget` with an empty gutter column right of it, so it lines up
    /// with the steps.
    fn append_with_gutter(&mut self, widget: &impl IsA<gtk::Widget>) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        widget.set_hexpand(true);
        row.append(widget);
        row.append(&gtk::Box::builder().width_request(GUTTER).build());
        self.editor.append(&row);
    }

    /// A row titled `label` with `control` at its end.
    fn suffix_row(&mut self, label: &str, control: &impl IsA<gtk::Widget>) -> adw::ActionRow {
        let row = adw::ActionRow::builder().title(label).build();
        control.set_valign(gtk::Align::Center);
        row.add_suffix(control);
        self.add_row(&row);
        row
    }

    /// A note: the subtitle of the row above it, else a line of its own.
    fn note(&mut self, text: &str) -> Drawn {
        if let Some(row) = self.section.last_row.clone() {
            row.set_subtitle(text);
            return Drawn::Subtitle(row);
        }
        let label = gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        self.add_inline(&label);
        set_shown(&label, !text.is_empty());
        Drawn::Label(label)
    }

    fn button(&self, (id, title, enabled): &ButtonSpec) -> gtk::Button {
        let b = gtk::Button::with_label(title);
        if let FieldId::Sponsor(i) = id
            && let Some(option) = crate::sponsor::OPTIONS.get(*i)
        {
            b.set_child(Some(
                &adw::ButtonContent::builder()
                    .label(title)
                    .icon_name(crate::icons::symbolic(option.title, option.svg))
                    .build(),
            ));
        }
        // A condition's buttons are small icons, like a step's: moving is
        // mostly a drag; the title stays their accessibility label and tooltip
        let arrow = match id {
            FieldId::ConditionMoveUp(_) => Some("go-up-symbolic"),
            FieldId::ConditionMoveDown(_) => Some("go-down-symbolic"),
            FieldId::ConditionRemove(_) => Some("user-trash-symbolic"),
            _ => None,
        };
        if let Some(icon) = arrow {
            b.set_icon_name(icon);
            b.set_tooltip_text(Some(title));
            b.update_property(&[gtk::accessible::Property::Label(title)]);
        }
        b.set_sensitive(*enabled);
        let send = self.sender();
        let id = id.clone();
        b.connect_clicked(move |_| send(SettingsInput::Pressed(id.clone())));
        b
    }

    /// A button standing on its own or ending a part of the window: a pill
    /// like the buttons of a dialog.
    fn pill_button(&self, spec: &ButtonSpec) -> gtk::Button {
        let b = self.button(spec);
        b.add_css_class("capycal-round");
        b
    }

    /// Draw `control`; `was_open` is the transform open before, for the
    /// chevrons to turn from.
    fn draw(&mut self, control: &Control, was_open: Option<usize>) -> Drawn {
        let send = self.sender();
        match control {
            Control::Note(text) => self.note(text),
            Control::Error(text) => {
                let label = gtk::Label::builder().xalign(0.0).wrap(true).build();
                self.add_inline(&label);
                show_warning(&label, Some(text.as_str()).filter(|t| !t.is_empty()));
                Drawn::Warning(label)
            }
            Control::Group(title) => {
                let steps = !self.stages.borrow().is_empty();
                let group = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(GAP_L)
                    .margin_top(GAP_L)
                    .build();
                if !title.is_empty() {
                    group.append(&section_title(title));
                }
                // Below a sync's steps, as wide as they are
                if steps {
                    self.append_with_gutter(&group);
                } else {
                    self.editor.append(&group);
                }
                self.section = Section::new(group);
                self.section.switches = !steps;
                Drawn::None
            }
            Control::Stage { label, .. } => {
                let stage = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(GAP_M)
                    .css_classes(["capycal-stage"])
                    .build();
                // The step worked in shows a ring: focus in it or a click
                let index = self.stages.borrow().len();
                if self.active_stage.get() == Some(index) {
                    stage.add_css_class("active");
                }
                let activate = {
                    let (stages, active) = (self.stages.clone(), self.active_stage.clone());
                    move || {
                        active.set(Some(index));
                        for (i, stage) in stages.borrow().iter().enumerate() {
                            if i == index {
                                stage.add_css_class("active");
                            } else {
                                stage.remove_css_class("active");
                            }
                        }
                    }
                };
                let focus = gtk::EventControllerFocus::new();
                let on_focus = activate.clone();
                focus.connect_enter(move |_| on_focus());
                stage.add_controller(focus);
                let click = gtk::GestureClick::builder()
                    .propagation_phase(gtk::PropagationPhase::Capture)
                    .build();
                click.connect_pressed(move |_, _, _, _| activate());
                stage.add_controller(click);
                self.stages.borrow_mut().push(stage.clone());
                stage.append(&section_title(label));
                stage.set_hexpand(true);
                let gutter = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .width_request(GUTTER)
                    .valign(gtk::Align::Center)
                    .build();
                let row = gtk::Box::new(gtk::Orientation::Horizontal, GAP_S);
                row.append(&stage);
                row.append(&gutter);
                self.editor.append(&row);
                self.gutter = Some(gutter);
                self.section = Section::new(stage);
                Drawn::None
            }
            Control::Row(label) => {
                let fields = gtk::Box::new(gtk::Orientation::Horizontal, GAP_M);
                self.suffix_row(label, &fields);
                self.text_row = Some(fields);
                Drawn::None
            }
            Control::RowEnd => {
                self.text_row = None;
                Drawn::None
            }
            Control::AnyOf(label) => {
                let caption = gtk::Label::builder()
                    .label(label.as_str())
                    .xalign(0.0)
                    .css_classes(["caption-heading", "dim-label"])
                    .build();
                self.any_of = true;
                self.add_widget(&caption);
                Drawn::None
            }
            Control::AnyOfEnd => {
                self.any_of = false;
                Drawn::None
            }
            Control::ConditionEnd => {
                self.condition = None;
                Drawn::None
            }
            Control::DropMark { slot, stays } => {
                // The first mark is drawn in the step itself: its box takes
                // the drop for all of them. u64, not the u32 of a dragged
                // step, so the step's own drop target does not take a
                // condition and the other way round
                if *slot == 0 {
                    self.condition_drop_target();
                }
                let mark = gtk::Box::builder()
                    .css_classes(["capycal-drop-mark"])
                    .opacity(0.0)
                    .build();
                let row = plain_row(&mark);
                row.add_css_class("capycal-drop-row");
                self.add_row(&row);
                self.condition_marks
                    .borrow_mut()
                    .push((mark.upcast(), stays.clone()));
                Drawn::None
            }
            Control::Condition(id) => {
                if let FieldId::Condition(index) = *id {
                    self.condition = Some((index, true));
                }
                Drawn::None
            }
            Control::GroupEnd => {
                self.section = Section::new(self.editor.clone());
                self.expander = None;
                Drawn::None
            }
            Control::Joint { id, items } => {
                let joint = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .hexpand(true)
                    .css_classes(["capycal-joint"])
                    .build();
                let line = || {
                    gtk::Separator::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .height_request(12)
                        .halign(gtk::Align::Center)
                        .build()
                };
                joint.append(&line());
                if let Some(id) = id {
                    // A menu of the transforms; the first item is the title
                    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    // Scrolls on a small screen instead of not fitting
                    let scroll = gtk::ScrolledWindow::builder()
                        .hscrollbar_policy(gtk::PolicyType::Never)
                        .propagate_natural_width(true)
                        .propagate_natural_height(true)
                        .max_content_height(360)
                        .child(&list)
                        .build();
                    let popover = gtk::Popover::builder().child(&scroll).build();
                    for (i, item) in items.iter().enumerate().skip(1) {
                        if item.header {
                            list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
                            list.append(
                                &gtk::Label::builder()
                                    .label(&item.title)
                                    .xalign(0.0)
                                    .margin_start(10)
                                    .margin_top(4)
                                    .css_classes(["dim-label", "caption-heading"])
                                    .build(),
                            );
                            continue;
                        }
                        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
                        text.append(&gtk::Label::builder().label(&item.title).xalign(0.0).build());
                        if let Some(hint) = &item.hint {
                            text.append(
                                &gtk::Label::builder()
                                    .label(hint)
                                    .xalign(0.0)
                                    .wrap(true)
                                    .max_width_chars(40)
                                    .css_classes(["dim-label", "caption"])
                                    .build(),
                            );
                        }
                        let b = gtk::Button::builder()
                            .child(&text)
                            .css_classes(["flat"])
                            .build();
                        b.update_property(&[gtk::accessible::Property::Label(&item.title)]);
                        if let Some(hint) = &item.hint {
                            b.update_property(&[gtk::accessible::Property::Description(hint)]);
                        }
                        let send = send.clone();
                        let id = id.clone();
                        let popover = popover.clone();
                        b.connect_clicked(move |_| {
                            popover.popdown();
                            send(SettingsInput::Chose(id.clone(), i, None));
                        });
                        list.append(&b);
                    }
                    let add = gtk::MenuButton::builder()
                        .icon_name("list-add-symbolic")
                        .tooltip_text("Add a transform here")
                        .popover(&popover)
                        .halign(gtk::Align::Center)
                        .css_classes(["circular", "suggested-action", "capycal-add-step"])
                        .build();
                    add.update_property(&[gtk::accessible::Property::Label(
                        "Add a transform here",
                    )]);
                    // Where a dragged step would land: a line through the
                    // "+", which sits centered between the joint's lines
                    let mark = gtk::Box::builder()
                        .css_classes(["capycal-drop-mark"])
                        .hexpand(true)
                        .valign(gtk::Align::Center)
                        .opacity(0.0)
                        .build();
                    let holder = gtk::Overlay::builder().child(&mark).build();
                    holder.add_overlay(&add);
                    holder.set_measure_overlay(&add, true);
                    joint.append(&holder);
                    joint.append(&line());
                    self.marks.borrow_mut().push(mark.upcast());
                }
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                row.append(&joint);
                row.append(&gtk::Box::builder().width_request(GUTTER).build());
                self.editor.append(&row);
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
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let image = gtk::Image::builder()
                    .pixel_size(28)
                    .css_classes(["capycal-icon"])
                    .valign(gtk::Align::Center)
                    .build();
                if *icon {
                    image.set_icon_name(Some("network-server-symbolic"));
                    image.add_css_class("account");
                    row.append(&image);
                }
                let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
                text.set_hexpand(true);
                // The title and, at its right, the switch
                let title_line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                text.append(&title_line);
                let (title_label, name_entry) = match name {
                    Some((id, error)) => {
                        let entry = gtk::Entry::builder()
                            .text(title)
                            .css_classes(["capycal-name", "flat"])
                            .hexpand(true)
                            .build();
                        let id = id.clone();
                        let send = send.clone();
                        report_leave(&entry, &id, send.clone());
                        entry.connect_changed(move |e| {
                            send(SettingsInput::Text(id.clone(), e.text().to_string()))
                        });
                        title_line.append(&entry);
                        let error_label = gtk::Label::builder().xalign(0.0).build();
                        show_warning(&error_label, error.as_deref());
                        text.append(&error_label);
                        (None, Some((entry, error_label)))
                    }
                    None => {
                        let label = gtk::Label::builder()
                            .label(title)
                            .xalign(0.0)
                            .hexpand(true)
                            .css_classes(["capycal-name"])
                            .build();
                        title_line.append(&label);
                        (Some(label), None)
                    }
                };
                let subtitle_label = gtk::Label::builder()
                    .label(subtitle)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["dim-label"])
                    .build();
                text.append(&subtitle_label);
                // How the saved sync is doing
                // One line, so the editor below does not move when it changes
                let status_label = gtk::Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build();
                show_status(&status_label, status.as_ref());
                text.append(&status_label);
                row.append(&text);
                let switch_widget = switch.as_ref().map(|(id, on)| {
                    let s = gtk::Switch::builder()
                        .active(*on)
                        .valign(gtk::Align::Center)
                        .tooltip_text("Enabled")
                        .build();
                    let id = id.clone();
                    let send = send.clone();
                    s.connect_active_notify(move |s| {
                        send(SettingsInput::Toggled(id.clone(), s.is_active()))
                    });
                    title_line.append(&s);
                    s
                });
                if switch.is_some() {
                    self.append_with_gutter(&row);
                } else {
                    self.add_widget(&row);
                }
                Drawn::Header {
                    title: title_label,
                    name: name_entry,
                    subtitle: subtitle_label,
                    switch: switch_widget,
                    status: status_label,
                }
            }
            Control::Segments {
                id,
                label,
                items,
                selected,
                centered,
            } => {
                let id = id.clone();
                let group = toggle_group(items, *selected, move |i| {
                    send(SettingsInput::Selected(id.clone(), i))
                });
                // A join stands on its own line: a pill
                if *centered {
                    group.add_css_class("round");
                    group.set_halign(gtk::Align::Center);
                } else {
                    group.set_halign(gtk::Align::Start);
                }
                if label.is_empty() {
                    self.add_widget(&group);
                } else {
                    self.suffix_row(label, &group);
                }
                Drawn::Segments(group)
            }
            Control::Preview {
                mode,
                source_label,
                source,
                copy_label,
                copy,
            } => {
                self.aside.append(
                    &gtk::Label::builder()
                        .label("One event on its way")
                        .xalign(0.0)
                        .css_classes(["heading"])
                        .build(),
                );
                // A sample or the source's next event
                // Standing on its own above the preview: a pill
                let modes = toggle_group(
                    &["Sample".to_string(), "Next event".to_string()],
                    Some(mode.unwrap_or(0)),
                    move |i| send(SettingsInput::Selected(FieldId::PreviewMode, i)),
                );
                modes.add_css_class("round");
                modes.set_halign(gtk::Align::Start);
                modes.set_visible(mode.is_some());
                self.aside.append(&modes);
                let (source_box, source_labels) = event_box(source_label, Some(source));
                let (copy_box, copy_labels) = event_box(copy_label, copy.as_ref());
                self.aside.append(&source_box);
                self.aside.append(
                    &gtk::Image::builder()
                        .icon_name("go-down-symbolic")
                        .css_classes(["dim-label"])
                        .build(),
                );
                self.aside.append(&copy_box);
                Drawn::Preview {
                    modes,
                    source: source_labels,
                    copy: copy_labels,
                    copy_box,
                }
            }
            Control::TransformRow {
                index,
                summary,
                open,
            } => {
                // The handle drags the step; dropped on another step it goes
                // before or after that one
                let handle = gtk::Image::builder()
                    .icon_name(crate::icons::symbolic("drag", DRAG_SVG))
                    .tooltip_text("Drag to move")
                    .css_classes(["dim-label"])
                    .build();
                let source = gtk::DragSource::builder()
                    .actions(gtk::gdk::DragAction::MOVE)
                    .build();
                let from = *index as u32;
                source.connect_prepare(move |_, _, _| {
                    Some(gtk::gdk::ContentProvider::for_value(&from.to_value()))
                });
                let stage = self.section.outer.clone();
                source.connect_drag_begin(move |source, _| {
                    source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&stage))), 0, 0);
                });
                handle.add_controller(source);
                let expander = adw::ExpanderRow::builder()
                    .title(summary)
                    .use_markup(false)
                    .build();
                match &self.gutter {
                    Some(gutter) => gutter.append(&handle),
                    None => expander.add_prefix(&handle),
                }
                let target = gtk::DropTarget::new(u32::static_type(), gtk::gdk::DragAction::MOVE);
                target.set_preload(true);
                let at = *index;
                // The gap above or below this step, by the pointer's half
                let gap = move |target: &gtk::DropTarget, y: f64| {
                    let height = target.widget().map_or(0, |w| w.height());
                    if y < f64::from(height) / 2.0 {
                        at
                    } else {
                        at + 1
                    }
                };
                let marks = self.marks.clone();
                let show = move |shown: Option<usize>| {
                    for (i, mark) in marks.borrow().iter().enumerate() {
                        mark.set_opacity(if shown == Some(i) { 1.0 } else { 0.0 });
                    }
                };
                let from_of = |target: &gtk::DropTarget| {
                    target
                        .value()
                        .and_then(|v| v.get::<u32>().ok())
                        .map(|f| f as usize)
                };
                let shown = show.clone();
                target.connect_motion(move |target, _, y| {
                    let g = gap(target, y);
                    // Next to itself it would stay where it is
                    let moves = from_of(target).is_some_and(|f| g != f && g != f + 1);
                    shown(moves.then_some(g));
                    gtk::gdk::DragAction::MOVE
                });
                let shown = show.clone();
                target.connect_leave(move |_| shown(None));
                let s = send.clone();
                target.connect_drop(move |target, value, _, y| {
                    show(None);
                    let Ok(from) = value.get::<u32>() else {
                        return false;
                    };
                    s(SettingsInput::Moved(
                        FieldId::TransformRow(from as usize),
                        gap(target, y),
                    ));
                    true
                });
                self.section.outer.add_controller(target);
                expander.add_prefix(
                    &gtk::Label::builder()
                        .label((index + 1).to_string())
                        .valign(gtk::Align::Center)
                        .css_classes(["capycal-number"])
                        .build(),
                );
                let remove = gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .tooltip_text("Remove")
                    .css_classes(["flat"])
                    .valign(gtk::Align::Center)
                    .build();
                let id = FieldId::TransformRowRemove(*index);
                let s = send.clone();
                remove.connect_clicked(move |_| s(SettingsInput::Pressed(id.clone())));
                expander.add_suffix(&remove);
                // One just opened slides open
                let opened = *open && was_open != Some(*index);
                expander.set_expanded(*open && !opened);
                if opened {
                    let (e, updating) = (expander.downgrade(), self.updating.clone());
                    glib::idle_add_local_once(move || {
                        if let Some(e) = e.upgrade() {
                            updating.set(true);
                            e.set_expanded(true);
                            updating.set(false);
                        }
                    });
                }
                let id = FieldId::TransformRow(*index);
                let s = send.clone();
                expander.connect_expanded_notify(move |_| s(SettingsInput::Pressed(id.clone())));
                // For screen readers and the keyboard: what the row is, and
                // a menu that moves it without dragging
                let step = index + 1;
                expander.upcast_ref::<gtk::Widget>().update_property(&[
                    gtk::accessible::Property::Label(&format!("Step {step}: {summary}")),
                ]);
                remove.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Remove step {step}"
                ))]);
                handle.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Move step {step}"
                ))]);
                let at = *index;
                step_menu(
                    &expander,
                    [
                        (
                            "Move Up",
                            SettingsInput::Moved(FieldId::TransformRow(at), at.saturating_sub(1)),
                        ),
                        (
                            "Move Down",
                            SettingsInput::Moved(FieldId::TransformRow(at), at + 2),
                        ),
                        (
                            "Remove",
                            SettingsInput::Pressed(FieldId::TransformRowRemove(at)),
                        ),
                    ],
                    send,
                );
                self.add_row(&expander);
                // The fields that follow an open row belong to it
                self.expander = Some(expander.clone());
                Drawn::TransformRow(expander)
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
                let send_text = send.clone();
                let id_text = id.clone();
                let changed = move |e: &gtk::Editable| {
                    send_text(SettingsInput::Text(id_text.clone(), e.text().to_string()))
                };
                let mut hint_label = None;
                let entry: gtk::Editable = if let Some(fields) = &self.text_row {
                    // A short field with its label after it
                    let e = gtk::Entry::builder()
                        .placeholder_text(placeholder)
                        .width_chars(5)
                        .build();
                    fields.append(&e);
                    fields.append(&gtk::Label::builder().label(label).margin_end(GAP_L).build());
                    e.upcast()
                } else {
                    let row: adw::EntryRow = if *secure {
                        adw::PasswordEntryRow::builder()
                            .title(label)
                            .build()
                            .upcast()
                    } else {
                        adw::EntryRow::builder().title(label).build()
                    };
                    // An entry row draws a placeholder over its title, and
                    // only while typing: the hint stays at its end instead
                    let hint = gtk::Label::builder()
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .valign(gtk::Align::Center)
                        .css_classes(["dim-label"])
                        .build();
                    // The row's own pencil would stand before the hint: its
                    // copy follows it, before a password row's eye
                    let pencil = gtk::Image::builder()
                        .icon_name("document-edit-symbolic")
                        .valign(gtk::Align::Center)
                        .accessible_role(gtk::AccessibleRole::Presentation)
                        .build();
                    row.add_css_class("capycal-hinted");
                    row.add_suffix(&hint);
                    row.add_suffix(&pencil);
                    if let Some(suffixes) = hint.parent().and_downcast::<gtk::Box>() {
                        suffixes.reorder_child_after(&hint, None::<&gtk::Widget>);
                        suffixes.reorder_child_after(&pencil, Some(&hint));
                    }
                    // Like the row's own: shown while the field has no focus
                    let focus = gtk::EventControllerFocus::new();
                    let shown = pencil.clone();
                    focus.connect_enter(move |_| shown.set_visible(false));
                    focus.connect_leave(move |_| pencil.set_visible(true));
                    row.add_controller(focus);
                    if !suggestions.is_empty() {
                        suggestion_button(&row, suggestions);
                    }
                    self.add_row(&row);
                    hint_label = Some(hint);
                    row.upcast()
                };
                entry.set_text(value);
                show_placeholder(&entry, hint_label.as_ref(), placeholder);
                if let Some(hint) = &hint_label {
                    let hint = hint.clone();
                    entry.connect_changed(move |e| {
                        hint.set_visible(e.text().is_empty() && !hint.label().is_empty())
                    });
                }
                entry.set_sensitive(*enabled);
                report_leave(&entry, id, send);
                entry.connect_changed(changed);
                let error_label = gtk::Label::builder().xalign(0.0).wrap(true).build();
                self.add_inline(&error_label);
                show_warning(&error_label, error.as_deref());
                Drawn::Text {
                    entry,
                    error: error_label,
                    hint: hint_label,
                }
            }
            Control::Check {
                id,
                label,
                on,
                hint,
            } => {
                let id = id.clone();
                if self.section.switches {
                    let row = adw::SwitchRow::builder().title(label).active(*on).build();
                    row.set_tooltip_text(hint.as_deref());
                    row.connect_active_notify(move |r| {
                        send(SettingsInput::Toggled(id.clone(), r.is_active()))
                    });
                    self.add_row(&row);
                    return Drawn::Check(row.upcast(), None);
                }
                let check = gtk::CheckButton::builder()
                    .active(*on)
                    .valign(gtk::Align::Center)
                    .build();
                check.connect_toggled(move |c| {
                    send(SettingsInput::Toggled(id.clone(), c.is_active()))
                });
                check.update_property(&[gtk::accessible::Property::Label(label)]);
                let row = adw::ActionRow::builder()
                    .title(label)
                    .use_markup(false)
                    .activatable_widget(&check)
                    .build();
                row.set_tooltip_text(hint.as_deref());
                row.add_prefix(&check);
                self.add_row(&row);
                Drawn::Check(row, Some(check))
            }
            Control::Popup {
                id,
                label,
                items,
                selected,
                buttons,
            } => {
                let row = adw::ComboRow::builder().title(label).build();
                combo_items(&row, items);
                row.set_selected(selected.map_or(gtk::INVALID_LIST_POSITION, |s| s as u32));
                let id = id.clone();
                row.connect_selected_notify(move |d| {
                    let index = d.selected();
                    if index != gtk::INVALID_LIST_POSITION {
                        send(SettingsInput::Chose(id.clone(), index as usize, None));
                    }
                });
                for spec in buttons {
                    let b = self.button(spec);
                    b.set_valign(gtk::Align::Center);
                    if b.icon_name().is_some() {
                        b.add_css_class("flat");
                    }
                    row.add_suffix(&b);
                }
                self.add_row(&row);
                Drawn::Popup(row, items.clone())
            }
            Control::Hero { image, title, text } => {
                if let Some(image) = image {
                    let (icon, size) = match image {
                        HeroImage::App => (crate::icons::APP, 128),
                        HeroImage::Done => ("object-select-symbolic", 64),
                    };
                    let image_widget = gtk::Image::builder()
                        .icon_name(icon)
                        .pixel_size(size)
                        .margin_top(12)
                        .build();
                    if *image == HeroImage::Done {
                        image_widget.add_css_class("success");
                    }
                    self.add_widget(&image_widget);
                }
                let title = gtk::Label::builder()
                    .label(title)
                    .wrap(true)
                    .justify(gtk::Justification::Center)
                    .css_classes(["title-1"])
                    .build();
                let text = gtk::Label::builder()
                    .label(text)
                    .wrap(true)
                    .max_width_chars(50)
                    .justify(gtk::Justification::Center)
                    .css_classes(["dim-label"])
                    .margin_bottom(6)
                    .build();
                self.add_widget(&title);
                self.add_widget(&text);
                Drawn::Hero(title, text)
            }
            Control::Steps { current, count } => Drawn::Label({
                let label = gtk::Label::builder()
                    .label(steps_text(*current, *count))
                    .css_classes(["capycal-steps"])
                    .build();
                self.add_widget(&label);
                label
            }),
            Control::Choices {
                id,
                style,
                items,
                selected,
            } => {
                let list = gtk::Box::new(
                    gtk::Orientation::Vertical,
                    if *style == ChoiceStyle::Cards {
                        GAP_M
                    } else {
                        GAP_S
                    },
                );
                let mut toggles = Vec::new();
                // Rows: a boxed list under each header, a radio button each
                let mut rows: Option<gtk::ListBox> = None;
                let mut first: Option<gtk::CheckButton> = None;
                for (i, item) in items.iter().enumerate() {
                    if item.header {
                        list.append(&section_title(&item.title));
                        rows = None;
                        toggles.push(None);
                        continue;
                    }
                    let select = {
                        let (send, id) = (send.clone(), id.clone());
                        move || send(SettingsInput::Selected(id.clone(), i))
                    };
                    if *style == ChoiceStyle::Cards {
                        let b = choice_button(item);
                        b.set_active(*selected == Some(i));
                        b.connect_clicked(move |_| select());
                        list.append(&b);
                        toggles.push(Some(b.upcast()));
                        continue;
                    }
                    let check = gtk::CheckButton::builder()
                        .active(*selected == Some(i))
                        .valign(gtk::Align::Center)
                        .build();
                    check.update_property(&[gtk::accessible::Property::Label(&item.title)]);
                    check.set_group(first.as_ref());
                    first.get_or_insert_with(|| check.clone());
                    check.connect_toggled(move |c| {
                        if c.is_active() {
                            select();
                        }
                    });
                    let row = adw::ActionRow::builder()
                        .title(&item.title)
                        .subtitle(&item.detail)
                        .use_markup(false)
                        .activatable_widget(&check)
                        .sensitive(item.enabled)
                        .build();
                    row.add_prefix(&check);
                    if let Some(hint) = &item.hint {
                        row.add_suffix(
                            &gtk::Label::builder()
                                .label(hint)
                                .valign(gtk::Align::Center)
                                .css_classes(["caption", "capycal-tag"])
                                .build(),
                        );
                    }
                    rows.get_or_insert_with(|| {
                        let rows = gtk::ListBox::builder()
                            .selection_mode(gtk::SelectionMode::None)
                            .css_classes(["boxed-list"])
                            .margin_bottom(GAP_M)
                            .build();
                        list.append(&rows);
                        rows
                    })
                    .append(&row);
                    toggles.push(Some(check.upcast()));
                }
                self.add_widget(&list);
                Drawn::Choices(toggles)
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
                    self.footer_start.append(&b);
                    drawn.push(b);
                }
                for (n, spec) in right.iter().enumerate() {
                    let b = self.pill_button(spec);
                    if n + 1 == right.len() {
                        b.add_css_class("suggested-action");
                        self.window.set_default_widget(Some(&b));
                    }
                    self.footer_end.append(&b);
                    drawn.push(b);
                }
                show_footer_text(&self.footer_text, &self.footer_sign, text, *warning);
                show_progress(&self.footer_progress, *progress);
                Drawn::Footer(
                    self.footer_text.clone(),
                    self.footer_sign.clone(),
                    self.footer_progress.clone(),
                    drawn,
                )
            }
            // In an open transform: rows like the conditions'
            Control::Buttons(buttons) if self.expander.is_some() => {
                let mut drawn = Vec::new();
                for (id, title, enabled) in buttons {
                    let row = adw::ButtonRow::builder()
                        .title(title)
                        .sensitive(*enabled)
                        .build();
                    if *id == FieldId::AddCondition {
                        row.set_start_icon_name(Some("list-add-symbolic"));
                    }
                    let (send, id) = (send.clone(), id.clone());
                    row.connect_activated(move |_| send(SettingsInput::Pressed(id.clone())));
                    self.add_row(&row);
                    drawn.push(row);
                }
                Drawn::ButtonRows(drawn)
            }
            Control::Buttons(buttons) => {
                let row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .halign(if self.section.outer == self.editor {
                        gtk::Align::Start
                    } else {
                        gtk::Align::End
                    })
                    .build();
                let mut drawn = Vec::new();
                for spec in buttons {
                    let b = self.pill_button(spec);
                    row.append(&b);
                    drawn.push(b);
                }
                self.add_widget(&row);
                Drawn::Buttons(drawn)
            }
            Control::Disclosure { id, label, open } => {
                // A pill centered between two hairlines
                let b = gtk::Button::builder()
                    .label(disclosure_title(label, *open))
                    .css_classes(["capycal-round"])
                    .build();
                let id = id.clone();
                b.connect_clicked(move |_| send(SettingsInput::Pressed(id.clone())));
                let line = || {
                    gtk::Separator::builder()
                        .hexpand(true)
                        .valign(gtk::Align::Center)
                        .build()
                };
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.append(&line());
                row.append(&b);
                row.append(&line());
                row.set_margin_top(6);
                if self.stages.borrow().is_empty() {
                    self.add_widget(&row);
                } else {
                    self.append_with_gutter(&row);
                }
                Drawn::Disclosure(b)
            }
        }
    }

    fn update(&mut self, view: &SettingsView) {
        let moved = self
            .last
            .as_ref()
            .is_some_and(|last| last.moves != view.moves);
        self.banner.set_title(view.banner.as_deref().unwrap_or(""));
        let items = view.syncs.iter().map(|i| (i, true));
        let items = items.chain(view.accounts.iter().map(|i| (i, false)));
        if let Some((general, icon)) = &self.general_entry {
            general.set_active(view.general.selected);
            side_icon_state(icon, true, true, view.general.problem);
        }
        for ((b, icon, dot), (item, sync)) in self.sidebar_entries.iter().zip(items) {
            b.set_active(item.selected);
            dot.set_visible(item.draft);
            side_icon_state(icon, sync, item.active, item.problem);
        }
        for (control, drawn) in view.controls.iter().zip(self.drawn.iter_mut()) {
            match (control, drawn) {
                (Control::Error(t), Drawn::Warning(l)) => {
                    show_warning(l, Some(t.as_str()).filter(|t| !t.is_empty()))
                }
                (Control::Note(t), Drawn::Label(l)) => {
                    l.set_label(t);
                    set_shown(l, !t.is_empty());
                }
                (Control::Note(t), Drawn::Subtitle(row)) => row.set_subtitle(t),
                (
                    Control::Text {
                        value,
                        placeholder,
                        error,
                        enabled,
                        ..
                    },
                    Drawn::Text {
                        entry,
                        error: e,
                        hint,
                    },
                ) => {
                    entry.set_sensitive(*enabled);
                    // Unless a move put another condition there
                    if moved {
                        if entry.text() != value.as_str() {
                            entry.set_text(value);
                        }
                    } else {
                        set_unless_focused(entry, value);
                    }
                    show_placeholder(entry, hint.as_ref(), placeholder);
                    show_warning(e, error.as_deref());
                }
                (Control::Check { on, label, .. }, Drawn::Check(row, check)) => {
                    row.set_title(label);
                    match check {
                        Some(check) => check.set_active(*on),
                        None => {
                            if let Some(s) = row.downcast_ref::<adw::SwitchRow>() {
                                s.set_active(*on);
                            }
                        }
                    }
                }
                (
                    Control::Popup {
                        items, selected, ..
                    },
                    Drawn::Popup(d, shown),
                ) => {
                    if items != shown {
                        combo_items(d, items);
                        *shown = items.clone();
                    }
                    d.set_selected(selected.map_or(gtk::INVALID_LIST_POSITION, |s| s as u32));
                }
                (
                    Control::Header {
                        title: text,
                        name: name_spec,
                        subtitle: sub,
                        switch: switch_spec,
                        status: line,
                        ..
                    },
                    Drawn::Header {
                        title,
                        name,
                        subtitle,
                        switch,
                        status,
                    },
                ) => {
                    show_status(status, line.as_ref());
                    if let Some(title) = title {
                        title.set_label(text);
                    }
                    if let (Some((entry, error)), Some((_, message))) = (name, name_spec) {
                        set_unless_focused(entry.upcast_ref(), text);
                        show_warning(error, message.as_deref());
                    }
                    subtitle.set_label(sub);
                    if let (Some(switch), Some((_, on))) = (switch, switch_spec) {
                        switch.set_active(*on);
                    }
                }
                (Control::Segments { selected, .. }, Drawn::Segments(group)) => {
                    group.set_active(selected.map_or(gtk::INVALID_LIST_POSITION, |s| s as u32))
                }
                (
                    Control::Preview {
                        mode,
                        source_label,
                        source: s,
                        copy_label,
                        copy: c,
                    },
                    Drawn::Preview {
                        modes,
                        source,
                        copy,
                        copy_box,
                    },
                ) => {
                    modes.set_visible(mode.is_some());
                    modes.set_active(mode.unwrap_or(0) as u32);
                    fill_event(source, source_label, Some(s));
                    fill_event(copy, copy_label, c.as_ref());
                    copy_box.set_opacity(if c.is_some() { 1.0 } else { 0.6 });
                }
                (Control::TransformRow { summary, .. }, Drawn::TransformRow(row)) => {
                    row.set_title(summary)
                }
                (
                    Control::Footer {
                        left,
                        text,
                        warning,
                        progress,
                        right,
                    },
                    Drawn::Footer(label, sign, bar, drawn),
                ) => {
                    show_footer_text(label, sign, text, *warning);
                    show_progress(bar, *progress);
                    for (b, (_, title, enabled)) in drawn.iter().zip(left.iter().chain(right)) {
                        b.set_label(title);
                        b.set_sensitive(*enabled);
                    }
                }
                (Control::Buttons(buttons), Drawn::ButtonRows(drawn)) => {
                    for (b, (_, title, enabled)) in drawn.iter().zip(buttons) {
                        b.set_title(title);
                        b.set_sensitive(*enabled);
                    }
                }
                (Control::Buttons(buttons), Drawn::Buttons(drawn)) => {
                    for (b, (id, title, enabled)) in drawn.iter().zip(buttons) {
                        // set_label would replace a sponsor button's logo
                        if !matches!(id, FieldId::Sponsor(_)) {
                            b.set_label(title);
                        }
                        b.set_sensitive(*enabled);
                    }
                }
                (Control::Hero { title, text, .. }, Drawn::Hero(t, l)) => {
                    t.set_label(title);
                    l.set_label(text);
                }
                (Control::Steps { current, count }, Drawn::Label(l)) => {
                    l.set_label(&steps_text(*current, *count))
                }
                (Control::Choices { selected, .. }, Drawn::Choices(toggles)) => {
                    for (j, b) in toggles.iter().enumerate() {
                        if let Some(b) = b
                            .as_ref()
                            .and_then(|b| b.downcast_ref::<gtk::ToggleButton>())
                        {
                            b.set_active(*selected == Some(j));
                        } else if let Some(b) = b
                            .as_ref()
                            .and_then(|b| b.downcast_ref::<gtk::CheckButton>())
                        {
                            b.set_active(*selected == Some(j));
                        }
                    }
                }
                (Control::Disclosure { label, open, .. }, Drawn::Disclosure(b)) => {
                    b.set_label(&disclosure_title(label, *open));
                }
                _ => {}
            }
        }
    }
}

/// Set a field's text unless it is being typed in (keeps text and cursor).
fn set_unless_focused(entry: &gtk::Editable, value: &str) {
    let focused = entry.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN);
    if !focused && entry.text() != value {
        entry.set_text(value);
    }
}

fn side_icon_state(icon: &gtk::Image, sync: bool, active: bool, problem: bool) {
    icon.remove_css_class("off");
    icon.remove_css_class("problem");
    if sync && !active {
        icon.add_css_class("off");
    } else if sync && problem {
        icon.add_css_class("problem");
    }
}

/// A status line: dimmed, with a warning sign and in the text color only
/// for a problem.
fn show_status(label: &gtk::Label, status: Option<&(String, bool)>) {
    label.set_tooltip_text(status.map(|(text, _)| text.as_str()));
    match status {
        Some((text, true)) => {
            label.remove_css_class("dim-label");
            show_warning(label, Some(text));
        }
        Some((text, false)) => {
            label.add_css_class("dim-label");
            label.set_label(text);
            label.set_visible(true);
        }
        None => label.set_visible(false),
    }
}

/// A caption above an event drawn like one in a calendar: time, title and
/// details. Returns the box and its labels (caption, time, title, details).
fn event_box(caption: &str, event: Option<&EventPreview>) -> (gtk::Box, [gtk::Label; 4]) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 3);
    // Wrapped at the column's width, so a long text does not widen it
    let label = |classes: &[&str]| {
        gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(30)
            .css_classes(classes.to_vec())
            .build()
    };
    let labels = [
        label(&["caption", "dim-label"]),
        label(&["caption", "numeric"]),
        label(&["heading"]),
        label(&["caption"]),
    ];
    outer.append(&labels[0]);
    let event_widget = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["capycal-event"])
        .build();
    for l in &labels[1..] {
        event_widget.append(l);
    }
    outer.append(&event_widget);
    fill_event(&labels, caption, event);
    (outer, labels)
}

fn fill_event(labels: &[gtk::Label; 4], caption: &str, event: Option<&EventPreview>) {
    labels[0].set_label(caption);
    let (time, title, detail) = match event {
        Some(e) => (e.time.as_str(), e.title.as_str(), e.detail.as_str()),
        None => ("", "No copy", ""),
    };
    labels[1].set_label(time);
    labels[1].set_visible(!time.is_empty());
    labels[2].set_label(title);
    labels[3].set_label(detail);
    labels[3].set_visible(!detail.is_empty());
}

/// A choice as a toggle: a card with icon, title, a line below and the
/// preview of an event.
fn choice_button(item: &Choice) -> gtk::ToggleButton {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    if let Some(icon) = item.icon {
        let name = match icon {
            ChoiceIcon::CalendarApp => "x-office-calendar-symbolic",
            ChoiceIcon::CalDav => "network-server-symbolic",
        };
        content.append(
            &gtk::Image::builder()
                .icon_name(name)
                .pixel_size(24)
                .valign(gtk::Align::Start)
                .build(),
        );
    }
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.append(
        &gtk::Label::builder()
            .label(&item.title)
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    if !item.detail.is_empty() {
        text.append(
            &gtk::Label::builder()
                .label(&item.detail)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label"])
                .build(),
        );
    }
    if let Some(preview) = &item.preview {
        let (event, _) = event_box("", Some(preview));
        event.set_margin_top(4);
        text.append(&event);
    }
    content.append(&text);
    if let Some(hint) = &item.hint {
        let hint = gtk::Label::builder()
            .label(hint)
            .css_classes(["caption", "capycal-tag"])
            .valign(gtk::Align::Center)
            .build();
        // A disabled row dims it already
        if item.enabled {
            hint.add_css_class("dim-label");
        }
        content.append(&hint);
    }
    gtk::ToggleButton::builder()
        .child(&content)
        .css_classes(["card", "capycal-choice"])
        .sensitive(item.enabled)
        .build()
}

/// The items of a combo row: calendars indented under their group, which
/// cannot be chosen; a read-only one marked, since a dropdown cannot grey
/// out single items.
fn combo_items(row: &adw::ComboRow, items: &[PopupItem]) {
    let titles: Vec<String> = items
        .iter()
        .map(|item| match (&item.hint, item.enabled) {
            (Some(hint), false) if !item.header => {
                format!("{} ({})", item.title, hint.to_lowercase())
            }
            _ => item.title.clone(),
        })
        .collect();
    let headers: Vec<bool> = items.iter().map(|i| i.header).collect();
    let grouped = headers.contains(&true);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&gtk::Label::builder().xalign(0.0).build()));
        }
    });
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(text)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) else {
            return;
        };
        label.set_label(&text.string());
        let header = headers.get(item.position() as usize) == Some(&true);
        label.set_margin_start(if grouped && !header { GAP_L } else { 0 });
        for class in ["caption-heading", "dim-label"] {
            if header {
                label.add_css_class(class);
            } else {
                label.remove_css_class(class);
            }
        }
        item.set_selectable(!header);
        item.set_activatable(!header);
    });
    row.set_list_factory(Some(&factory));
    let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
    row.set_model(Some(&gtk::StringList::new(&titles)));
}

/// Mutually exclusive `items`, `selected` one active; `select` gets the one
/// the user picks.
fn toggle_group(
    items: &[String],
    selected: Option<usize>,
    select: impl Fn(usize) + 'static,
) -> adw::ToggleGroup {
    let group = adw::ToggleGroup::new();
    for item in items {
        group.add(adw::Toggle::builder().label(item).build());
    }
    group.set_active(selected.map_or(gtk::INVALID_LIST_POSITION, |s| s as u32));
    group.connect_active_notify(move |g| {
        let active = g.active();
        if active != gtk::INVALID_LIST_POSITION {
            select(active as usize);
        }
    });
    group
}

/// The title of a group or step above its list.
fn section_title(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["heading"])
        .build()
}

/// A row holding a widget that is no row, such as a note or buttons.
fn plain_row(widget: &impl IsA<gtk::Widget>) -> gtk::ListBoxRow {
    gtk::ListBoxRow::builder()
        .child(widget)
        .activatable(false)
        .selectable(false)
        .focusable(false)
        .css_classes(["capycal-plain"])
        .build()
}

/// Show or hide a label, and the row it alone fills.
fn set_shown(label: &gtk::Label, shown: bool) {
    label.set_visible(shown);
    if let Some(row) = label.parent().and_downcast::<gtk::ListBoxRow>() {
        row.set_visible(shown);
    }
}

/// The text a field shows while empty: an entry's placeholder, or a row's
/// hint at its end.
fn show_placeholder(entry: &gtk::Editable, hint: Option<&gtk::Label>, text: &str) {
    match hint {
        Some(hint) => {
            hint.set_label(text);
            hint.set_visible(entry.text().is_empty() && !text.is_empty());
        }
        None => {
            if let Some(e) = entry.downcast_ref::<gtk::Entry>() {
                e.set_placeholder_text(Some(text));
            }
        }
    }
}

/// The handle that drags condition `index`, showing `row` while dragged.
fn condition_handle(index: usize, row: &gtk::Widget) -> gtk::Widget {
    let handle = gtk::Image::builder()
        .icon_name(crate::icons::symbolic("drag", DRAG_SVG))
        .tooltip_text("Drag to move")
        .valign(gtk::Align::Center)
        .css_classes(["dim-label"])
        .build();
    handle.update_property(&[gtk::accessible::Property::Label("Drag to move")]);
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    let from = index as u64;
    source.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&from.to_value()))
    });
    let dragged = row.downgrade();
    source.connect_drag_begin(move |source, _| {
        let paintable = gtk::WidgetPaintable::new(dragged.upgrade().as_ref());
        source.set_icon(Some(&paintable), 0, 0);
    });
    handle.add_controller(source);
    handle.upcast()
}

fn disclosure_title(label: &str, open: bool) -> String {
    if open {
        format!("Hide {} settings \u{25b4}", label.to_lowercase())
    } else {
        format!("{label} settings \u{25be}")
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        // No input from a window that is going away
        self.updating.set(true);
        // The next window opens as large as this one was
        if self.last.as_ref().is_some_and(|l| !l.compact) {
            LAST_SIZE.set(Some(self.window.default_size()));
        }
        self.window.destroy();
    }
}

/// A button at the end of `row` that lists `suggestions`; picking one
/// replaces the text, any other text stays possible.
fn suggestion_button(row: &adw::EntryRow, suggestions: &[String]) {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let popover = gtk::Popover::builder().child(&list).build();
    for suggestion in suggestions {
        let item = gtk::Button::builder()
            .label(suggestion)
            .css_classes(["flat"])
            .build();
        if let Some(label) = item.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.0);
        }
        let (row, popover, text) = (row.downgrade(), popover.clone(), suggestion.clone());
        item.connect_clicked(move |_| {
            popover.popdown();
            if let Some(row) = row.upgrade() {
                row.set_text(&text);
                row.set_position(-1);
            }
        });
        list.append(&item);
    }
    let button = gtk::MenuButton::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text("Suggestions")
        .popover(&popover)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label("Suggestions")]);
    row.add_suffix(&button);
}

/// The footer's text: a warning in the text color after a warning sign,
/// anything else dimmed; the tooltip holds a text cut off.
fn show_footer_text(label: &gtk::Label, sign: &gtk::Image, text: &str, warning: bool) {
    label.set_label(text);
    label.set_tooltip_text(Some(text));
    if warning {
        label.remove_css_class("dim-label");
    } else {
        label.add_css_class("dim-label");
    }
    sign.set_visible(warning);
}

/// A bar for (done, total); hidden without one.
fn show_progress(bar: &gtk::ProgressBar, progress: Option<(usize, usize)>) {
    if let Some((done, total)) = progress {
        bar.set_fraction(done as f64 / total.max(1) as f64);
    }
    bar.set_visible(progress.is_some());
}

/// What is wrong with a field: an orange warning sign and the message in
/// the text color; hidden while there is none.
fn show_warning(label: &gtk::Label, message: Option<&str>) {
    let text = message.unwrap_or("");
    label.set_markup(&format!(
        "<span foreground=\"#e66100\">\u{26a0}</span>  {}",
        gtk::glib::markup_escape_text(text)
    ));
    set_shown(label, message.is_some());
}

/// A right click on a sync's sidebar row offers "Duplicate".
fn duplicate_menu(
    row: &gtk::ToggleButton,
    name: &str,
    send: impl Fn(SettingsInput) + Clone + 'static,
) {
    let item = gtk::Button::builder()
        .label("Duplicate")
        .css_classes(["flat"])
        .build();
    let popover = gtk::Popover::builder()
        .child(&item)
        .has_arrow(false)
        .build();
    popover.set_parent(row);
    let child = popover.clone();
    row.connect_destroy(move |_| child.unparent());
    let (shown, name) = (popover.downgrade(), name.to_string());
    item.connect_clicked(move |_| {
        if let Some(popover) = shown.upgrade() {
            popover.popdown();
        }
        send(SettingsInput::Pressed(FieldId::DuplicateSync(name.clone())));
    });
    let click = gtk::GestureClick::builder().button(3).build();
    click.connect_pressed(move |_, _, x, y| {
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    });
    row.add_controller(click);
}

/// Sends `Left` when the focus leaves `entry`, so its mistakes show.
fn report_leave(
    entry: &impl IsA<gtk::Widget>,
    id: &FieldId,
    send: impl Fn(SettingsInput) + 'static,
) {
    let focus = gtk::EventControllerFocus::new();
    let id = id.clone();
    focus.connect_leave(move |_| send(SettingsInput::Left(id.clone())));
    entry.add_controller(focus);
}

/// Esc in a field, or a click on the editor's background, ends the typing
/// there (the focus leaves the field, so its mistakes show). Elsewhere Esc
/// closes the wizard, which cancels it (Cancel's key on macOS).
fn end_editing_on_escape_or_background(window: &adw::Window, editor: &gtk::Box) {
    let typing = |window: &adw::Window| {
        gtk::prelude::GtkWindowExt::focus(window).is_some_and(|f| f.is::<gtk::Text>())
    };
    let keys = gtk::EventControllerKey::new();
    let w = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if let Some(window) = w.upgrade()
            && key == gtk::gdk::Key::Escape
            && typing(&window)
        {
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
            return glib::Propagation::Stop;
        }
        if let Some(window) = w.upgrade()
            && key == gtk::gdk::Key::Escape
            && gtk::prelude::GtkWindowExt::is_modal(&window)
        {
            gtk::prelude::GtkWindowExt::close(&window);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);
    let click = gtk::GestureClick::new();
    let (w, e) = (window.downgrade(), editor.downgrade());
    click.connect_pressed(move |_, _, x, y| {
        let (Some(window), Some(editor)) = (w.upgrade(), e.upgrade()) else {
            return;
        };
        let picked = editor.pick(x, y, gtk::PickFlags::DEFAULT);
        let control = picked.is_some_and(|p| {
            [
                gtk::Button::static_type(),
                gtk::Text::static_type(),
                gtk::DropDown::static_type(),
                gtk::CheckButton::static_type(),
                gtk::Switch::static_type(),
            ]
            .into_iter()
            .any(|t| p.type_().is_a(t) || p.ancestor(t).is_some())
        });
        if !control && typing(&window) {
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        }
    });
    editor.add_controller(click);
}

/// Ctrl+S, Ctrl+N, Ctrl+W, Ctrl+Q, Ctrl+Delete and, outside a field,
/// Ctrl+Z.
fn add_shortcuts(window: &adw::Window, notify: Rc<dyn Fn(SettingsInput)>) {
    use crate::settings_view::Shortcut;
    let keys = gtk::EventControllerKey::new();
    let w = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let mask = gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::SHIFT_MASK
            | gtk::gdk::ModifierType::ALT_MASK
            | gtk::gdk::ModifierType::SUPER_MASK;
        // Ctrl+Alt+Up / Down move the open step
        let alt = gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK;
        if modifiers & mask == alt {
            let shortcut = match key {
                gtk::gdk::Key::Up => Shortcut::MoveUp,
                gtk::gdk::Key::Down => Shortcut::MoveDown,
                _ => return glib::Propagation::Proceed,
            };
            notify(SettingsInput::Shortcut(shortcut));
            return glib::Propagation::Stop;
        }
        if modifiers & mask != gtk::gdk::ModifierType::CONTROL_MASK {
            return glib::Propagation::Proceed;
        }
        let typing = w.upgrade().is_some_and(|w| {
            gtk::prelude::GtkWindowExt::focus(&w).is_some_and(|f| f.is::<gtk::Text>())
        });
        let shortcut = match key.to_lower() {
            gtk::gdk::Key::s => Shortcut::Save,
            gtk::gdk::Key::n => Shortcut::NewSync,
            gtk::gdk::Key::w => Shortcut::Close,
            gtk::gdk::Key::q => Shortcut::Quit,
            gtk::gdk::Key::Delete | gtk::gdk::Key::BackSpace if !typing => Shortcut::Delete,
            gtk::gdk::Key::z if !typing => Shortcut::Undo,
            _ => return glib::Propagation::Proceed,
        };
        notify(SettingsInput::Shortcut(shortcut));
        glib::Propagation::Stop
    });
    window.add_controller(keys);
}

/// A right click on a step's row offers to move or remove it.
fn step_menu<const N: usize>(
    row: &impl IsA<gtk::Widget>,
    items: [(&str, SettingsInput); N],
    send: impl Fn(SettingsInput) + Clone + 'static,
) {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let popover = gtk::Popover::builder()
        .child(&list)
        .has_arrow(false)
        .build();
    popover.set_parent(row);
    let child = popover.clone();
    row.connect_destroy(move |_| child.unparent());
    for (title, input) in items {
        let item = gtk::Button::builder()
            .label(title)
            .css_classes(["flat"])
            .build();
        if let Some(label) = item.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.0);
        }
        let (shown, send) = (popover.downgrade(), send.clone());
        item.connect_clicked(move |_| {
            if let Some(popover) = shown.upgrade() {
                popover.popdown();
            }
            send(input.clone());
        });
        list.append(&item);
    }
    let click = gtk::GestureClick::builder().button(3).build();
    click.connect_pressed(move |_, _, x, y| {
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    });
    row.add_controller(click);
}
