//! A little Hyprland: pin Gaze's window as a small floating mirror in the
//! corner of its monitor, and put it back. Hyprland 0.56's `hyprctl
//! dispatch` takes a Lua expression (`hl.dsp.*` dispatchers); each
//! direction here is one such expression that finds our window by pid.
//! Everything is optional: without Hyprland the compact mode only hides
//! the window's own chrome.

use std::process::Command;

pub fn available() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

fn dispatch(lua: &str) -> Result<(), String> {
    let output = Command::new("hyprctl")
        .args(["dispatch", lua])
        .output()
        .map_err(|e| format!("hyprctl: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && !text.contains("error") {
        Ok(())
    } else {
        Err(format!("hyprctl: {}", text.trim()))
    }
}

/// Float, shrink and pin the window in the bottom-right corner of its
/// monitor, or undo that.
pub fn set_compact(on: bool, width: i32, height: i32) {
    if !available() {
        return;
    }
    let pid = std::process::id();
    let lua = if on {
        format!(
            "(function() \
               local w = hl.get_window('pid:{pid}') \
               if not w then return hl.dsp.window.float({{ action = 'unset', window = 'pid:{pid}' }}) end \
               hl.dispatch(hl.dsp.window.float({{ action = 'set', window = w }})) \
               hl.dispatch(hl.dsp.window.resize({{ x = {width}, y = {height}, exact = true, window = w }})) \
               local m = w.monitor \
               if m then \
                 hl.dispatch(hl.dsp.window.move({{ \
                   x = m.x + m.width - {width} - 24, y = m.y + m.height - {height} - 24, \
                   exact = true, window = w }})) \
               end \
               return hl.dsp.window.pin({{ action = 'set', window = w }}) \
             end)()"
        )
    } else {
        format!(
            "(function() \
               local w = hl.get_window('pid:{pid}') \
               if not w then return hl.dsp.window.float({{ action = 'unset', window = 'pid:{pid}' }}) end \
               hl.dispatch(hl.dsp.window.pin({{ action = 'unset', window = w }})) \
               return hl.dsp.window.float({{ action = 'unset', window = w }}) \
             end)()"
        )
    };
    if let Err(error) = dispatch(&lua) {
        log::warn!("compact mode: {error}");
    }
}
