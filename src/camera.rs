//! Cameras and the GStreamer pipeline behind the viewfinder.
//!
//! ```text
//! source ! capsfilter (raw, system memory, <= 1080p) ! tee
//!   tee. ! queue (leaky) ! gtk4paintablesink                 viewfinder + photos
//!   tee. ! valve ! queue ! videorate ! videoconvert ! videoscale ! appsink (RGB)   stats
//!   tee. ! [recording bin: H.264 (VA-API) + AAC -> MP4]      added while recording
//! ```
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

pub struct CameraInfo {
    pub name: String,
    source: Source,
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
        match (api.as_str(), path) {
            ("v4l2", Some(path)) => {
                if !seen.contains(&path) {
                    seen.push(path.clone());
                    v4l2.push(CameraInfo {
                        name,
                        source: Source::V4l2(path),
                    });
                }
            }
            _ => other.push(CameraInfo {
                name,
                source: Source::Device(device),
            }),
        }
    }
    v4l2.extend(other);
    v4l2
}

/// What the bus watch tells the window.
pub enum Event {
    Error(String),
    RecordingSaved(PathBuf),
    RecordingFailed(String),
}

pub struct Camera {
    pipeline: gst::Pipeline,
    paintable: gdk::Paintable,
    preview: gst::Element,
    tee: gst::Element,
    stats_valve: gst::Element,
    stats_sink: gst_app::AppSink,
    recording: std::cell::RefCell<Option<Recording>>,
    _bus_watch: gst::bus::BusWatchGuard,
}

struct Recording {
    bin: gst::Bin,
    tee_pad: gst::Pad,
}

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| format!("GStreamer element “{factory}” is missing"))
}

impl Camera {
    /// Build the pipeline for `info` and start it. `on_event` runs on the
    /// main loop.
    pub fn open(info: &CameraInfo, on_event: impl Fn(Event) + 'static) -> Result<Self, String> {
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
        let caps = gst::Caps::builder("video/x-raw")
            .field("width", gst::IntRange::new(1, 1920))
            .field("height", gst::IntRange::new(1, 1080))
            .build();
        let filter = make("capsfilter")?;
        filter.set_property("caps", &caps);
        let tee = make("tee")?;

        let preview_queue = make("queue")?;
        preview_queue.set_property("max-size-buffers", 2u32);
        preview_queue.set_property_from_str("leaky", "downstream");
        let preview = make("gtk4paintablesink")?;
        preview.set_property("enable-last-sample", true);

        let stats_valve = make("valve")?;
        stats_valve.set_property("drop", true);
        let stats_queue = make("queue")?;
        stats_queue.set_property("max-size-buffers", 1u32);
        stats_queue.set_property_from_str("leaky", "downstream");
        let rate = make("videorate")?;
        rate.set_property("drop-only", true);
        rate.set_property("max-rate", 4i32);
        let convert = make("videoconvert")?;
        let scale = make("videoscale")?;
        let stats_sink = gst_app::AppSink::builder()
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGB")
                    .field("width", 480i32)
                    .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                    .build(),
            )
            .max_buffers(1)
            .drop(true)
            .sync(false)
            .build();
        // The valve keeps this branch empty until the card is shown; a sink
        // that waited for a first frame there would hold the whole pipeline
        // in PAUSED.
        stats_sink.set_async(false);

        let elements = [
            &source,
            &filter,
            &tee,
            &preview_queue,
            &preview,
            &stats_valve,
            &stats_queue,
            &rate,
            &convert,
            &scale,
            stats_sink.upcast_ref(),
        ];
        pipeline.add_many(elements).map_err(|e| e.to_string())?;
        gst::Element::link_many([&source, &filter, &tee]).map_err(|e| e.to_string())?;
        gst::Element::link_many([&tee, &preview_queue, &preview]).map_err(|e| e.to_string())?;
        gst::Element::link_many([
            &tee,
            &stats_valve,
            &stats_queue,
            &rate,
            &convert,
            &scale,
            stats_sink.upcast_ref(),
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
                        // An error inside the recording bin ends the
                        // recording, not the viewfinder.
                        let from_recording = message
                            .src()
                            .and_then(|s| s.downcast_ref::<gst::Element>().cloned())
                            .is_some_and(|e| in_recording_bin(&e));
                        if from_recording {
                            on_event(Event::RecordingFailed(text));
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
            tee,
            stats_valve,
            stats_sink,
            recording: std::cell::RefCell::new(None),
            _bus_watch: bus_watch,
        })
    }

    pub fn paintable(&self) -> gdk::Paintable {
        self.paintable.clone()
    }

    pub fn set_stats(&self, on: bool) {
        self.stats_valve.set_property("drop", !on);
    }

    /// The newest statistics frame as (RGB bytes, width, height, stride).
    pub fn stats_frame(&self) -> Option<(Vec<u8>, usize, usize, usize)> {
        let sample = self.stats_sink.try_pull_sample(gst::ClockTime::ZERO)?;
        let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
        let buffer = sample.buffer()?;
        let map = buffer.map_readable().ok()?;
        Some((
            map.as_slice().to_vec(),
            info.width() as usize,
            info.height() as usize,
            info.stride()[0] as usize,
        ))
    }

    /// Save the frame on screen as a JPEG. Runs the conversion and encoding
    /// off the main loop.
    pub fn take_photo(
        &self,
        path: PathBuf,
    ) -> impl std::future::Future<Output = Result<PathBuf, String>> + 'static {
        let sample = self.preview.property::<Option<gst::Sample>>("last-sample");
        async move {
            let sample = sample.ok_or("no frame yet")?;
            gio::spawn_blocking(move || save_jpeg(&sample, &path).map(|_| path))
                .await
                .map_err(|_| "photo thread failed".to_string())?
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.borrow().is_some()
    }

    /// Start recording the camera and the default microphone to `path`
    /// (H.264 on the GPU through VA-API, AAC, MP4).
    pub fn start_recording(&self, path: &Path) -> Result<(), String> {
        if self.is_recording() {
            return Ok(());
        }
        let description = format!(
            "queue name=vq max-size-buffers=0 max-size-bytes=0 max-size-time=2000000000 \
               ! videoconvert ! vah264enc bitrate=8000 ! h264parse \
               ! mp4mux name=mux ! filesink name=out location=\"{}\" \
             pulsesrc name=mic ! queue max-size-time=2000000000 ! audioconvert ! audioresample \
               ! audio/x-raw,rate=48000,channels=2 ! avenc_aac bitrate=160000 ! aacparse ! mux.",
            path.display().to_string().replace('"', "\\\"")
        );
        let bin = gst::parse::bin_from_description(&description, true)
            .map_err(|e| format!("cannot build the recorder: {e}"))?;
        bin.set_property("message-forward", true);
        bin.set_property("name", "recording");

        // Both branches reach the muxer in pipeline running time; start the
        // file at zero.
        let now = self
            .pipeline
            .current_running_time()
            .unwrap_or(gst::ClockTime::ZERO);
        let offset = -(now.nseconds() as i64);
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

    /// End the recording: detach it from the camera and send end-of-stream
    /// through both branches. The file is complete when the bus reports
    /// [`Event::RecordingSaved`].
    pub fn stop_recording(&self) {
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

fn in_recording_bin(element: &gst::Element) -> bool {
    let mut parent = element.parent();
    while let Some(p) = parent {
        if p.name() == "recording" {
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
