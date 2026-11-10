//! Live thumbnails for the effects picker: a second, tiny GL pipeline fed
//! by a tap of the camera, with one `gleffects` branch per effect.
//!
//! ```text
//! appsrc (RGB, ~160 px) ! videoconvert ! glupload ! glcolorconvert ! tee
//!   tee. ! queue (leaky) ! gleffects effect=X ! gldownload ! videoconvert ! appsink   per effect
//! ```

use crate::camera::{Camera, Frame, Tap, TapId, EFFECTS};
use gst::prelude::*;
use std::sync::{Arc, Mutex};

/// A thumbnail for one effect ("none" is the plain picture).
pub type Thumb = (&'static str, Frame);

/// Built once per camera and kept: closing the picker only detaches the
/// tap. Taking a GL pipeline to NULL while frames are in flight hung the
/// whole window on the SP11 (the GL thread stuck in Mesa, the main thread
/// waiting for its stream locks), so the real teardown happens rarely and
/// off the main thread (see `Drop`).
pub struct Previews {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    results: async_channel::Sender<Thumb>,
    size: Arc<Mutex<Option<(usize, usize)>>>,
    tap: Option<TapId>,
}

const WIDTH: u32 = 160;
const FPS: u32 = 6;

impl Previews {
    /// Tap `camera` and run every effect on its frames; thumbnails go to
    /// `results` (the newest wins when the window is slow).
    pub fn start(camera: &Camera, results: async_channel::Sender<Thumb>) -> Result<Self, String> {
        if gst::ElementFactory::find("gleffects").is_none() {
            return Err("GStreamer's OpenGL effects (gleffects) are missing".into());
        }
        let mut description = String::from(
            "appsrc name=src is-live=true format=time do-timestamp=true \
             ! videoconvert ! glupload ! glcolorconvert ! tee name=t ",
        );
        for effect in EFFECTS {
            description.push_str(&format!(
                "t. ! queue max-size-buffers=1 leaky=downstream ! gleffects effect={} \
                 ! gldownload ! videoconvert ! video/x-raw,format=RGB \
                 ! appsink name=sink_{} sync=false max-buffers=1 drop=true ",
                effect.id, effect.id
            ));
        }
        let pipeline = gst::parse::launch(&description)
            .map_err(|e| format!("cannot build the effect previews: {e}"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "effect previews are not a pipeline".to_string())?;
        for effect in EFFECTS {
            let sink = pipeline
                .by_name(&format!("sink_{}", effect.id))
                .and_then(|s| s.downcast::<gst_app::AppSink>().ok())
                .ok_or("effect preview sink missing")?;
            let results = results.clone();
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        if let Some(frame) = crate::camera::frame_from_sample(&sample) {
                            let _ = results.force_send((effect.id, frame));
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }
        let appsrc = pipeline
            .by_name("src")
            .and_then(|s| s.downcast::<gst_app::AppSrc>().ok())
            .ok_or("effect preview source missing")?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| "effect previews could not start".to_string())?;
        let mut previews = Self {
            pipeline,
            appsrc,
            results,
            size: Arc::default(),
            tap: None,
        };
        previews.resume(camera);
        Ok(previews)
    }

    /// Whether the previews are being fed (the picker is open).
    pub fn is_live(&self) -> bool {
        self.tap.is_some()
    }

    /// Feed the previews from `camera` again.
    pub fn resume(&mut self, camera: &Camera) {
        if self.tap.is_some() {
            return;
        }
        let (appsrc, results, size) =
            (self.appsrc.clone(), self.results.clone(), self.size.clone());
        // The tap feeds the appsrc; its caps follow the first frame's size.
        self.tap = Some(camera.add_tap(
            Tap {
                max_width: Some(WIDTH),
                fps: FPS,
            },
            move |frame| feed(&appsrc, &results, &size, frame),
        ));
    }

    /// Stop feeding the previews; the pipeline stays up for next time.
    pub fn pause(&mut self, camera: &Camera) {
        if let Some(tap) = self.tap.take() {
            camera.remove_tap(tap);
        }
    }
}

fn feed(
    appsrc: &gst_app::AppSrc,
    results: &async_channel::Sender<Thumb>,
    size: &Mutex<Option<(usize, usize)>>,
    frame: &Frame,
) {
    let mut size = size.lock().unwrap_or_else(|e| e.into_inner());
    if *size != Some((frame.width, frame.height)) {
        appsrc.set_caps(Some(
            &gst::Caps::builder("video/x-raw")
                .field("format", "RGB")
                .field("width", frame.width as i32)
                .field("height", frame.height as i32)
                .field("framerate", gst::Fraction::new(0, 1))
                .build(),
        ));
        *size = Some((frame.width, frame.height));
    }
    let _ = results.force_send(("none", frame.clone()));
    let mut buffer =
        gst::Buffer::with_size(frame.width * frame.height * 3).expect("a small buffer");
    {
        let buffer = buffer.get_mut().expect("a fresh buffer");
        let mut map = buffer.map_writable().expect("a writable buffer");
        let row = frame.width * 3;
        for (src, dst) in frame
            .rgb
            .chunks(frame.stride)
            .take(frame.height)
            .zip(map.as_mut_slice().chunks_mut(row))
        {
            dst.copy_from_slice(&src[..row]);
        }
    }
    let _ = appsrc.push_buffer(buffer);
}

impl Drop for Previews {
    /// Drain the pipeline with EOS, then take it to NULL on its own thread:
    /// if the GL driver wedges, the window and the camera keep running.
    fn drop(&mut self) {
        let _ = self.appsrc.end_of_stream();
        let pipeline = self.pipeline.clone();
        let _ = std::thread::Builder::new()
            .name("gaze:thumbs-stop".into())
            .spawn(move || {
                if let Some(bus) = pipeline.bus() {
                    bus.timed_pop_filtered(
                        gst::ClockTime::from_seconds(2),
                        &[gst::MessageType::Eos, gst::MessageType::Error],
                    );
                }
                let _ = pipeline.set_state(gst::State::Null);
            });
    }
}
