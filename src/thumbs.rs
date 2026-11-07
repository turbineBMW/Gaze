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

pub struct Previews {
    pipeline: gst::Pipeline,
    tap: TapId,
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

        // The tap feeds the appsrc; its caps follow the first frame's size.
        let size: Arc<Mutex<Option<(usize, usize)>>> = Arc::default();
        let tap = camera.add_tap(
            Tap {
                max_width: Some(WIDTH),
                fps: FPS,
            },
            move |frame| {
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
            },
        );
        Ok(Self { pipeline, tap })
    }

    pub fn stop(self, camera: &Camera) {
        camera.remove_tap(self.tap);
    }
}

impl Drop for Previews {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
