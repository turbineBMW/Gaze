//! Cameras and the GStreamer pipeline behind the viewfinder.
//!
//! ```text
//! source ! capsfilter (raw, system memory, <= 1080p) ! effect ! background ! tee
//!   tee. ! queue (leaky) ! gtk4paintablesink                 viewfinder + photos
//!   tee. ! valve ! queue ! videorate ! videoconvert ! videoscale ! capsfilter ! appsink (RGB)
//!                                                              analysis taps
//!   tee. ! [recording bin: H.264 + AAC -> MP4]              added while recording
//!   tee. ! [output bin: queue ! videoconvert ! v4l2sink]      added while the virtual camera is on
//! ```
//!
//! The analysis branch is one CPU path shared by everything that wants to
//! look at frames (colour statistics, code scanning, text recognition): each
//! consumer registers a [`Tap`] saying how large and how often it needs
//! frames, and the branch is scaled and rate-limited to the most demanding
//! one. The valve keeps it empty while nobody is tapping.
//!
//! `effect` is an `identity`, or a GL bin (`glupload ! gleffects !
//! gldownload`) swapped in under a blocking pad probe, so photos, recordings
//! and taps all see the effect. `background` is another identity, or a bin
//! that bridges frames through the matting threads (appsink -> blur and
//! composite -> appsrc) for background blur.
//!
//! V4L2 cameras are opened directly with `v4l2src`, the way Chromium opens
//! them; anything else (libcamera) through the PipeWire device's own element.
//! The caps stay in system memory so every branch sees the same frames and
//! `videoconvert` does the YUV -> RGB conversion for photos and statistics,
//! honouring the stream's colorimetry (the hardware-ISP relay sends
//! limited-range video; decoding it as full range is what makes Snapshot's
//! picture grey).

use gst::prelude::*;
use gtk::{gdk, gio, glib};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct CameraInfo {
    pub name: String,
    source: Source,
    /// The raw formats the camera offers, largest and fastest first.
    pub formats: Vec<Format>,
}

impl CameraInfo {
    /// The V4L2 node, for controls; `None` for other kinds of source.
    pub fn device_path(&self) -> Option<PathBuf> {
        match &self.source {
            Source::V4l2(path) => Some(PathBuf::from(path)),
            Source::Device(_) => None,
        }
    }
}

/// A size and frame rate a camera can deliver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Format {
    pub width: i32,
    pub height: i32,
    pub fps: Option<gst::Fraction>,
}

impl Format {
    /// A stable id for menus and the settings file: `1920x1080@30/1`.
    pub fn id(&self) -> String {
        match self.fps {
            Some(fps) => format!(
                "{}x{}@{}/{}",
                self.width,
                self.height,
                fps.numer(),
                fps.denom()
            ),
            None => format!("{}x{}", self.width, self.height),
        }
    }

    pub fn parse(id: &str) -> Option<Format> {
        let (size, fps) = match id.split_once('@') {
            Some((size, fps)) => (size, Some(fps)),
            None => (id, None),
        };
        let (w, h) = size.split_once('x')?;
        let fps = match fps {
            Some(fps) => {
                let (n, d) = fps.split_once('/').unwrap_or((fps, "1"));
                Some(gst::Fraction::new(n.parse().ok()?, d.parse().ok()?))
            }
            None => None,
        };
        Some(Format {
            width: w.parse().ok()?,
            height: h.parse().ok()?,
            fps,
        })
    }

    /// For people: `1920 × 1080 · 30 fps`.
    pub fn label(&self) -> String {
        let size = format!("{} × {}", self.width, self.height);
        match self.fps {
            Some(fps) if fps.denom() != 0 => {
                let rate = fps.numer() as f64 / fps.denom() as f64;
                if (rate - rate.round()).abs() < 0.01 {
                    format!("{size} · {} fps", rate.round() as i64)
                } else {
                    format!("{size} · {rate:.2} fps")
                }
            }
            _ => size,
        }
    }

    fn pixels(&self) -> i64 {
        self.width as i64 * self.height as i64
    }

    fn rate(&self) -> f64 {
        self.fps
            .filter(|f| f.denom() != 0)
            .map_or(0.0, |f| f.numer() as f64 / f.denom() as f64)
    }
}

/// The largest size the pipeline asks for by default.
const DEFAULT_MAX: (i32, i32) = (1920, 1080);

/// What to ask a camera for when no format is chosen: anything up to 1080p
/// and the driver's pick. A camera that offers nothing that small (an ISP
/// node that only speaks 4K, say) is asked for its smallest size instead,
/// or negotiation fails and the picture stays black.
fn default_caps(formats: &[Format]) -> gst::Caps {
    let (max_w, max_h) = DEFAULT_MAX;
    let fits = |f: &Format| f.width <= max_w && f.height <= max_h;
    if formats.is_empty() || formats.iter().any(fits) {
        return gst::Caps::builder("video/x-raw")
            .field("width", gst::IntRange::new(1, max_w))
            .field("height", gst::IntRange::new(1, max_h))
            .build();
    }
    // Sorted largest first, so the smallest is at the end.
    let smallest = formats.last().unwrap();
    gst::Caps::builder("video/x-raw")
        .field("width", smallest.width)
        .field("height", smallest.height)
        .build()
}

/// Whether a pixel aspect ratio is one no camera has. `pipewiresrc` (1.6.10)
/// hands out 1/2147483647 when the node advertises none, and `videoscale`
/// then fails with "integer overflow" in the analysis branch.
fn absurd_par(par: gst::Fraction) -> bool {
    let (n, d) = (par.numer() as i64, par.denom() as i64);
    n <= 0 || d <= 0 || n * 16 < d || d * 16 < n
}

/// Rewrite the source's caps to square pixels when its pixel aspect ratio
/// is absurd, so the effects, thumbnails, stats and recordings all see 1/1.
fn square_pixels_if_absurd(pad: &gst::Pad) {
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, |_, info| {
        let Some(gst::PadProbeData::Event(event)) = &info.data else {
            return gst::PadProbeReturn::Ok;
        };
        let gst::EventView::Caps(c) = event.view() else {
            return gst::PadProbeReturn::Ok;
        };
        let caps = c.caps();
        let bad = caps.iter().any(|s| {
            s.get::<gst::Fraction>("pixel-aspect-ratio")
                .is_ok_and(absurd_par)
        });
        if !bad {
            return gst::PadProbeReturn::Ok;
        }
        let mut fixed = caps.to_owned();
        for s in fixed.make_mut().iter_mut() {
            if s.get::<gst::Fraction>("pixel-aspect-ratio")
                .is_ok_and(absurd_par)
            {
                s.set("pixel-aspect-ratio", gst::Fraction::new(1, 1));
            }
        }
        log::info!("camera caps had an absurd pixel-aspect-ratio; using 1/1");
        info.data = Some(gst::PadProbeData::Event(gst::event::Caps::new(&fixed)));
        gst::PadProbeReturn::Ok
    });
}

