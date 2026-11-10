//! A camera's V4L2 controls (brightness, exposure, white balance, zoom,
//! focus, whatever the driver offers), read and set straight on the device
//! node with the standard ioctls. Another open of the node is fine for
//! controls while GStreamer streams from it.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Integer {
        min: i64,
        max: i64,
        step: i64,
        default: i64,
    },
    Boolean {
        default: bool,
    },
    /// Choices by value; `default` is a value.
    Menu {
        items: Vec<(i64, String)>,
        default: i64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    pub id: u32,
    pub name: String,
    pub kind: Kind,
    pub value: i64,
    /// Greyed out by the driver (an automatic mode owns it).
    pub inactive: bool,
    pub read_only: bool,
}

impl Control {
    pub fn default_value(&self) -> i64 {
        match &self.kind {
            Kind::Integer { default, .. } => *default,
            Kind::Boolean { default } => i64::from(*default),
            Kind::Menu { default, .. } => *default,
        }
    }
}

const CTRL_TYPE_INTEGER: u32 = 1;
const CTRL_TYPE_BOOLEAN: u32 = 2;
const CTRL_TYPE_MENU: u32 = 3;
const CTRL_TYPE_INTEGER_MENU: u32 = 9;
const FLAG_DISABLED: u32 = 0x0001;
const FLAG_READ_ONLY: u32 = 0x0004;
const FLAG_INACTIVE: u32 = 0x0010;
const FLAG_NEXT_CTRL: u32 = 0x8000_0000;

#[repr(C)]
struct QueryExtCtrl {
    id: u32,
    type_: u32,
    name: [u8; 32],
    minimum: i64,
    maximum: i64,
    step: u64,
    default_value: i64,
    flags: u32,
    elem_size: u32,
    elems: u32,
    nr_of_dims: u32,
    dims: [u32; 4],
    reserved: [u32; 32],
}

#[repr(C)]
struct V4l2Control {
    id: u32,
    value: i32,
}

#[repr(C, packed)]
struct QueryMenu {
    id: u32,
    index: u32,
    /// `name` for menus, an `i64` value for integer menus.
    name: [u8; 32],
    reserved: u32,
}

#[repr(C)]
struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

const fn ioctl(dir: libc::c_ulong, nr: libc::c_ulong, size: usize) -> libc::c_ulong {
    (dir << 30) | ((size as libc::c_ulong) << 16) | ((b'V' as libc::c_ulong) << 8) | nr
}
const IOR: libc::c_ulong = 2;
const IOWR: libc::c_ulong = 3;
const VIDIOC_QUERYCAP: libc::c_ulong = ioctl(IOR, 0, std::mem::size_of::<Capability>());
const VIDIOC_G_CTRL: libc::c_ulong = ioctl(IOWR, 27, std::mem::size_of::<V4l2Control>());
const VIDIOC_S_CTRL: libc::c_ulong = ioctl(IOWR, 28, std::mem::size_of::<V4l2Control>());
const VIDIOC_QUERYMENU: libc::c_ulong = ioctl(IOWR, 37, std::mem::size_of::<QueryMenu>());
const VIDIOC_QUERY_EXT_CTRL: libc::c_ulong = ioctl(IOWR, 103, std::mem::size_of::<QueryExtCtrl>());

fn open(device: &Path) -> Result<File, String> {
    File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(device)
        .map_err(|e| format!("{}: {e}", device.display()))
}

