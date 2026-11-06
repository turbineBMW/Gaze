//! The few things Gaze remembers, as `key=value` lines in
//! `$XDG_CONFIG_HOME/gaze/settings`.

use gtk::glib;
use std::path::PathBuf;

pub struct Settings {
    /// The camera last chosen, by name.
    pub camera: Option<String>,
    /// Show the viewfinder mirrored (front cameras).
    pub mirror: bool,
}

fn path() -> PathBuf {
    glib::user_config_dir().join("gaze").join("settings")
}

impl Settings {
    pub fn load() -> Self {
        let mut settings = Self {
            camera: None,
            mirror: true,
        };
        let Ok(text) = std::fs::read_to_string(path()) else {
            return settings;
        };
        for line in text.lines() {
            match line.split_once('=') {
                Some(("camera", v)) if !v.is_empty() => settings.camera = Some(v.to_string()),
                Some(("mirror", v)) => settings.mirror = v.trim() == "true",
                _ => {}
            }
        }
        settings
    }

    pub fn save(&self) {
        let path = path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = format!(
            "camera={}\nmirror={}\n",
            self.camera.as_deref().unwrap_or(""),
            self.mirror
        );
        if let Err(error) = std::fs::write(&path, text) {
            log::warn!("cannot save {}: {error}", path.display());
        }
    }
}
