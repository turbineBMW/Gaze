//! Gaze: a camera app for the Linux desktop (GTK 4, libadwaita, GStreamer),
//! following the Omarchy theme.

mod camera;
mod codes;
mod controls;
mod motion;
mod omarchy;
mod scan;
mod settings;
mod stats;
mod text;
mod theme;
mod thumbs;
mod vcam;
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
.stats, .code { padding: 12px 14px; border-radius: 12px; }
.code { min-width: 280px; }
.code .payload { font-family: monospace; }
.timer { padding: 6px 12px; border-radius: 999px; font-weight: bold; }
label.countdown {
  font-size: 160px; font-weight: 800; color: white;
  text-shadow: 0 2px 24px rgb(0 0 0 / 70%);
}
menubutton.self-timer > button { padding-left: 10px; padding-right: 12px; }
popover.effects button { min-width: 92px; padding: 4px; }
popover.effects .tile-frame { border-radius: 8px; }
popover.effects button label { font-size: 12px; }
levelbar.mic-level trough { min-height: 8px; background-color: rgb(0 0 0 / 45%); border: none; }
levelbar.mic-level block { min-height: 8px; }
button.thumb { padding: 3px; border-radius: 12px; min-width: 0; min-height: 0; }
.thumb-frame { border-radius: 9px; }
.gallery button { padding: 2px; border-radius: 10px; }
.gallery .thumb-frame { border-radius: 8px; }
button.text-chip {
  min-height: 0; padding: 0 6px; border-radius: 6px;
  background-color: rgb(0 0 0 / 55%); color: white;
  border: 1px solid rgb(255 255 255 / 40%);
}
button.text-chip:hover { background-color: var(--accent-bg-color); color: var(--accent-fg-color); }
button.text-chip label { font-size: 12px; }
popover.effects button:checked { background-color: var(--accent-bg-color); color: var(--accent-fg-color); }
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
    // `gaze --add-virtual-camera` is what Gaze runs as root (through pkexec)
    // to make the loopback device; it prints the device and exits.
    let root_task = match std::env::args().nth(1).as_deref() {
        Some(vcam::ADD_FLAG) => Some(vcam::add_as_root().map(|p| p.display().to_string())),
        Some(vcam::INSTALL_FLAG) => Some(vcam::install_service_as_root().map(|_| String::new())),
        Some(vcam::UNINSTALL_FLAG) => {
            Some(vcam::uninstall_service_as_root().map(|_| String::new()))
        }
        _ => None,
    };
    if let Some(result) = root_task {
        return match result {
            Ok(out) => {
                if !out.is_empty() {
                    println!("{out}");
                }
                glib::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                glib::ExitCode::FAILURE
            }
        };
    }
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("GAZE_LOG", "warn")).init();
    if let Err(error) = gst::init() {
        eprintln!("GStreamer: {error}");
        return glib::ExitCode::FAILURE;
    }
    glib::set_application_name("Gaze");
    let app = adw::Application::builder().application_id(APP_ID).build();
    let state: Rc<RefCell<AppState>> = Rc::default();
    // app.quit closes the window the proper way (the camera is released
    // first), for scripts and the D-Bus action interface.
    let quit = gtk::gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            if let Some(window) = app.active_window() {
                window.close();
            } else {
                app.quit();
            }
        }
    ));
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    // app.open-path opens a file with its default app: what a capture
    // notification does when clicked.
    let open_path = gtk::gio::SimpleAction::new("open-path", Some(glib::VariantTy::STRING));
    open_path.connect_activate(|_, value| {
        if let Some(path) = value.and_then(|v| v.str()) {
            let uri = gtk::gio::File::for_path(path).uri();
            let _ =
                gtk::gio::AppInfo::launch_default_for_uri(&uri, gtk::gio::AppLaunchContext::NONE);
        }
    });
    app.add_action(&open_path);
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