/// One ioctl with a struct argument; the OS error on failure.
///
/// SAFETY (for callers): `arg` must be the struct the request expects, and
/// the driver only reads or fills it in place.
unsafe fn call<T>(fd: &File, request: libc::c_ulong, arg: &mut T) -> Result<(), std::io::Error> {
    // SAFETY: as stated above; the fd is open for the call's duration.
    if unsafe { libc::ioctl(fd.as_raw_fd(), request, arg as *mut T) } < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn query_capability(device: &Path) -> Option<Capability> {
    let fd = open(device).ok()?;
    // SAFETY: Capability is what VIDIOC_QUERYCAP fills.
    let mut cap: Capability = unsafe { std::mem::zeroed() };
    unsafe { call(&fd, VIDIOC_QUERYCAP, &mut cap) }.ok()?;
    Some(cap)
}

/// The device's driver name, e.g. "uvcvideo" or "v4l2 loopback".
pub fn driver(device: &Path) -> Option<String> {
    Some(c_string(&query_capability(device)?.driver))
}

/// V4L2_CAP_IO_MC: the node streams only once the media controller has
/// routed a pipeline to it.
const CAP_IO_MC: u32 = 0x2000_0000;

/// Whether the node is a fragment of a complex camera (a raw ISP output,
/// as the Qualcomm and Intel IPU drivers expose by the dozen) rather than a
/// camera in itself. libcamera drives those and publishes the result as a
/// camera of its own; plain webcams and loopback devices never set the flag.
pub fn needs_media_controller(device: &Path) -> bool {
    query_capability(device).is_some_and(|cap| cap.device_caps & CAP_IO_MC != 0)
}

/// The adjustable controls, in the driver's order. Loopback devices'
/// housekeeping controls are left out unless `GAZE_ALL_CONTROLS` is set.
pub fn list(device: &Path) -> Vec<Control> {
    let Ok(fd) = open(device) else {
        return Vec::new();
    };
    if std::env::var_os("GAZE_ALL_CONTROLS").is_none()
        && driver(device).is_some_and(|d| d.contains("loopback"))
    {
        return Vec::new();
    }
    let mut controls = Vec::new();
    let mut next = FLAG_NEXT_CTRL;
    loop {
        // SAFETY: QueryExtCtrl is what VIDIOC_QUERY_EXT_CTRL takes and fills.
        let mut q: QueryExtCtrl = unsafe { std::mem::zeroed() };
        q.id = next;
        if unsafe { call(&fd, VIDIOC_QUERY_EXT_CTRL, &mut q) }.is_err() {
            break;
        }
        next = q.id | FLAG_NEXT_CTRL;
        if q.flags & FLAG_DISABLED != 0 || q.elems > 1 || q.nr_of_dims > 0 {
            continue;
        }
        let kind = match q.type_ {
            CTRL_TYPE_INTEGER => Kind::Integer {
                min: q.minimum,
                max: q.maximum,
                step: (q.step as i64).max(1),
                default: q.default_value,
            },
            CTRL_TYPE_BOOLEAN => Kind::Boolean {
                default: q.default_value != 0,
            },
            CTRL_TYPE_MENU | CTRL_TYPE_INTEGER_MENU => {
                let mut items = Vec::new();
                for index in q.minimum..=q.maximum {
                    // SAFETY: QueryMenu is what VIDIOC_QUERYMENU takes and fills.
                    let mut m: QueryMenu = unsafe { std::mem::zeroed() };
                    m.id = q.id;
                    m.index = index as u32;
                    if unsafe { call(&fd, VIDIOC_QUERYMENU, &mut m) }.is_err() {
                        continue; // a hole in the menu
                    }
                    let name = if q.type_ == CTRL_TYPE_MENU {
                        c_string(&m.name)
                    } else {
                        i64::from_ne_bytes(m.name[..8].try_into().unwrap_or([0; 8])).to_string()
                    };
                    items.push((index, name));
                }
                if items.is_empty() {
                    continue;
                }
                Kind::Menu {
                    items,
                    default: q.default_value,
                }
            }
            _ => continue,
        };
        // SAFETY: V4l2Control is what VIDIOC_G_CTRL takes and fills.
        let mut c = V4l2Control { id: q.id, value: 0 };
        let value = match unsafe { call(&fd, VIDIOC_G_CTRL, &mut c) } {
            Ok(()) => c.value as i64,
            Err(_) => q.default_value,
        };
        controls.push(Control {
            id: q.id,
            name: c_string(&q.name),
            kind,
            value,
            inactive: q.flags & FLAG_INACTIVE != 0,
            read_only: q.flags & FLAG_READ_ONLY != 0,
        });
    }
    controls
}

pub fn set(device: &Path, id: u32, value: i64) -> Result<(), String> {
    let fd = open(device)?;
    let mut c = V4l2Control {
        id,
        value: value as i32,
    };
    // SAFETY: V4l2Control is what VIDIOC_S_CTRL takes.
    unsafe { call(&fd, VIDIOC_S_CTRL, &mut c) }.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_the_kernel() {
        assert_eq!(std::mem::size_of::<QueryExtCtrl>(), 232);
        assert_eq!(std::mem::size_of::<V4l2Control>(), 8);
        assert_eq!(std::mem::size_of::<QueryMenu>(), 44);
        assert_eq!(std::mem::size_of::<Capability>(), 104);
        // _IOWR('V', 27, struct v4l2_control) as the kernel computes it.
        assert_eq!(VIDIOC_G_CTRL, 0xc008_561b);
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
    }

    /// On a machine with a v4l2loopback device: set its `timeout` control
    /// and read it back. `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn loopback_round_trip() {
        let device = (0..64)
            .map(|n| std::path::PathBuf::from(format!("/dev/video{n}")))
            .find(|p| driver(p).is_some_and(|d| d.contains("loopback")))
            .expect("a v4l2loopback device");
        std::env::set_var("GAZE_ALL_CONTROLS", "1");
        let controls = list(&device);
        let timeout = controls
            .iter()
            .find(|c| c.name == "timeout")
            .expect("the loopback timeout control");
        let was = timeout.value;
        set(&device, timeout.id, 7).unwrap();
        let now = list(&device)
            .into_iter()
            .find(|c| c.id == timeout.id)
            .unwrap()
            .value;
        set(&device, timeout.id, was).unwrap();
        assert_eq!(now, 7);
    }

    #[test]
    fn nothing_from_a_missing_device() {
        assert!(list(Path::new("/dev/video-none")).is_empty());
        assert!(set(Path::new("/dev/video-none"), 1, 1).is_err());
    }
}