/// The fixed raw formats in a device's caps, deduplicated, largest and
/// fastest first. Frame-rate lists are expanded; ranges contribute their
/// top.
fn formats_from_caps(caps: &gst::Caps) -> Vec<Format> {
    let mut formats: Vec<Format> = Vec::new();
    for structure in caps.iter() {
        if structure.name() != "video/x-raw" {
            continue;
        }
        let (Ok(width), Ok(height)) = (
            structure.get::<i32>("width"),
            structure.get::<i32>("height"),
        ) else {
            continue;
        };
        let mut rates: Vec<Option<gst::Fraction>> = Vec::new();
        if let Ok(fps) = structure.get::<gst::Fraction>("framerate") {
            rates.push(Some(fps));
        } else if let Ok(list) = structure.get::<gst::List>("framerate") {
            rates.extend(
                list.iter()
                    .filter_map(|v| v.get::<gst::Fraction>().ok().map(Some)),
            );
        } else if let Ok(range) = structure.get::<gst::FractionRange>("framerate") {
            rates.push(Some(range.max()));
        } else {
            rates.push(None);
        }
        for fps in rates {
            let format = Format { width, height, fps };
            if !formats.contains(&format) {
                formats.push(format);
            }
        }
    }
    formats.sort_by(|a, b| {
        b.pixels().cmp(&a.pixels()).then(
            b.rate()
                .partial_cmp(&a.rate())
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    formats
}

enum Source {
    V4l2(String),
    Device(gst::Device),
}

/// The cameras GStreamer's device monitor reports, V4L2 devices first, one
/// entry per device node.
pub fn list() -> Vec<CameraInfo> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Source"), None);
    if let Err(error) = monitor.start() {
        log::warn!("device monitor: {error}");
        return Vec::new();
    }
    let devices = monitor.devices();
    monitor.stop();

    let mut v4l2 = Vec::new();
    let mut other = Vec::new();
    let mut seen = Vec::new();
    for device in devices {
        let name = device.display_name().to_string();
        let props = device.properties();
        let api = props
            .as_ref()
            .and_then(|p| p.get::<String>("device.api").ok())
            .unwrap_or_default();
        let path = props.as_ref().and_then(|p| {
            p.get::<String>("api.v4l2.path")
                .or_else(|_| p.get::<String>("device.path"))
                .ok()
        });
        // Gaze's own virtual camera is not a camera for Gaze.
        if name == crate::vcam::LABEL {
            continue;
        }
        let formats = device
            .caps()
            .map(|c| formats_from_caps(&c))
            .unwrap_or_default();
        match (api.as_str(), path) {
            ("v4l2", Some(path)) => {
                // Raw ISP outputs are not cameras; libcamera's node for the
                // sensor is listed below.
                if crate::controls::needs_media_controller(Path::new(&path)) {
                    continue;
                }
                if !seen.contains(&path) {
                    seen.push(path.clone());
                    v4l2.push(CameraInfo {
                        name,
                        source: Source::V4l2(path),
                        formats,
                    });
                }
            }
            _ => other.push(CameraInfo {
                name,
                source: Source::Device(device),
                formats,
            }),
        }
    }
    v4l2.extend(other);
    v4l2
}

/// A microphone, as PipeWire/PulseAudio names it.
pub struct MicInfo {
    pub name: String,
    /// What `pulsesrc`'s `device` takes.
    pub device: String,
}

/// The microphones the device monitor reports.
pub fn mics() -> Vec<MicInfo> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Audio/Source"), None);
    if let Err(error) = monitor.start() {
        log::warn!("device monitor (audio): {error}");
        return Vec::new();
    }
    let devices = monitor.devices();
    monitor.stop();
    let mut mics = Vec::new();
    for device in devices {
        let Some(props) = device.properties() else {
            continue;
        };
        let Ok(node) = props.get::<String>("node.name") else {
            continue;
        };
        // Monitors of outputs are not microphones.
        if node.ends_with(".monitor") || mics.iter().any(|m: &MicInfo| m.device == node) {
            continue;
        }
        mics.push(MicInfo {
            name: device.display_name().to_string(),
            device: node,
        });
    }
    mics
}

/// What the bus watch tells the window.
pub enum Event {
    Error(String),
    RecordingSaved(PathBuf),
    RecordingFailed(String),
    /// The microphone's level while recording, 0 to 1.
    AudioLevel(f64),
    /// The virtual camera branch failed and was removed.
    OutputFailed(String),
}

/// A frame from the analysis branch: packed RGB, 3 bytes a pixel, `stride`
/// bytes a row.
#[derive(Clone)]
pub struct Frame {
    pub rgb: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

/// What a consumer wants from the analysis branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tap {
    /// Scale frames down to at most this many pixels wide; `None` for the
    /// camera's own size.
    pub max_width: Option<u32>,
    /// Frames a second, at most.
    pub fps: u32,
}

pub type TapId = u64;

/// Runs on the streaming thread with each frame; must return quickly and
/// must not touch the [`Camera`].
type Handler = Box<dyn Fn(&Frame) + Send + 'static>;

#[derive(Default)]
struct Taps {
    next_id: TapId,
    list: Vec<(TapId, Tap, Handler)>,
}

impl Taps {
    /// The one tap that satisfies all of them.
    fn union(&self) -> Option<Tap> {
        self.list.iter().map(|(_, tap, _)| *tap).reduce(|a, b| Tap {
            max_width: match (a.max_width, b.max_width) {
                (Some(x), Some(y)) => Some(x.max(y)),
                _ => None,
            },
            fps: a.fps.max(b.fps),
        })
    }
}

/// Keeps the newest frame for a consumer that polls from the main loop.
#[derive(Clone, Default)]
pub struct LatestFrame(Arc<Mutex<Option<Frame>>>);

impl LatestFrame {
    pub fn store(&self, frame: &Frame) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame.clone());
    }

    pub fn take(&self) -> Option<Frame> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// A Photo Booth-style effect, done by `gleffects`.
