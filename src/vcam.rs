//! The virtual camera: a v4l2loopback device named "Gaze" that other apps
//! (Chromium, Meet, Zoom, OBS) open as a camera, fed by Gaze's pipeline.
//!
//! Creating the device needs root: Gaze re-runs itself as
//! `gaze --add-virtual-camera` through `pkexec`, and that invocation asks
//! the v4l2loopback module for a new device over its control node, the way
//! `v4l2loopback-ctl add` would. The device lasts until reboot (or the
//! module is unloaded); it is found again by its name.

use gtk::gio;
use std::path::PathBuf;

pub const LABEL: &str = "Gaze";
pub const ADD_FLAG: &str = "--add-virtual-camera";
pub const INSTALL_FLAG: &str = "--install-virtual-camera-service";
pub const UNINSTALL_FLAG: &str = "--uninstall-virtual-camera-service";

const UNIT: &str = "/etc/systemd/system/gaze-virtual-camera.service";
const MODULES: &str = "/etc/modules-load.d/gaze-v4l2loopback.conf";

/// Whether the boot-time service that recreates the device is installed.
pub fn service_installed() -> bool {
    std::path::Path::new(UNIT).exists()
}

/// Run one of this binary's root tasks through pkexec.
async fn as_root(flag: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let argv: Vec<std::ffi::OsString> = vec!["pkexec".into(), exe.into(), flag.into()];
    let process = gio::Subprocess::newv(
        &argv.iter().map(|a| a.as_os_str()).collect::<Vec<_>>(),
        gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE,
    )
    .map_err(|e| format!("cannot run pkexec: {e}"))?;
    let (_, stderr) = process
        .communicate_utf8_future(None)
        .await
        .map_err(|e| e.to_string())?;
    if process.is_successful() {
        return Ok(());
    }
    let why = stderr.map(|s| s.to_string()).unwrap_or_default();
    let why = why.trim().lines().last().unwrap_or("").to_string();
    Err(if why.is_empty() || why.contains("dismissed") {
        "Cancelled.".into()
    } else {
        why
    })
}

/// Install (or remove) the service that adds the device at boot, after a
/// polkit prompt.
pub async fn set_service(installed: bool) -> Result<(), String> {
    as_root(if installed {
        INSTALL_FLAG
    } else {
        UNINSTALL_FLAG
    })
    .await
}

/// `gaze --install-virtual-camera-service`, as root: a oneshot unit that
/// runs this very binary's `--add-virtual-camera` once the loopback
/// module is loaded, and a modules-load.d entry so it is.
pub fn install_service_as_root() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let unit = format!(
        "[Unit]\n\
         Description=Gaze virtual camera (v4l2loopback device named {LABEL})\n\
         After=systemd-modules-load.service\n\
         ConditionPathExists=/dev/v4l2loopback\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={} {ADD_FLAG}\n\
         RemainAfterExit=yes\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        exe.display()
    );
    std::fs::write(UNIT, unit).map_err(|e| format!("{UNIT}: {e}"))?;
    std::fs::write(MODULES, "v4l2loopback\n").map_err(|e| format!("{MODULES}: {e}"))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "gaze-virtual-camera.service"])
}

pub fn uninstall_service_as_root() -> Result<(), String> {
    let _ = systemctl(&["disable", "gaze-virtual-camera.service"]);
    for path in [UNIT, MODULES] {
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("{path}: {error}"));
            }
        }
    }
    systemctl(&["daemon-reload"])
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The loopback device named [`LABEL`], if there is one.
pub fn find() -> Option<PathBuf> {
    let entries = std::fs::read_dir("/sys/class/video4linux").ok()?;
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| std::fs::read_to_string(e.path().join("name")).is_ok_and(|n| n.trim() == LABEL))
        .map(|e| PathBuf::from("/dev").join(e.file_name()))
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Find the device, or create it (after a polkit prompt) and wait for its
/// node to appear.
pub async fn ensure() -> Result<PathBuf, String> {
    if let Some(path) = find() {
        return Ok(path);
    }
    if !std::path::Path::new("/dev/v4l2loopback").exists() {
        return Err(
            "The v4l2loopback kernel module is not loaded (install v4l2loopback-dkms and \
             run `sudo modprobe v4l2loopback`)."
                .into(),
        );
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let argv: Vec<std::ffi::OsString> = vec!["pkexec".into(), exe.into(), ADD_FLAG.into()];
    let process = gio::Subprocess::newv(
        &argv.iter().map(|a| a.as_os_str()).collect::<Vec<_>>(),
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
    )
    .map_err(|e| format!("cannot run pkexec: {e}"))?;
    let (stdout, stderr) = process
        .communicate_utf8_future(None)
        .await
        .map_err(|e| e.to_string())?;
    if !process.is_successful() {
        let why = stderr.map(|s| s.to_string()).unwrap_or_default();
        let why = why.trim().lines().last().unwrap_or("").to_string();
        return Err(if why.is_empty() || why.contains("dismissed") {
            "Creating the virtual camera was cancelled.".into()
        } else {
            format!("Could not create the virtual camera: {why}")
        });
    }
    let path = PathBuf::from(
        stdout
            .map(|s| s.trim().to_string())
            .filter(|s| s.starts_with("/dev/video"))
            .ok_or("Could not create the virtual camera.")?,
    );
    // udev makes the node and grants access a moment later.
    for _ in 0..40 {
        if std::fs::OpenOptions::new().write(true).open(&path).is_ok() {
            return Ok(path);
        }
        gtk::glib::timeout_future(std::time::Duration::from_millis(50)).await;
    }
    Err(format!("{} did not become writable.", path.display()))
}

/// `gaze --add-virtual-camera`, run as root: add the device and print its
/// path. Mirrors `struct v4l2_loopback_config` and `V4L2LOOPBACK_CTL_ADD`
/// from v4l2loopback 0.13+.
pub fn add_as_root() -> Result<PathBuf, String> {
    #[repr(C)]
    struct Config {
        output_nr: i32,
        unused: i32,
        card_label: [u8; 32],
        min_width: u32,
        max_width: u32,
        min_height: u32,
        max_height: u32,
        max_buffers: i32,
        max_openers: i32,
        debug: i32,
        announce_all_caps: i32,
    }
    let mut label = [0u8; 32];
    label[..LABEL.len()].copy_from_slice(LABEL.as_bytes());
    let config = Config {
        output_nr: -1,
        unused: -1,
        card_label: label,
        min_width: 0,
        max_width: 0,
        min_height: 0,
        max_height: 0,
        max_buffers: 0,
        max_openers: 0,
        debug: 0,
        // Exclusive caps: a camera only while Gaze is feeding it, so apps
        // do not list an empty device.
        announce_all_caps: 0,
    };
    let control = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/v4l2loopback")
        .map_err(|e| format!("/dev/v4l2loopback: {e}"))?;
    // _IOW('~', 1, struct v4l2_loopback_config)
    let request: libc::c_ulong = (1 << 30)
        | ((std::mem::size_of::<Config>() as libc::c_ulong) << 16)
        | ((b'~' as libc::c_ulong) << 8)
        | 1;
    use std::os::fd::AsRawFd;
    // SAFETY: a valid fd and a pointer to a correctly laid out, live struct
    // that the driver only reads.
    let nr = unsafe { libc::ioctl(control.as_raw_fd(), request, &config as *const Config) };
    if nr < 0 {
        return Err(format!(
            "V4L2LOOPBACK_CTL_ADD: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(PathBuf::from(format!("/dev/video{nr}")))
}
