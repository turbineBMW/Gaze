//! Gaze: a camera app for the Linux desktop (GTK 4, libadwaita, GStreamer),
//! following the Omarchy theme.

mod camera;
mod omarchy;
mod settings;
mod stats;
mod theme;
mod window;

use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

pub const APP_ID: &str = "io.github.turbinebmw.Gaze";

/// What the application keeps alive: the theme watcher and the window.
type AppState = Option<(theme::OmarchyTheme, Rc<window::Window>)>;

const CSS: &str = "
.viewfinder-frame { background-color: black; }
picture.viewfinder.mirrored { transform: scaleX(-1); }
.flash { background-color: white; }
.stats { padding: 12px 14px; border-radius: 12px; }
.timer { padding: 6px 12px; border-radius: 999px; font-weight: bold; }
label.countdown {
  font-size: 160px; font-weight: 800; color: white;
  text-shadow: 0 2px 24px rgb(0 0 0 / 70%);
}
menubutton.self-timer > button { padding-left: 10px; padding-right: 12px; }
/* Toasts sit above the shutter row instead of on it. */
toast { margin-bottom: 100px; }
button.shutter {
  min-width: 68px; min-height: 68px; padding: 0; border-radius: 999px;
  border: 4px solid white; background: none;
  box-shadow: 0 0 0 2px rgb(0 0 0 / 35%), inset 0 0 0 2px rgb(0 0 0 / 35%);
}
button.shutter:hover { background-color: rgb(255 255 255 / 12%); }
button.shutter > .inner {
  min-width: 52px; min-height: 52px; border-radius: 999px;
  transition: all 150ms ease-out;
}
button.shutter.photo > .inner { background-color: white; }
button.shutter.video > .inner { background-color: var(--error-bg-color); }
button.shutter.recording > .inner {
  min-width: 26px; min-height: 26px; border-radius: 6px;
  background-color: var(--error-bg-color);
}
";

fn main() -> glib::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("GAZE_LOG", "warn")).init();
    if let Err(error) = gst::init() {
        eprintln!("GStreamer: {error}");
        return glib::ExitCode::FAILURE;
    }
    glib::set_application_name("Gaze");
    let app = adw::Application::builder().application_id(APP_ID).build();
    let state: Rc<RefCell<AppState>> = Rc::default();
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        let display = gtk::gdk::Display::default().expect("a display");
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let theme = theme::OmarchyTheme::install(&display);
        let window = window::Window::new(app);
        state.replace(Some((theme, window)));
    });
    app.run()
}