#[derive(Debug, PartialEq, Eq)]
pub struct Effect {
    pub id: &'static str,
    pub name: &'static str,
    /// Distortions warp the picture; the rest recolour it.
    pub distorts: bool,
    gl: &'static str,
}

const fn effect(id: &'static str, name: &'static str, distorts: bool) -> Effect {
    Effect {
        id,
        name,
        distorts,
        gl: id,
    }
}

pub const EFFECTS: &[Effect] = &[
    effect("mirror", "Mirror", true),
    effect("squeeze", "Squeeze", true),
    effect("stretch", "Stretch", true),
    effect("tunnel", "Light Tunnel", true),
    effect("fisheye", "Fish Eye", true),
    effect("twirl", "Twirl", true),
    effect("bulge", "Bulge", true),
    effect("square", "Square", true),
    effect("sepia", "Sepia", false),
    effect("xpro", "X-Pro", false),
    effect("heat", "Thermal", false),
    effect("xray", "X-Ray", false),
    effect("glow", "Glow", false),
    effect("blur", "Blur", false),
    effect("sobel", "Edges", false),
    effect("sin", "Red Only", false),
];

pub fn effect_by_id(id: &str) -> Option<&'static Effect> {
    EFFECTS.iter().find(|e| e.id == id)
}

pub struct Camera {
    pipeline: gst::Pipeline,
    paintable: gdk::Paintable,
    preview: gst::Element,
    /// What sits between the capsfilter and the tee, and which effect it is.
    effect: std::cell::RefCell<(gst::Element, Option<&'static str>)>,
    /// The background slot after it, and the backdrop its matting threads
    /// composite over while it is on.
    background: std::cell::RefCell<(gst::Element, Option<Arc<Mutex<crate::matte::Backdrop>>>)>,
    tee: gst::Element,
    tap_valve: gst::Element,
    tap_rate: gst::Element,
    tap_filter: gst::Element,
    taps: Arc<Mutex<Taps>>,
    recording: std::cell::RefCell<Option<Recording>>,
    output: std::cell::RefCell<Option<Recording>>,
    /// Pause bookkeeping shared with the recorder's pad probes.
    pauses: Arc<Mutex<Pauses>>,
    _bus_watch: gst::bus::BusWatchGuard,
}

/// A bin hanging off the tee: the recorder or the virtual camera output.
struct Recording {
    bin: gst::Bin,
    tee_pad: gst::Pad,
}

/// Where a recording is paused, in pipeline running time (what buffers
/// are stamped with). Buffers stamped after `pause_at` are dropped; once
/// resumed, buffers from before `resume_at` are dropped too and later ones
/// are shifted back by everything paused so far.
#[derive(Default)]
struct Pauses {
    pause_at: Option<gst::ClockTime>,
    resume_at: gst::ClockTime,
    shift: u64,
    /// Time-lapse: keep one frame in `lapse` and squeeze the stamps by as
    /// much (1 or 0: off). Video only; a time-lapse has no sound.
    lapse: u64,
    seen: u64,
    lapse_start: Option<gst::ClockTime>,
}

impl Pauses {
    /// What to do with a buffer stamped `pts`: `None` drops it, otherwise
    /// the stamp to give it.
    fn apply(&mut self, pts: gst::ClockTime) -> Option<gst::ClockTime> {
        if self.pause_at.is_some_and(|p| pts >= p) || pts < self.resume_at {
            return None;
        }
        let pts = pts.nseconds().saturating_sub(self.shift);
        if self.lapse <= 1 {
            return Some(gst::ClockTime::from_nseconds(pts));
        }
        let keep = self.seen.is_multiple_of(self.lapse);
        self.seen += 1;
        if !keep {
            return None;
        }
        let start = *self
            .lapse_start
            .get_or_insert(gst::ClockTime::from_nseconds(pts));
        Some(gst::ClockTime::from_nseconds(
            start.nseconds() + pts.saturating_sub(start.nseconds()) / self.lapse,
        ))
    }
}

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| format!("GStreamer element “{factory}” is missing"))
}

