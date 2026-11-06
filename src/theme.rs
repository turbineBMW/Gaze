//! Follow the Omarchy desktop theme, live (the GTK half of `omarchy.rs`,
//! after Rustle's). The theme's CSS goes into a provider above the app
//! stylesheet, and libadwaita is forced light or dark to match the theme's
//! mode. Without Omarchy the provider stays empty and the system decides.

use crate::omarchy;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// One theme switch is a burst of file events; reload once it settles.
const SETTLE: Duration = Duration::from_millis(120);

/// Owns the file monitor, so the application keeps it.
pub struct OmarchyTheme {
    _inner: Rc<Inner>,
}

struct Inner {
    provider: gtk::CssProvider,
    monitor: RefCell<Option<gio::FileMonitor>>,
}

impl OmarchyTheme {
    pub fn install(display: &gtk::gdk::Display) -> Self {
        let inner = Rc::new(Inner {
            provider: gtk::CssProvider::new(),
            monitor: RefCell::new(None),
        });
        gtk::style_context_add_provider_for_display(
            display,
            &inner.provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2,
        );
        inner.provider.connect_parsing_error(|_, section, error| {
            log::warn!(
                "omarchy theme css line {}: {error}",
                section.start_location().lines() + 1
            );
        });
        inner.watch();
        inner.reload();
        Self { _inner: inner }
    }
}

impl Inner {
    fn reload(&self) {
        let theme = omarchy::load(&glib::home_dir());
        self.provider
            .load_from_string(theme.as_ref().map_or("", |theme| theme.css.as_str()));
        adw::StyleManager::default().set_color_scheme(match &theme {
            Some(theme) if theme.light => adw::ColorScheme::ForceLight,
            Some(_) => adw::ColorScheme::ForceDark,
            None => adw::ColorScheme::Default,
        });
    }

    /// `omarchy theme set` renames a staging directory over `current/theme`,
    /// so the watch is on the stable parent.
    fn watch(self: &Rc<Self>) {
        let home = glib::home_dir();
        if !omarchy::detected(&home) {
            return;
        }
        let dir = omarchy::state_dir(&home);
        let monitor = match gio::File::for_path(&dir)
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            Ok(monitor) => monitor,
            Err(error) => {
                log::warn!("cannot watch {}: {error}", dir.display());
                return;
            }
        };
        let pending = Rc::new(Cell::new(false));
        monitor.connect_changed(glib::clone!(
            #[weak(rename_to = inner)]
            self,
            move |_, _, _, _| {
                if pending.replace(true) {
                    return;
                }
                let pending = pending.clone();
                glib::timeout_add_local_once(
                    SETTLE,
                    glib::clone!(
                        #[weak]
                        inner,
                        move || {
                            pending.set(false);
                            inner.reload();
                        }
                    ),
                );
            }
        ));
        self.monitor.replace(Some(monitor));
    }
}
