//! The few things Gaze remembers, as `key=value` lines in
//! `$XDG_CONFIG_HOME/gaze/settings`.

use gtk::glib;
use std::path::PathBuf;

pub struct Settings {
    /// The camera last chosen, by name.
    pub camera: Option<String>,
    /// Show the viewfinder mirrored (front cameras).
    pub mirror: bool,
    /// Self-timer in seconds; 0 is off.
    pub timer: u32,
}

/// The self-timer lengths offered, in seconds.
pub const TIMERS: [u32; 4] = [0, 3, 5, 10];

fn path() -> PathBuf {
    glib::user_config_dir().join("gaze").join("settings")
}

impl Settings {
    pub fn load() -> Self {
        let mut settings = Self {
            camera: None,
            mirror: true,
            timer: 0,
        };
        let Ok(text) = std::fs::read_to_string(path()) else {
            return settings;
        };
        for line in text.lines() {
            match line.split_once('=') {
                Some(("camera", v)) if !v.is_empty() => settings.camera = Some(v.to_string()),
                Some(("mirror", v)) => settings.mirror = v.trim() == "true",
                Some(("timer", v)) => {
                    if let Ok(t) = v.trim().parse::<u32>() {
                        if TIMERS.contains(&t) {
                            settings.timer = t;
                        }
                    }
                }
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
            "camera={}\nmirror={}\ntimer={}\n",
            self.camera.as_deref().unwrap_or(""),
            self.mirror,
            self.timer
        );
        if let Err(error) = std::fs::write(&path, text) {
            log::warn!("cannot save {}: {error}", path.display());
        }
    }
}