impl Camera {
    /// Build the pipeline for `info` and start it. `on_event` runs on the
    /// main loop.
    pub fn open(
        info: &CameraInfo,
        format: Option<&Format>,
        on_event: impl Fn(Event) + 'static,
    ) -> Result<Self, String> {
        let pipeline = gst::Pipeline::with_name("gaze");
        let source = match &info.source {
            Source::V4l2(path) => {
                let src = make("v4l2src")?;
                src.set_property("device", path);
                src
            }
            Source::Device(device) => device
                .create_element(Some("source"))
                .map_err(|e| format!("cannot open {}: {e}", info.name))?,
        };
        // A chosen format exactly; otherwise anything up to 1080p and the
        // driver's pick.
        let caps = match format {
            Some(f) => {
                let mut caps = gst::Caps::builder("video/x-raw")
                    .field("width", f.width)
                    .field("height", f.height);
                if let Some(fps) = f.fps {
                    caps = caps.field("framerate", fps);
                }
                caps.build()
            }
            None => default_caps(&info.formats),
        };
        if let Some(pad) = source.static_pad("src") {
            square_pixels_if_absurd(&pad);
        }
        // pipewireclock only ticks while the producer can dequeue a buffer.
        // Once the viewfinder, the analysis branch and GTK hold the whole
        // pool (8 buffers on the SP11 ISP node), the sinks wait on a clock
        // that waits on them: the picture freezes. The system clock keeps
        // running, so the sinks render and give the buffers back.
        if source.find_property("provide-clock").is_some() {
            source.set_property("provide-clock", false);
        }
        let filter = make("capsfilter")?;
        filter.set_property("caps", &caps);
        let effect = make("identity")?;
        let background = make("identity")?;
        let tee = make("tee")?;

        let preview_queue = make("queue")?;
        preview_queue.set_property("max-size-buffers", 2u32);
        preview_queue.set_property_from_str("leaky", "downstream");
        let preview = make("gtk4paintablesink")?;
        preview.set_property("enable-last-sample", true);

        // Analysis branch, shared by every tap.
        let tap_valve = make("valve")?;
        tap_valve.set_property("drop", true);
        let tap_queue = make("queue")?;
        tap_queue.set_property("max-size-buffers", 1u32);
        tap_queue.set_property_from_str("leaky", "downstream");
        let tap_rate = make("videorate")?;
        tap_rate.set_property("drop-only", true);
        tap_rate.set_property("max-rate", 4i32);
        let convert = make("videoconvert")?;
        let scale = make("videoscale")?;
        let tap_filter = make("capsfilter")?;
        tap_filter.set_property("caps", tap_caps(None));
        let taps: Arc<Mutex<Taps>> = Arc::default();
        let tap_sink = gst_app::AppSink::builder()
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGB")
                    .build(),
            )
            .max_buffers(1)
            .drop(true)
            .sync(false)
            .callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample({
                        let taps = taps.clone();
                        move |sink| {
                            let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                            if let Some(frame) = frame_from_sample(&sample) {
                                let taps = taps.lock().unwrap_or_else(|e| e.into_inner());
                                for (_, _, handler) in &taps.list {
                                    handler(&frame);
                                }
                            }
                            Ok(gst::FlowSuccess::Ok)
                        }
                    })
                    .build(),
            )
            .build();
        // The valve keeps this branch empty until someone taps it; a sink
        // that waited for a first frame there would hold the whole pipeline
        // in PAUSED.
        tap_sink.set_async(false);

        let elements = [
            &source,
            &filter,
            &effect,
            &background,
            &tee,
            &preview_queue,
            &preview,
            &tap_valve,
            &tap_queue,
            &tap_rate,
            &convert,
            &scale,
            &tap_filter,
            tap_sink.upcast_ref(),
        ];
        pipeline.add_many(elements).map_err(|e| e.to_string())?;
        gst::Element::link_many([&source, &filter, &effect, &background, &tee])
            .map_err(|e| e.to_string())?;
        gst::Element::link_many([&tee, &preview_queue, &preview]).map_err(|e| e.to_string())?;
        gst::Element::link_many([
            &tee,
            &tap_valve,
            &tap_queue,
            &tap_rate,
            &convert,
            &scale,
            &tap_filter,
            tap_sink.upcast_ref(),
        ])
        .map_err(|e| e.to_string())?;

        let bus = pipeline.bus().expect("a pipeline has a bus");
        let weak = pipeline.downgrade();
        let bus_watch = bus
            .add_watch_local(move |_, message| {
                use gst::MessageView;
                match message.view() {
                    MessageView::Error(err) => {
                        let text = err.error().to_string();
                        log::warn!("{text} ({:?})", err.debug());
                        // An error inside the recording or output bin ends
                        // that branch, not the viewfinder.
                        let element = message
                            .src()
                            .and_then(|s| s.downcast_ref::<gst::Element>().cloned());
                        if element.as_ref().is_some_and(|e| in_bin(e, "recording")) {
                            on_event(Event::RecordingFailed(text));
                        } else if element.as_ref().is_some_and(|e| in_bin(e, "output")) {
                            on_event(Event::OutputFailed(text));
                        } else {
                            on_event(Event::Error(text));
                        }
                    }
                    MessageView::Element(element) => {
                        // The recording bin forwards its sink's EOS: the file
                        // is complete.
                        let Some(s) = element.structure() else {
                            return glib::ControlFlow::Continue;
                        };
                        if s.name() == "level" {
                            if let Some(db) = level_db(s) {
                                on_event(Event::AudioLevel(((db + 60.0) / 60.0).clamp(0.0, 1.0)));
                            }
                        }
                        if s.name() == "GstBinForwarded" {
                            if let Ok(inner) = s.get::<gst::Message>("message") {
                                if let MessageView::Eos(_) = inner.view() {
                                    if let Some(pipeline) = weak.upgrade() {
                                        if let Some(path) = finish_recording(&pipeline) {
                                            on_event(Event::RecordingSaved(path));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
                glib::ControlFlow::Continue
            })
            .map_err(|e| e.to_string())?;

        // The sink has to hand out its paintable before it starts; otherwise
        // it makes one of its own (and a window for it) on the way to READY.
        let paintable = preview.property::<gdk::Paintable>("paintable");
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| format!("{} could not be started", info.name))?;

        Ok(Self {
            pipeline,
            paintable,
            preview,
            effect: std::cell::RefCell::new((effect, None)),
            background: std::cell::RefCell::new((background, None)),
            tee,
            tap_valve,
            tap_rate,
            tap_filter,
            taps,
            recording: std::cell::RefCell::new(None),
            output: std::cell::RefCell::new(None),
            pauses: Arc::default(),
            _bus_watch: bus_watch,
        })
    }

    pub fn paintable(&self) -> gdk::Paintable {
        self.paintable.clone()
    }

    /// Put `effect` (or none) between the camera and the tee. The swap
    /// happens on the streaming thread with the flow blocked, so no frame
    /// is lost to the old element.
    pub fn set_effect(&self, effect: Option<&'static Effect>) -> Result<(), String> {
        let wanted = effect.map(|e| e.id);
        if self.effect.borrow().1 == wanted {
            return Ok(());
        }
        let new = match effect {
            None => make("identity")?,
            Some(effect) => {
                if gst::ElementFactory::find("gleffects").is_none() {
                    return Err("GStreamer's OpenGL effects (gleffects) are missing".into());
                }
                gst::parse::bin_from_description(
                    &format!(
                        "glupload ! glcolorconvert ! gleffects effect={} ! gldownload \
                         ! capsfilter caps=video/x-raw",
                        effect.gl
                    ),
                    true,
                )
                .map_err(|e| format!("cannot build the effect: {e}"))?
                .upcast()
            }
        };
        let (old, _) = self.effect.replace((new.clone(), wanted));
        self.swap_slot(old, new)
    }

    /// Replace `old` (an element between two others) with `new`, on the
    /// streaming thread with the flow blocked, so no frame is lost.
    fn swap_slot(&self, old: gst::Element, new: gst::Element) -> Result<(), String> {
        let upstream_pad = old
            .static_pad("sink")
            .and_then(|p| p.peer())
            .ok_or("slot has no upstream")?;
        let pipeline = self.pipeline.clone();
        upstream_pad
            .clone()
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |_, _| {
                let Some(upstream) = upstream_pad.parent_element() else {
                    return gst::PadProbeReturn::Remove;
                };
                let Some(downstream) = old
                    .static_pad("src")
                    .and_then(|p| p.peer())
                    .and_then(|p| p.parent_element())
                else {
                    return gst::PadProbeReturn::Remove;
                };
                upstream.unlink(&old);
                old.unlink(&downstream);
                let _ = old.set_state(gst::State::Null);
                let _ = pipeline.remove(&old);
                if let Err(error) = pipeline
                    .add(&new)
                    .map_err(|e| e.to_string())
                    .and_then(|_| upstream.link(&new).map_err(|e| e.to_string()))
                    .and_then(|_| new.link(&downstream).map_err(|e| e.to_string()))
                    .and_then(|_| new.sync_state_with_parent().map_err(|e| e.to_string()))
                {
                    log::warn!("slot swap: {error}");
                }
                gst::PadProbeReturn::Remove
            });
        Ok(())
    }

    /// Put `backdrop` behind the person (the matting model must be in the
    /// cache), change it, or with `None` show the plain picture again.
    pub fn set_background(&self, backdrop: Option<crate::matte::Backdrop>) -> Result<(), String> {
        let current = self.background.borrow().1.clone();
        match (backdrop, current) {
            (None, None) => Ok(()),
            (Some(backdrop), Some(shared)) => {
                *shared.lock().unwrap_or_else(|e| e.into_inner()) = backdrop;
                Ok(())
            }
            (Some(backdrop), None) => {
                let shared = Arc::new(Mutex::new(backdrop));
                let new: gst::Element = background_bin(shared.clone())?.upcast();
                let (old, _) = self.background.replace((new.clone(), Some(shared)));
                self.swap_slot(old, new)
            }
            (None, Some(_)) => {
                let new = make("identity")?;
                let (old, _) = self.background.replace((new.clone(), None));
                self.swap_slot(old, new)
            }
        }
    }

    /// Start handing frames to `handler` (on the streaming thread). The
    /// analysis branch is opened, or widened and sped up, to suit.
    pub fn add_tap(&self, tap: Tap, handler: impl Fn(&Frame) + Send + 'static) -> TapId {
        let id = {
            let mut taps = self.taps.lock().unwrap_or_else(|e| e.into_inner());
            let id = taps.next_id;
            taps.next_id += 1;
            taps.list.push((id, tap, Box::new(handler)));
            id
        };
        self.configure_taps();
        id
    }

    pub fn remove_tap(&self, id: TapId) {
        self.taps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .list
            .retain(|(i, _, _)| *i != id);
        self.configure_taps();
    }

    /// Set the analysis branch to the union of the taps, or close it.
    fn configure_taps(&self) {
        let union = self.taps.lock().unwrap_or_else(|e| e.into_inner()).union();
        match union {
            Some(tap) => {
                self.tap_rate
                    .set_property("max-rate", tap.fps.max(1) as i32);
                self.tap_filter
                    .set_property("caps", tap_caps(tap.max_width));
                self.tap_valve.set_property("drop", false);
            }
            None => self.tap_valve.set_property("drop", true),
        }
    }

    /// Save the frame on screen as a JPEG, and make a small RGB copy of it
    /// for the gallery. Runs the conversion and encoding off the main loop.
    pub fn take_photo(
        &self,
        path: PathBuf,
    ) -> impl std::future::Future<Output = Result<(PathBuf, Option<Frame>), String>> + 'static {
        let sample = self.preview.property::<Option<gst::Sample>>("last-sample");
        async move {
            let sample = sample.ok_or("no frame yet")?;
            gio::spawn_blocking(move || {
                save_jpeg(&sample, &path)?;
                Ok((path, thumbnail(&sample, THUMBNAIL_WIDTH)))
            })
            .await
            .map_err(|_| "photo thread failed".to_string())?
        }
    }

    /// A small RGB copy of the frame on screen (for a recording's gallery
    /// entry).
    pub fn snapshot(&self) -> impl std::future::Future<Output = Option<Frame>> + 'static {
        let sample = self.preview.property::<Option<gst::Sample>>("last-sample");
        async move {
            let sample = sample?;
            gio::spawn_blocking(move || thumbnail(&sample, THUMBNAIL_WIDTH))
                .await
                .ok()
                .flatten()
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.borrow().is_some()
    }

    /// Start recording the camera and a microphone (`None`: the default) to
    /// `path` (H.264 from [`h264_encoder`], AAC, MP4). A `lapse` above
    /// 1 makes a time-lapse that many times faster, without sound.
    pub fn start_recording(
        &self,
        path: &Path,
        mic: Option<&str>,
        lapse: u32,
    ) -> Result<(), String> {
        if self.is_recording() {
            return Ok(());
        }
        let encoder = h264_encoder().ok_or(
            "no H.264 encoder: install gst-plugin-va (Intel/AMD GPUs) or gst-plugins-ugly (x264)",
        )?;
        let video = format!(
            "queue name=vq max-size-buffers=0 max-size-bytes=0 max-size-time=2000000000 \
               ! videoconvert ! {encoder} ! h264parse \
               ! mp4mux name=mux ! filesink name=out location=\"{}\"",
            path.display().to_string().replace('"', "\\\"")
        );
        let audio = "pulsesrc name=mic ! queue name=aq max-size-time=2000000000 ! audioconvert \
               ! level interval=100000000 ! audioresample \
               ! audio/x-raw,rate=48000,channels=2 ! avenc_aac bitrate=160000 ! aacparse ! mux.";
        let description = if lapse > 1 {
            video
        } else {
            format!("{video} {audio}")
        };
        let bin = gst::parse::bin_from_description(&description, true)
            .map_err(|e| format!("cannot build the recorder: {e}"))?;
        bin.set_property("message-forward", true);
        bin.set_property("name", "recording");
        if let (Some(mic), Some(device)) = (bin.by_name("mic"), mic) {
            mic.set_property("device", device);
        }

        // Pausing: the queues' probes drop or restamp buffers. Pad offsets
        // move the segment, not the stamps, so buffers carry pipeline
        // running time and Pauses works in that.
        *self.pauses.lock().unwrap_or_else(|e| e.into_inner()) = Pauses {
            lapse: lapse.max(1) as u64,
            ..Pauses::default()
        };
        for queue in ["vq", "aq"] {
            let Some(pad) = bin.by_name(queue).and_then(|q| q.static_pad("sink")) else {
                continue;
            };
            let pauses = self.pauses.clone();
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data else {
                    return gst::PadProbeReturn::Ok;
                };
                let Some(pts) = buffer.pts() else {
                    return gst::PadProbeReturn::Ok;
                };
                let decision = pauses.lock().unwrap_or_else(|e| e.into_inner()).apply(pts);
                match decision {
                    None => gst::PadProbeReturn::Drop,
                    Some(stamped) => {
                        if stamped != pts {
                            let b = buffer.make_mut();
                            b.set_pts(stamped);
                            b.set_dts(None);
                        }
                        gst::PadProbeReturn::Ok
                    }
                }
            });
        }

        // Both branches reach the muxer in pipeline running time; start the
        // file at zero. The anchor is the frame on screen, not the clock:
        // the next frame to arrive is stamped a little before "now" (capture
        // time plus the pipeline's latency), and a frame stamped before the
        // segment start is clipped by the encoder, taking the opening
        // keyframe with it.
        let shown = self
            .preview
            .property::<Option<gst::Sample>>("last-sample")
            .and_then(|s| s.buffer().and_then(|b| b.pts()));
        let start = shown
            .or_else(|| self.pipeline.current_running_time())
            .unwrap_or(gst::ClockTime::ZERO);
        let offset = -(start.nseconds() as i64);
        let sink_pad = bin.static_pad("sink").ok_or("recorder has no input")?;
        sink_pad.set_offset(offset);
        if let Some(mic) = bin.by_name("mic").and_then(|m| m.static_pad("src")) {
            mic.set_offset(offset);
        }

        self.pipeline.add(&bin).map_err(|e| e.to_string())?;
        bin.sync_state_with_parent().map_err(|e| e.to_string())?;
        let tee_pad = self
            .tee
            .request_pad_simple("src_%u")
            .ok_or("tee has no free pad")?;
        tee_pad
            .link(&sink_pad)
            .map_err(|e| format!("cannot attach the recorder: {e:?}"))?;
        self.recording.replace(Some(Recording { bin, tee_pad }));
        Ok(())
    }

    fn running_time(&self) -> Option<gst::ClockTime> {
        if !self.is_recording() {
            return None;
        }
        self.pipeline.current_running_time()
    }

    pub fn is_paused(&self) -> bool {
        self.pauses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pause_at
            .is_some()
    }

    /// Stop writing frames and sound until [`Camera::resume_recording`];
    /// the file carries on as if the pause never happened.
    pub fn pause_recording(&self) {
        let Some(now) = self.running_time() else {
            return;
        };
        let mut pauses = self.pauses.lock().unwrap_or_else(|e| e.into_inner());
        if pauses.pause_at.is_none() {
            pauses.pause_at = Some(now);
        }
    }

    pub fn resume_recording(&self) {
        let Some(now) = self.running_time() else {
            return;
        };
        let mut pauses = self.pauses.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(paused_at) = pauses.pause_at.take() {
            pauses.shift += now.nseconds().saturating_sub(paused_at.nseconds());
            pauses.resume_at = now;
        }
    }

    /// End the recording: detach it from the camera and send end-of-stream
    /// through both branches. The file is complete when the bus reports
    /// [`Event::RecordingSaved`].
    pub fn stop_recording(&self) {
        self.resume_recording();
        let recording = self.recording.borrow();
        let Some(rec) = recording.as_ref() else {
            return;
        };
        let bin = rec.bin.clone();
        let tee = self.tee.clone();
        let tee_pad = rec.tee_pad.clone();
        tee_pad.add_probe(gst::PadProbeType::IDLE, move |pad, _| {
            if let Some(peer) = pad.peer() {
                let _ = pad.unlink(&peer);
                peer.send_event(gst::event::Eos::new());
            }
            tee.release_request_pad(pad);
            gst::PadProbeReturn::Remove
        });
        if let Some(mic) = bin.by_name("mic") {
            mic.send_event(gst::event::Eos::new());
        }
    }

    /// Stop everything. A recording in progress is ended first and given a
    /// moment to finish its file.
    pub fn close(&self) {
        if self.is_recording() {
            self.stop_recording();
            if let Some(bus) = self.pipeline.bus() {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                while std::time::Instant::now() < deadline {
                    let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
                        continue;
                    };
                    if let gst::MessageView::Element(e) = msg.view() {
                        if e.structure().is_some_and(|s| s.name() == "GstBinForwarded") {
                            break;
                        }
                    }
                }
            }
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// The loudest channel's RMS from a `level` message, in dB.
fn level_db(s: &gst::StructureRef) -> Option<f64> {
    let values: Vec<f64> = if let Ok(array) = s.get::<glib::ValueArray>("rms") {
        array.iter().filter_map(|v| v.get::<f64>().ok()).collect()
    } else if let Ok(list) = s.get::<gst::List>("rms") {
        list.iter().filter_map(|v| v.get::<f64>().ok()).collect()
    } else {
        return None;
    };
    values.into_iter().reduce(f64::max)
}

fn in_bin(element: &gst::Element, name: &str) -> bool {
    let mut parent = element.parent();
    while let Some(p) = parent {
        if p.name() == name {
            return true;
        }
        parent = p.parent();
    }
    false
}

/// Take the finished recording bin out of the pipeline. Returns its file.
fn finish_recording(pipeline: &gst::Pipeline) -> Option<PathBuf> {
    let bin = pipeline.by_name("recording")?.downcast::<gst::Bin>().ok()?;
    let path = bin
        .by_name("out")
        .map(|sink| PathBuf::from(sink.property::<String>("location")));
    let _ = bin.set_state(gst::State::Null);
    let _ = pipeline.remove(&bin);
    path
}

impl Camera {
    pub fn is_output(&self) -> bool {
        self.output.borrow().is_some()
    }

    /// Feed the virtual camera at `device` from the tee (YUY2, no sync, so
    /// a slow reader never holds the viewfinder up).
    pub fn start_output(&self, device: &Path) -> Result<(), String> {
        if self.is_output() {
            return Ok(());
        }
        let description = format!(
            "queue max-size-buffers=2 leaky=downstream ! videoconvert \
               ! video/x-raw,format=YUY2 ! v4l2sink name=vsink sync=false device=\"{}\"",
            device.display().to_string().replace('"', "\\\"")
        );
        let bin = gst::parse::bin_from_description(&description, true)
            .map_err(|e| format!("cannot build the virtual camera output: {e}"))?;
        bin.set_property("name", "output");
        let sink_pad = bin.static_pad("sink").ok_or("output has no input")?;
        self.pipeline.add(&bin).map_err(|e| e.to_string())?;
        if let Err(error) = bin.sync_state_with_parent() {
            let _ = bin.set_state(gst::State::Null);
            let _ = self.pipeline.remove(&bin);
            return Err(format!("cannot open {}: {error}", device.display()));
        }
        let tee_pad = self
            .tee
            .request_pad_simple("src_%u")
            .ok_or("tee has no free pad")?;
        tee_pad
            .link(&sink_pad)
            .map_err(|e| format!("cannot attach the virtual camera: {e:?}"))?;
        self.output.replace(Some(Recording { bin, tee_pad }));
        Ok(())
    }

    /// Detach and drop the virtual camera branch.
    pub fn stop_output(&self) {
        let Some(out) = self.output.take() else {
            return;
        };
        let pipeline = self.pipeline.clone();
        let tee = self.tee.clone();
        let bin = out.bin.clone();
        out.tee_pad
            .add_probe(gst::PadProbeType::IDLE, move |pad, _| {
                if let Some(peer) = pad.peer() {
                    let _ = pad.unlink(&peer);
                }
                tee.release_request_pad(pad);
                let _ = bin.set_state(gst::State::Null);
                let _ = pipeline.remove(&bin);
                gst::PadProbeReturn::Remove
            });
    }
}

/// Hook for the window: forget the recording once its file is done.
impl Camera {
    pub fn recording_finished(&self) {
        self.recording.replace(None);
    }

    /// Drop a recording that failed: detach and discard its bin.
    pub fn abort_recording(&self) {
        let Some(rec) = self.recording.take() else {
            return;
        };
        if let Some(peer) = rec.tee_pad.peer() {
            let _ = rec.tee_pad.unlink(&peer);
        }
        self.tee.release_request_pad(&rec.tee_pad);
        let _ = rec.bin.set_state(gst::State::Null);
        let _ = self.pipeline.remove(&rec.bin);
    }
}

/// The background blur slot: frames leave the pipeline through an appsink
/// into the matting threads and come back through an appsrc, composited.
fn background_bin(backdrop: Arc<Mutex<crate::matte::Backdrop>>) -> Result<gst::Bin, String> {
    let bin = gst::Bin::new();
    let convert = make("videoconvert")?;
    let to_rgb = make("capsfilter")?;
    to_rgb.set_property(
        "caps",
        gst::Caps::builder("video/x-raw")
            .field("format", "RGB")
            .build(),
    );
    let sink = gst_app::AppSink::builder()
        .max_buffers(1)
        .drop(true)
        .sync(false)
        .build();
    sink.set_async(false);
    let src = gst_app::AppSrc::builder()
        .is_live(true)
        .format(gst::Format::Time)
        .do_timestamp(true)
        .build();
    let queue = make("queue")?;
    queue.set_property("max-size-buffers", 2u32);
    queue.set_property_from_str("leaky", "downstream");
    bin.add_many([
        &convert,
        &to_rgb,
        sink.upcast_ref(),
        src.upcast_ref(),
        &queue,
    ])
    .map_err(|e| e.to_string())?;
    gst::Element::link_many([&convert, &to_rgb, sink.upcast_ref()]).map_err(|e| e.to_string())?;
    src.link(&queue).map_err(|e| e.to_string())?;
    let ghost_sink = gst::GhostPad::with_target(&convert.static_pad("sink").ok_or("no sink")?)
        .map_err(|e| e.to_string())?;
    let ghost_src = gst::GhostPad::with_target(&queue.static_pad("src").ok_or("no src")?)
        .map_err(|e| e.to_string())?;
    bin.add_pad(&ghost_sink).map_err(|e| e.to_string())?;
    bin.add_pad(&ghost_src).map_err(|e| e.to_string())?;

    // Frames in: to the matting threads, remembering the frame rate so the
    // frames out can carry it.
    let rate: Arc<Mutex<gst::Fraction>> = Arc::new(Mutex::new(gst::Fraction::new(30, 1)));
    let out_caps: Arc<Mutex<Option<(usize, usize)>>> = Arc::default();
    let matting = Arc::new(crate::matte::Matting::start(backdrop, {
        let src = src.clone();
        let rate = rate.clone();
        move |frame: Frame| {
            let mut known = out_caps.lock().unwrap_or_else(|e| e.into_inner());
            if *known != Some((frame.width, frame.height)) {
                let fps = *rate.lock().unwrap_or_else(|e| e.into_inner());
                src.set_caps(Some(
                    &gst::Caps::builder("video/x-raw")
                        .field("format", "RGB")
                        .field("width", frame.width as i32)
                        .field("height", frame.height as i32)
                        .field("framerate", fps)
                        .build(),
                ));
                *known = Some((frame.width, frame.height));
            }
            drop(known);
            let mut buffer =
                gst::Buffer::with_size(frame.width * frame.height * 3).expect("a frame buffer");
            {
                let b = buffer.get_mut().expect("a fresh buffer");
                let mut map = b.map_writable().expect("a writable buffer");
                let row = frame.width * 3;
                for (src_row, dst) in frame
                    .rgb
                    .chunks(frame.stride)
                    .take(frame.height)
                    .zip(map.as_mut_slice().chunks_mut(row))
                {
                    dst.copy_from_slice(&src_row[..row]);
                }
            }
            let _ = src.push_buffer(buffer);
        }
    })?);
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if let Some(fps) = sample
                    .caps()
                    .and_then(|c| c.structure(0))
                    .and_then(|s| s.get::<gst::Fraction>("framerate").ok())
                {
                    if fps.numer() > 0 {
                        *rate.lock().unwrap_or_else(|e| e.into_inner()) = fps;
                    }
                }
                if let Some(frame) = frame_from_sample(&sample) {
                    matting.push(frame);
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    Ok(bin)
}

/// Caps for the analysis branch: RGB, square pixels, at most `max_width`
/// wide.
fn tap_caps(max_width: Option<u32>) -> gst::Caps {
    let mut caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGB")
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1));
    if let Some(width) = max_width {
        caps = caps.field("width", gst::IntRange::new(1, width.max(1) as i32));
    }
    caps.build()
}

pub(crate) fn frame_from_sample(sample: &gst::Sample) -> Option<Frame> {
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;
    Some(Frame {
        rgb: map.as_slice().to_vec(),
        width: info.width() as usize,
        height: info.height() as usize,
        stride: info.stride()[0] as usize,
    })
}

pub const THUMBNAIL_WIDTH: i32 = 320;

/// The sample scaled to `width` pixels wide, packed RGB.
fn thumbnail(sample: &gst::Sample, width: i32) -> Option<Frame> {
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    if info.width() == 0 {
        return None;
    }
    let height = ((width as i64 * info.height() as i64 / info.width() as i64) & !1).max(2);
    let small = gst_video::convert_sample(
        sample,
        &gst::Caps::builder("video/x-raw")
            .field("format", "RGB")
            .field("width", width)
            .field("height", height as i32)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build(),
        Some(gst::ClockTime::from_seconds(5)),
    )
    .ok()?;
    frame_from_sample(&small)
}

fn save_jpeg(sample: &gst::Sample, path: &Path) -> Result<(), String> {
    let rgb = gst_video::convert_sample(
        sample,
        &gst::Caps::builder("video/x-raw")
            .field("format", "RGB")
            .build(),
        Some(gst::ClockTime::from_seconds(5)),
    )
    .map_err(|e| format!("cannot convert the frame: {e}"))?;
    let info = gst_video::VideoInfo::from_caps(rgb.caps().ok_or("frame has no caps")?)
        .map_err(|e| e.to_string())?;
    let buffer = rgb.buffer().ok_or("frame has no data")?;
    let map = buffer.map_readable().map_err(|e| e.to_string())?;
    let bytes = glib::Bytes::from(map.as_slice());
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(
        &bytes,
        gtk::gdk_pixbuf::Colorspace::Rgb,
        false,
        8,
        info.width() as i32,
        info.height() as i32,
        info.stride()[0],
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    pixbuf
        .savev(path, "jpeg", &[("quality", "95")])
        .map_err(|e| format!("cannot save {}: {e}", path.display()))
}

/// H.264 encoders the recorder can use, best first, each with the settings
/// for about 8 Mbit/s: VA-API on Intel/AMD GPUs, a V4L2 hardware encoder on
/// ARM boards, then x264 and OpenH264 on the CPU (no VA-API driver exists for
/// some GPUs, Adreno among them, so gst-plugin-va registers nothing there).
const H264_ENCODERS: &[(&str, &str)] = &[
    ("vah264enc", "vah264enc bitrate=8000"),
    (
        "v4l2h264enc",
        "v4l2h264enc extra-controls=\"controls,video_bitrate=8000000\"",
    ),
    (
        "x264enc",
        // 4:2:0, or x264 takes 4:4:4 from RGB sources (High 4:4:4, which
        // browsers and phones do not play).
        "video/x-raw,format=I420 ! x264enc bitrate=8000 tune=zerolatency speed-preset=veryfast",
    ),
    ("openh264enc", "openh264enc bitrate=8000000"),
];

/// The first H.264 encoder GStreamer has, as a pipeline fragment.
fn h264_encoder() -> Option<&'static str> {
    H264_ENCODERS
        .iter()
        .find(|(factory, _)| gst::ElementFactory::find(factory).is_some())
        .map(|(_, fragment)| *fragment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_absurd_pixel_aspect_ratios_are_replaced() {
        let f = gst::Fraction::new;
        assert!(absurd_par(f(1, i32::MAX)));
        assert!(absurd_par(f(i32::MAX, 1)));
        assert!(absurd_par(f(0, 1)));
        assert!(!absurd_par(f(1, 1)));
        assert!(!absurd_par(f(10, 11)));
        assert!(!absurd_par(f(4, 3)));
    }

    fn fmt(width: i32, height: i32) -> Format {
        Format {
            width,
            height,
            fps: None,
        }
    }

    #[test]
    fn default_caps_keep_the_1080p_ceiling_when_the_camera_fits() {
        gst::init().unwrap();
        let caps = default_caps(&[fmt(3840, 2160), fmt(1920, 1080), fmt(640, 480)]);
        let s = caps.structure(0).unwrap();
        assert_eq!(s.get::<gst::IntRange<i32>>("width").unwrap().max(), 1920);
        assert_eq!(s.get::<gst::IntRange<i32>>("height").unwrap().max(), 1080);
        // No formats known (a loopback nobody feeds yet): same open request.
        assert_eq!(default_caps(&[]), caps);
    }

    #[test]
    fn default_caps_take_the_smallest_size_of_a_camera_above_1080p() {
        gst::init().unwrap();
        let caps = default_caps(&[fmt(3840, 2160)]);
        let s = caps.structure(0).unwrap();
        assert_eq!(s.get::<i32>("width").unwrap(), 3840);
        assert_eq!(s.get::<i32>("height").unwrap(), 2160);
        let caps = default_caps(&[fmt(4096, 2160), fmt(2560, 1440)]);
        assert_eq!(
            caps.structure(0).unwrap().get::<i32>("width").unwrap(),
            2560
        );
    }
    use std::str::FromStr;

    #[test]
    fn format_ids_round_trip() {
        let f = Format {
            width: 1280,
            height: 720,
            fps: Some(gst::Fraction::new(30000, 1001)),
        };
        assert_eq!(f.id(), "1280x720@30000/1001");
        assert_eq!(Format::parse(&f.id()), Some(f.clone()));
        assert_eq!(f.label(), "1280 × 720 · 29.97 fps");
        let plain = Format::parse("640x480").unwrap();
        assert_eq!(plain.fps, None);
        assert_eq!(plain.label(), "640 × 480");
        assert_eq!(Format::parse("nonsense"), None);
    }

    #[test]
    fn pauses_drop_and_shift() {
        let t = gst::ClockTime::from_mseconds;
        let mut p = Pauses::default();
        assert_eq!(p.apply(t(500)), Some(t(500)));
        // Paused at 1 s: later stamps are dropped, earlier late ones pass.
        p.pause_at = Some(t(1000));
        assert_eq!(p.apply(t(990)), Some(t(990)));
        assert_eq!(p.apply(t(1000)), None);
        assert_eq!(p.apply(t(2500)), None);
        // Resumed at 3 s: 2 s vanish; a straggler from the pause is dropped.
        p.pause_at = None;
        p.shift = 2000 * 1_000_000;
        p.resume_at = t(3000);
        assert_eq!(p.apply(t(2900)), None);
        assert_eq!(p.apply(t(3000)), Some(t(1000)));
        assert_eq!(p.apply(t(3033)), Some(t(1033)));
    }

    #[test]
    fn formats_from_caps_expand_and_sort() {
        gst::init().unwrap();
        let caps = gst::Caps::from_str(
            "video/x-raw, width=640, height=480, framerate={ 15/1, 30/1 }; \
             video/x-raw, width=1920, height=1080, framerate=30/1; \
             video/x-raw, width=1920, height=1080, framerate=30/1; \
             image/jpeg, width=3840, height=2160, framerate=30/1; \
             video/x-raw, width=1280, height=720, framerate=[ 1/1, 60/1 ]",
        )
        .unwrap();
        let labels: Vec<String> = formats_from_caps(&caps).iter().map(Format::label).collect();
        assert_eq!(
            labels,
            [
                "1920 × 1080 · 30 fps",
                "1280 × 720 · 60 fps",
                "640 × 480 · 30 fps",
                "640 × 480 · 15 fps",
            ]
        );
    }
}
