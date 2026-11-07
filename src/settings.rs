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
    /// Feed the virtual camera whenever a camera is open.
    pub virtual_camera: bool,
    /// Rule-of-thirds grid over the picture.
    pub grid: bool,
    /// Chosen format per camera name, as `Format::id` strings.
    pub formats: Vec<(String, String)>,
    /// The microphone to record with (a PipeWire node name); `None` is the
    /// default one.
    pub mic: Option<String>,
    /// Time-lapse speed for recordings; 1 is off.
    pub lapse: u32,
}

/// The time-lapse speeds offered.
pub const LAPSES: [u32; 6] = [1, 5, 10, 30, 60, 120];

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
            virtual_camera: false,
            grid: false,
            formats: Vec::new(),
            mic: None,
            lapse: 1,
        };
        let Ok(text) = std::fs::read_to_string(path()) else {
            return settings;
        };
        for line in text.lines() {
            match line.split_once('=') {
                Some(("camera", v)) if !v.is_empty() => settings.camera = Some(v.to_string()),
                Some(("mirror", v)) => settings.mirror = v.trim() == "true",
                Some(("virtual_camera", v)) => settings.virtual_camera = v.trim() == "true",
                Some(("grid", v)) => settings.grid = v.trim() == "true",
                Some(("mic", v)) if !v.trim().is_empty() => settings.mic = Some(v.trim().into()),
                Some(("lapse", v)) => {
                    if let Ok(l) = v.trim().parse::<u32>() {
                        if LAPSES.contains(&l) {
                            settings.lapse = l;
                        }
                    }
                }
                Some((key, v)) if key.starts_with("format.") && !v.trim().is_empty() => {
                    settings.set_format(&key["format.".len()..], Some(v.trim()));
                }
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

    pub fn format_for(&self, camera: &str) -> Option<&str> {
        self.formats
            .iter()
            .find(|(c, _)| c == camera)
            .map(|(_, f)| f.as_str())
    }

    /// Remember (or with `None`, forget) the format for a camera.
    pub fn set_format(&mut self, camera: &str, format: Option<&str>) {
        self.formats.retain(|(c, _)| c != camera);
        if let Some(format) = format {
            self.formats.push((camera.to_string(), format.to_string()));
        }
    }

    pub fn save(&self) {
        let path = path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut text = format!(
            "camera={}\nmirror={}\ntimer={}\nvirtual_camera={}\ngrid={}\n",
            self.camera.as_deref().unwrap_or(""),
            self.mirror,
            self.timer,
            self.virtual_camera,
            self.grid
        );
        for (camera, format) in &self.formats {
            text.push_str(&format!("format.{camera}={format}\n"));
        }
        if let Some(mic) = &self.mic {
            text.push_str(&format!("mic={mic}\n"));
        }
        text.push_str(&format!("lapse={}\n", self.lapse));
        if let Err(error) = std::fs::write(&path, text) {
            log::warn!("cannot save {}: {error}", path.display());
        }
    }
}
