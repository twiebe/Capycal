//! The tray icon through the StatusNotifierItem D-Bus protocol (KDE, GNOME
//! with the AppIndicator extension, most other Linux desktops). It runs on
//! its own thread; a menu click is only sent to the app as an `Action`.

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};
use tracing::error;

use crate::icons;
use crate::menu::{Action, Icon, Item};
use crate::tray_icon::Rgba;

pub struct Tray {
    handle: Handle<CapycalTray>,
}

struct CapycalTray {
    items: Vec<Item>,
    state: Icon,
    tooltip: String,
    /// The app icon in the sizes the tray may pick from.
    icons: Vec<Rgba>,
    actions: async_channel::Sender<Action>,
    /// Whether a tray host (StatusNotifierWatcher) shows the icon.
    hosted: async_channel::Sender<bool>,
    /// Where the icons are when they are not installed.
    theme_path: String,
}

impl Tray {
    /// Register the tray icon. Fails without a session bus. Without a tray
    /// host (yet) it waits for one and reports `false` on `hosted`.
    pub fn spawn(
        icons: Vec<Rgba>,
        actions: async_channel::Sender<Action>,
        hosted: async_channel::Sender<bool>,
    ) -> Option<Self> {
        let tray = CapycalTray {
            items: Vec::new(),
            state: Icon::Inactive,
            tooltip: "Capycal".to_string(),
            icons,
            actions,
            hosted,
            theme_path: icons::cache_dir().to_string_lossy().into_owned(),
        };
        // The app may start before the desktop's panel at login
        match tray.assume_sni_available(true).spawn() {
            Ok(handle) => Some(Self { handle }),
            Err(e) => {
                error!(error = %e, "cannot create the tray icon");
                None
            }
        }
    }

    /// Show `items`, `state` and `tooltip`; unchanged values are not sent.
    pub fn update(&self, items: &[Item], state: Icon, tooltip: &str) {
        self.handle.update(|tray| {
            if tray.items != items {
                tray.items = items.to_vec();
            }
            tray.state = state;
            if tray.tooltip != tooltip {
                tray.tooltip = tooltip.to_string();
            }
        });
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.handle.shutdown().wait();
    }
}

impl ksni::Tray for CapycalTray {
    // A left click opens the menu, like the macOS menu bar
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "app.capycal".to_string()
    }

    fn title(&self) -> String {
        "Capycal".to_string()
    }

    /// Symbolic icons, so the panel draws them in its own text color; the
    /// pixmap is the fallback for hosts that cannot find them.
    fn icon_name(&self) -> String {
        match self.state {
            Icon::Ok => icons::TRAY,
            Icon::Error => icons::TRAY_ERROR,
            Icon::Syncing => icons::TRAY_SYNCING,
            Icon::Inactive => icons::TRAY_INACTIVE,
        }
        .to_string()
    }

    fn icon_theme_path(&self) -> String {
        self.theme_path.clone()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons
            .iter()
            .map(|icon| ksni::Icon {
                width: icon.size as i32,
                height: icon.size as i32,
                data: icon.for_state(self.state).argb(),
            })
            .collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.tooltip.clone(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        self.items.iter().map(|item| self.menu_item(item)).collect()
    }

    fn watcher_online(&self) {
        let _ = self.hosted.send_blocking(true);
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        let _ = self.hosted.send_blocking(false);
        // Keep serving; the icon appears when a host starts
        true
    }
}

impl CapycalTray {
    fn menu_item(&self, item: &Item) -> MenuItem<Self> {
        let send = |action: &Action| {
            let action = action.clone();
            Box::new(move |tray: &mut Self| {
                let _ = tray.actions.send_blocking(action.clone());
            })
        };
        match item {
            Item::Status(text) => StandardItem {
                label: escape(text),
                enabled: false,
                ..Default::default()
            }
            .into(),
            Item::Separator => MenuItem::Separator,
            Item::Action {
                action,
                label,
                enabled,
            } => StandardItem {
                label: escape(label),
                enabled: *enabled,
                activate: send(action),
                ..Default::default()
            }
            .into(),
        }
    }
}

/// A single underscore marks an access key in menu labels; keep it literal.
fn escape(label: &str) -> String {
    label.replace('_', "__")
}
