//! The main window: viewfinder, shutter, camera picker, the colour
//! statistics card and the code scanner's card.

use crate::camera::{
    self, Camera, CameraInfo, Effect, Event, Format, LatestFrame, Tap, TapId, EFFECTS,
};
use crate::codes::{self, Code, Payload, Scanner};
use crate::controls;
use crate::gifcap::{self, GifCapture};
use crate::matte;
use crate::motion;
use crate::scan;
use crate::settings::{Settings, LAPSES, TIMERS};
use crate::stats::{self, Region};
use crate::text;
use crate::thumbs::{Previews, Thumb};
use crate::vcam;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Photo,
    Video,
    Gif,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Photo => "photo",
            Mode::Video => "video",
            Mode::Gif => "gif",
        }
    }

    fn from_name(name: &str) -> Option<Mode> {
        match name {
            "photo" => Some(Mode::Photo),
            "video" => Some(Mode::Video),
            "gif" => Some(Mode::Gif),
            _ => None,
        }
    }
}

/// A photo or recording made this session, for the gallery.
struct Capture {
    path: PathBuf,
    video: bool,
}

/// How many the gallery keeps.
const GALLERY_SIZE: usize = 24;

struct Ui {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    status: adw::StatusPage,
    picture: gtk::Picture,
    /// Drawn over the picture: the measured region and code outlines.
    overlay: gtk::DrawingArea,
    countdown: gtk::Label,
    timer_label: gtk::Label,
    stats_card: gtk::Box,
    code_card: gtk::Box,
    code_title: gtk::Label,
    code_format: gtk::Label,
    code_text: gtk::Label,
    code_primary: gtk::Button,
    /// Text mode: the chips over each line, and its card.
    text_layer: gtk::Fixed,
    text_card: gtk::Box,
    text_title: gtk::Label,
    read_text: gtk::ToggleButton,
    stats_labels: Vec<gtk::Label>,
    swatch: gtk::DrawingArea,
    cameras_dropdown: gtk::DropDown,
    shutter: gtk::Button,
    mode_toggle: adw::ToggleGroup,
    timer: gtk::Label,
    flash: gtk::Box,
    controls: gtk::CenterBox,
    /// Leaves compact mode; only shown in it.
    expand: gtk::Button,
    toolbar: adw::ToolbarView,
    /// The Format submenu, refilled for each camera.
    formats_menu: gio::Menu,
    /// The Microphone submenu, refilled with the cameras.
    mics_menu: gio::Menu,
    pause_button: gtk::ToggleButton,
    level: gtk::LevelBar,
    /// The last capture, and the gallery popover behind it.
    thumb_button: gtk::MenuButton,
    thumb_picture: gtk::Picture,
    gallery: gtk::FlowBox,
    /// The camera controls popover's content, rebuilt per camera.
    controls_box: gtk::Box,
    controls_button: gtk::MenuButton,
    /// The effects picker and its live tiles, by effect id ("none" too).
    effects_button: gtk::MenuButton,
    effect_pictures: Vec<(&'static str, gtk::Picture)>,
}

pub struct Window {
    ui: Ui,
    settings: RefCell<Settings>,
    cameras: RefCell<Vec<CameraInfo>>,
    camera: RefCell<Option<Camera>>,
    mode: Cell<Mode>,
    mirror: Cell<bool>,
    stats_on: Cell<bool>,
    /// The stats card's tap on the open camera, and where its frames land.
    stats_tap: Cell<Option<TapId>>,
    stats_frames: LatestFrame,
    /// Self-timer length in seconds; 0 is off.
    timer_secs: Cell<u32>,
    /// A countdown in progress: its ticking source and the seconds left.
    countdown: RefCell<Option<glib::SourceId>>,
    countdown_left: Cell<u32>,
    /// Code scanning: whether it is on, the scanner on the open camera, the
    /// codes in the last frame (for the outlines) and the one on the card.
    scan_on: Cell<bool>,
    scanner: RefCell<Option<Scanner>>,
    code_sender: async_channel::Sender<Vec<Code>>,
    codes: RefCell<Vec<Code>>,
    shown_code: RefCell<Option<(Code, Payload)>>,
    /// A dismissed code's text: not shown again until it leaves the frame.
    dismissed_code: RefCell<Option<String>>,
    effect: Cell<Option<&'static Effect>>,
    /// Text mode: the frozen frame's lines and their chips, if reading.
    text_lines: RefCell<Vec<text::Line>>,
    text_chips: RefCell<Vec<gtk::Button>>,
    frozen: Cell<bool>,
    reading: Cell<bool>,
    /// The virtual camera: wanted (the setting) and being set up.
    virtual_on: Cell<bool>,
    virtual_starting: Cell<bool>,
    grid: Cell<bool>,
    captures: RefCell<Vec<Capture>>,
    /// The frame a recording started on, for its gallery entry.
    recording_thumb: RefCell<Option<camera::Frame>>,
    /// A burst in progress: its ticking source and how many so far.
    burst: RefCell<Option<glib::SourceId>>,
    burst_count: Cell<u32>,
    /// Time the current recording has spent paused, and since when if it
    /// is paused now.
    paused_total: Cell<Duration>,
    paused_at: Cell<Option<Instant>>,
    /// The open camera's V4L2 node, for its controls.
    device_path: RefCell<Option<PathBuf>>,
    /// Motion-triggered capture: on, its tap, the frames, the last one
    /// compared, and when the last photo was taken.
    motion_on: Cell<bool>,
    motion_tap: Cell<Option<TapId>>,
    motion_frames: LatestFrame,
    motion_prev: RefCell<Option<camera::Frame>>,
    motion_last_shot: Cell<Option<Instant>>,
    lapse: Cell<u32>,
    /// The page found by Scan Document, outlined while it is processed.
    scan_quad: RefCell<Option<scan::Quad>>,
    /// The backdrop wanted ("none", "blur" or a picture's path), being set
    /// up, and compact mode.
    background: RefCell<String>,
    background_starting: Cell<bool>,
    compact: Cell<bool>,
    /// A GIF being captured, and since when.
    gif: RefCell<Option<GifCapture>>,
    gif_since: Cell<Option<Instant>>,
    /// The effects picker's live previews while it is open.
    previews: RefCell<Option<Previews>>,
    thumb_sender: async_channel::Sender<Thumb>,
    /// Region being measured, in displayed (possibly mirrored) coordinates.
    region: Cell<Option<Region>>,
    drag_start: Cell<Option<(f64, f64)>>,
    recording_since: Cell<Option<Instant>>,
    swatch_rgb: Cell<[f64; 3]>,
    /// Set while the camera list is rebuilt, so the dropdown's selection
    /// changes don't open cameras on the way.
    rebuilding: Cell<bool>,
}

impl Window {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        let settings = Settings::load();
        let ui = build_ui(app);
        // The scanner's worker thread sends each scan here; only the newest
        // unread one is kept.
        let (code_sender, code_receiver) = async_channel::bounded::<Vec<Code>>(1);
        let (thumb_sender, thumb_receiver) = async_channel::bounded::<Thumb>(64);
        let this = Rc::new(Self {
            ui,
            mirror: Cell::new(settings.mirror),
            stats_on: Cell::new(false),
            stats_tap: Cell::new(None),
            stats_frames: LatestFrame::default(),
            timer_secs: Cell::new(settings.timer),
            countdown: RefCell::new(None),
            countdown_left: Cell::new(0),
            scan_on: Cell::new(false),
            scanner: RefCell::new(None),
            code_sender,
            codes: RefCell::new(Vec::new()),
            shown_code: RefCell::new(None),
            dismissed_code: RefCell::new(None),
            effect: Cell::new(None),
            text_lines: RefCell::new(Vec::new()),
            text_chips: RefCell::new(Vec::new()),
            frozen: Cell::new(false),
            reading: Cell::new(false),
            virtual_on: Cell::new(settings.virtual_camera),
            virtual_starting: Cell::new(false),
            grid: Cell::new(settings.grid),
            captures: RefCell::new(Vec::new()),
            recording_thumb: RefCell::new(None),
            burst: RefCell::new(None),
            burst_count: Cell::new(0),
            paused_total: Cell::new(Duration::ZERO),
            paused_at: Cell::new(None),
            device_path: RefCell::new(None),
            motion_on: Cell::new(false),
            motion_tap: Cell::new(None),
            motion_frames: LatestFrame::default(),
            motion_prev: RefCell::new(None),
            motion_last_shot: Cell::new(None),
            lapse: Cell::new(settings.lapse),
            scan_quad: RefCell::new(None),
            background: RefCell::new(settings.background.clone()),
            background_starting: Cell::new(false),
            compact: Cell::new(false),
            gif: RefCell::new(None),
            gif_since: Cell::new(None),
            previews: RefCell::new(None),
            thumb_sender,
            settings: RefCell::new(settings),
            cameras: RefCell::new(Vec::new()),
            camera: RefCell::new(None),
            mode: Cell::new(Mode::Photo),
            region: Cell::new(None),
            drag_start: Cell::new(None),
            recording_since: Cell::new(None),
            swatch_rgb: Cell::new([0.0; 3]),
            rebuilding: Cell::new(false),
        });
        this.connect();
        this.install_actions(app);
        glib::spawn_future_local(glib::clone!(
            #[weak]
            this,
            async move {
                while let Ok(codes) = code_receiver.recv().await {
                    this.on_codes(codes);
                }
            }
        ));
        glib::spawn_future_local(glib::clone!(
            #[weak]
            this,
            async move {
                while let Ok((id, frame)) = thumb_receiver.recv().await {
                    this.on_thumb(id, frame);
                }
            }
        ));
        this.refresh_cameras();
        this.apply_mirror();
        this.apply_timer();
        this.ui.window.present();
        this
    }

    fn connect(self: &Rc<Self>) {
        let ui = &self.ui;
        ui.shutter.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.shutter()
        ));
        // Hold the shutter for a burst (photo mode): claiming the sequence
        // keeps the release from counting as a click too.
        let hold = gtk::GestureLongPress::new();
        hold.set_propagation_phase(gtk::PropagationPhase::Capture);
        hold.connect_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, _, _| {
                if this.mode.get() == Mode::Photo
                    && !this.counting_down()
                    && !this.frozen.get()
                    && this.camera.borrow().is_some()
                {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    this.start_burst();
                }
            }
        ));
        hold.connect_end(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.stop_burst()
        ));
        hold.connect_cancelled(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.stop_burst()
        ));
        ui.shutter.add_controller(hold);
        if let Some(popover) = ui.effects_button.popover() {
            popover.connect_show(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.start_previews()
            ));
            popover.connect_hide(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.stop_previews()
            ));
        }
        ui.gallery.connect_child_activated(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, child| {
                let index = child.index();
                let path = {
                    let captures = this.captures.borrow();
                    (index >= 0).then(|| captures.get(index as usize).map(|c| c.path.clone()))
                };
                if let Some(Some(path)) = path {
                    this.ui.thumb_button.popdown();
                    open_file(&path);
                }
            }
        ));
        ui.mode_toggle.connect_active_name_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |toggle| {
                let mode = toggle
                    .active_name()
                    .as_deref()
                    .and_then(Mode::from_name)
                    .unwrap_or(Mode::Photo);
                this.mode.set(mode);
                this.update_shutter();
                if let Some(action) = this.ui.window.lookup_action("mode") {
                    if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                        action.set_state(&mode.name().to_variant());
                    }
                }
            }
        ));
        ui.cameras_dropdown.connect_selected_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |dropdown| {
                if this.rebuilding.get() {
                    return;
                }
                let index = dropdown.selected() as usize;
                let name = this.cameras.borrow().get(index).map(|c| c.name.clone());
                if let Some(name) = name {
                    if this.settings.borrow().camera.as_deref() != Some(name.as_str()) {
                        this.settings.borrow_mut().camera = Some(name);
                        this.settings.borrow().save();
                        this.mirror.set(this.default_mirror());
                        this.apply_mirror();
                    }
                }
                this.open_selected();
            }
        ));

        // Drag on the picture to choose the region the stats card measures;
        // a click without a drag clears it.
        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, x, y| {
                if this.stats_on.get() {
                    this.drag_start.set(this.to_video(x, y));
                }
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, dx, dy| {
                let (Some(start), Some((sx, sy))) = (this.drag_start.get(), gesture.start_point())
                else {
                    return;
                };
                if let Some(end) = this.to_video(sx + dx, sy + dy) {
                    this.region.set(Some(Region {
                        x: start.0.min(end.0),
                        y: start.1.min(end.1),
                        w: (end.0 - start.0).abs(),
                        h: (end.1 - start.1).abs(),
                    }));
                    this.ui.overlay.queue_draw();
                }
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, dx, dy| {
                if this.drag_start.take().is_some() && dx.abs() < 4.0 && dy.abs() < 4.0 {
                    this.region.set(None);
                    this.ui.overlay.queue_draw();
                }
            }
        ));
        ui.stack.add_controller(drag);

        ui.overlay.set_draw_func(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, cr, _, _| {
                let Some((vx, vy, vw, vh)) = this.video_rect() else {
                    return;
                };
                let mirrored = this.mirror.get();
                // Code outlines, in the accent colour. Bounds are in frame
                // coordinates; the picture may be mirrored.
                let codes = this.codes.borrow();
                if !codes.is_empty() {
                    let accent = adw::StyleManager::default().accent_color_rgba();
                    for code in codes.iter() {
                        let (x, y, w, h) = code.bounds;
                        let x = if mirrored { 1.0 - x - w } else { x };
                        rounded_rect(cr, vx + x * vw, vy + y * vh, w * vw, h * vh, 10.0);
                        cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
                        cr.set_line_width(6.0);
                        let _ = cr.stroke_preserve();
                        cr.set_source_rgba(
                            accent.red() as f64,
                            accent.green() as f64,
                            accent.blue() as f64,
                            1.0,
                        );
                        cr.set_line_width(3.0);
                        let _ = cr.stroke();
                    }
                }
                drop(codes);
                if let Some(quad) = *this.scan_quad.borrow() {
                    let accent = adw::StyleManager::default().accent_color_rgba();
                    for (i, (x, y)) in quad.iter().enumerate() {
                        let x = if mirrored { 1.0 - x } else { *x };
                        let (px, py) = (vx + x * vw, vy + y * vh);
                        if i == 0 {
                            cr.move_to(px, py);
                        } else {
                            cr.line_to(px, py);
                        }
                    }
                    cr.close_path();
                    cr.set_source_rgba(
                        accent.red() as f64,
                        accent.green() as f64,
                        accent.blue() as f64,
                        0.25,
                    );
                    let _ = cr.fill_preserve();
                    cr.set_source_rgba(
                        accent.red() as f64,
                        accent.green() as f64,
                        accent.blue() as f64,
                        1.0,
                    );
                    cr.set_line_width(3.0);
                    let _ = cr.stroke();
                }
                if this.grid.get() {
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
                    cr.set_line_width(1.0);
                    for i in 1..3 {
                        let f = i as f64 / 3.0;
                        cr.move_to(vx + f * vw, vy);
                        cr.line_to(vx + f * vw, vy + vh);
                        cr.move_to(vx, vy + f * vh);
                        cr.line_to(vx + vw, vy + f * vh);
                    }
                    let _ = cr.stroke();
                }
                if !this.stats_on.get() {
                    return;
                }
                let Some(r) = this.region.get() else {
                    return;
                };
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
                cr.set_line_width(2.0);
                cr.set_dash(&[6.0, 4.0], 0.0);
                cr.rectangle(vx + r.x * vw, vy + r.y * vh, r.w * vw, r.h * vh);
                let _ = cr.stroke();
            }
        ));
        ui.overlay.connect_resize(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, _| this.layout_chips()
        ));
        ui.code_primary.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.code_action()
        ));
        ui.swatch.set_draw_func(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, cr, w, h| {
                let [r, g, b] = this.swatch_rgb.get();
                cr.set_source_rgb(r / 255.0, g / 255.0, b / 255.0);
                cr.rectangle(0.0, 0.0, w as f64, h as f64);
                let _ = cr.fill();
            }
        ));

        glib::timeout_add_local(
            Duration::from_millis(500),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    this.tick();
                    glib::ControlFlow::Continue
                }
            ),
        );

        ui.window.connect_close_request(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| {
                if let Some(camera) = this.camera.take() {
                    camera.close();
                }
                glib::Propagation::Proceed
            }
        ));
    }

    fn install_actions(self: &Rc<Self>, app: &adw::Application) {
        let window = &self.ui.window;
        let shutter = gio::SimpleAction::new("shutter", None);
        shutter.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.shutter()
        ));
        window.add_action(&shutter);
        app.set_accels_for_action("win.shutter", &["space", "Return"]);

        // Photo or video, by name: what the toggle at the bottom left does.
        let mode = gio::SimpleAction::new_stateful(
            "mode",
            Some(glib::VariantTy::STRING),
            &"photo".to_variant(),
        );
        mode.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, value| {
                if let Some(name @ ("photo" | "video" | "gif")) = value.and_then(|v| v.str()) {
                    this.ui.mode_toggle.set_active_name(Some(name));
                }
            }
        ));
        window.add_action(&mode);

        // Start or stop a recording from any mode.
        let record = gio::SimpleAction::new("record", None);
        record.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                if this.mode.get() != Mode::Video {
                    this.ui.mode_toggle.set_active_name(Some("video"));
                }
                this.shutter();
            }
        ));
        window.add_action(&record);
        app.set_accels_for_action("win.record", &["<Control>r"]);

        let mirror =
            gio::SimpleAction::new_stateful("mirror", None, &self.mirror.get().to_variant());
        mirror.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |action, _| {
                let on = !this.mirror.get();
                this.mirror.set(on);
                action.set_state(&on.to_variant());
                this.settings.borrow_mut().mirror = on;
                this.settings.borrow().save();
                this.apply_mirror();
            }
        ));
        window.add_action(&mirror);
        app.set_accels_for_action("win.mirror", &["<Control>m"]);

        let stats = gio::SimpleAction::new_stateful("stats", None, &false.to_variant());
        stats.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |action, _| {
                let on = !this.stats_on.get();
                this.stats_on.set(on);
                action.set_state(&on.to_variant());
                this.ui.stats_card.set_visible(on);
                this.ui.overlay.queue_draw();
                this.sync_stats_tap();
            }
        ));
        window.add_action(&stats);
        app.set_accels_for_action("win.stats", &["<Control>i"]);

        let scan = gio::SimpleAction::new_stateful("scan", None, &false.to_variant());
        scan.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |action, _| {
                let on = !this.scan_on.get();
                this.scan_on.set(on);
                action.set_state(&on.to_variant());
                if !on {
                    this.codes.borrow_mut().clear();
                    this.ui.overlay.queue_draw();
                    this.close_code();
                }
                this.sync_scanner();
            }
        ));
        window.add_action(&scan);
        app.set_accels_for_action("win.scan", &["<Control>s"]);

        // Effects: a radio over EFFECTS by id, "none" for the plain picture.
        let effect = gio::SimpleAction::new_stateful(
            "effect",
            Some(glib::VariantTy::STRING),
            &"none".to_variant(),
        );
        effect.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, value| {
                let id = value.and_then(|v| v.str()).unwrap_or("none");
                this.set_effect(camera::effect_by_id(id));
            }
        ));
        window.add_action(&effect);
        app.set_accels_for_action("win.effect::none", &["<Control>0"]);

        // Read the text in the picture (freezes it), or go back to live.
        let read = gio::SimpleAction::new_stateful("read-text", None, &false.to_variant());
        read.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                if this.frozen.get() || this.reading.get() {
                    this.unfreeze();
                } else {
                    this.read_text();
                }
            }
        ));
        window.add_action(&read);
        app.set_accels_for_action("win.read-text", &["<Control><Shift>t"]);

        let copy_all = gio::SimpleAction::new("copy-all-text", None);
        copy_all.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                let lines = this.text_lines.borrow();
                if lines.is_empty() {
                    return;
                }
                let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
                this.ui.window.clipboard().set_text(&text.join("\n"));
                this.ui.toasts.add_toast(adw::Toast::new("Copied all text"));
            }
        ));
        window.add_action(&copy_all);

        let virtual_camera = gio::SimpleAction::new_stateful(
            "virtual-camera",
            None,
            &self.virtual_on.get().to_variant(),
        );
        virtual_camera.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.set_virtual_camera(!this.virtual_on.get())
        ));
        window.add_action(&virtual_camera);
        app.set_accels_for_action("win.virtual-camera", &["<Control>v"]);

        // The session's gallery, from the keyboard.
        let gallery = gio::SimpleAction::new("gallery", None);
        gallery.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                if this.ui.thumb_button.is_visible() {
                    this.ui.thumb_button.popup();
                }
            }
        ));
        window.add_action(&gallery);
        app.set_accels_for_action("win.gallery", &["<Control><Shift>g"]);

        // The backdrop: a radio over "none", "blur" and "image" (the
        // picture last chosen), plus ways to pick the picture.
        let background = gio::SimpleAction::new_stateful(
            "background",
            Some(glib::VariantTy::STRING),
            &background_state(&self.background.borrow()).to_variant(),
        );
        background.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, value| {
                let wanted = match value.and_then(|v| v.str()) {
                    Some("blur") => "blur".to_string(),
                    Some("image") => {
                        let current = this.background.borrow().clone();
                        if current != "none" && current != "blur" {
                            current
                        } else {
                            this.choose_background_picture();
                            return;
                        }
                    }
                    _ => "none".to_string(),
                };
                this.set_background(wanted);
            }
        ));
        window.add_action(&background);

        // Ctrl+B: blur on and off, the quick way.
        let blur = gio::SimpleAction::new("background-blur", None);
        blur.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                let next = if *this.background.borrow() == "blur" {
                    "none"
                } else {
                    "blur"
                };
                this.set_background(next.to_string());
            }
        ));
        window.add_action(&blur);
        app.set_accels_for_action("win.background-blur", &["<Control>b"]);

        let choose = gio::SimpleAction::new("choose-background", None);
        choose.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.choose_background_picture()
        ));
        window.add_action(&choose);

        let wallpaper = gio::SimpleAction::new("background-wallpaper", None);
        wallpaper.set_enabled(omarchy_wallpaper().is_some());
        wallpaper.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                if let Some(path) = omarchy_wallpaper() {
                    this.set_background(path.to_string_lossy().to_string());
                }
            }
        ));
        window.add_action(&wallpaper);

        // Compact: a small, always-on-top mirror in the corner.
        let compact = gio::SimpleAction::new_stateful("compact", None, &false.to_variant());
        compact.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.set_compact(!this.compact.get())
        ));
        window.add_action(&compact);
        app.set_accels_for_action("win.compact", &["<Control>k"]);

        // Scan the page in front of the camera to a PDF.
        let scan_doc = gio::SimpleAction::new("scan-document", None);
        scan_doc.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.scan_document()
        ));
        window.add_action(&scan_doc);
        app.set_accels_for_action("win.scan-document", &["<Control><Shift>d"]);

        // The effects picker, from the keyboard.
        let effects_action = gio::SimpleAction::new("effects", None);
        effects_action.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.ui.effects_button.popup()
        ));
        window.add_action(&effects_action);
        app.set_accels_for_action("win.effects", &["<Control><Shift>e"]);

        // The camera controls popover, from the keyboard.
        let controls_action = gio::SimpleAction::new("controls", None);
        controls_action.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.ui.controls_button.popup()
        ));
        window.add_action(&controls_action);
        app.set_accels_for_action("win.controls", &["<Control><Shift>k"]);

        // Copy the last photo to the clipboard.
        let copy_last = gio::SimpleAction::new("copy-last", None);
        copy_last.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.copy_last_photo()
        ));
        window.add_action(&copy_last);
        app.set_accels_for_action("win.copy-last", &["<Control><Shift>c"]);

        // Keep the virtual camera device across reboots: a system service
        // installed (or removed) through pkexec.
        let permanent = gio::SimpleAction::new_stateful(
            "virtual-camera-permanent",
            None,
            &vcam::service_installed().to_variant(),
        );
        permanent.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |action, _| {
                let want = !vcam::service_installed();
                let action = action.clone();
                let this = this.clone();
                glib::spawn_future_local(async move {
                    match vcam::set_service(want).await {
                        Ok(()) => this.ui.toasts.add_toast(adw::Toast::new(if want {
                            "The virtual camera will be there after every boot"
                        } else {
                            "The virtual camera will need a prompt after a reboot"
                        })),
                        Err(error) => this.ui.toasts.add_toast(adw::Toast::new(&error)),
                    }
                    action.set_state(&vcam::service_installed().to_variant());
                });
            }
        ));
        window.add_action(&permanent);

        let close_code = gio::SimpleAction::new("close-code", None);
        close_code.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.close_code()
        ));
        window.add_action(&close_code);

        let copy_code = gio::SimpleAction::new("copy-code", None);
        copy_code.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.copy_code()
        ));
        window.add_action(&copy_code);

        // Self-timer: a radio over the lengths in TIMERS, as seconds in a
        // string so a menu can target it.
        let timer = gio::SimpleAction::new_stateful(
            "timer",
            Some(glib::VariantTy::STRING),
            &self.timer_secs.get().to_string().to_variant(),
        );
        timer.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, value| {
                let secs = value
                    .and_then(|v| v.str())
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|s| TIMERS.contains(s));
                if let Some(secs) = secs {
                    this.set_timer(secs);
                }
            }
        ));
        window.add_action(&timer);

        // Ctrl+T steps through the timer lengths.
        let cycle = gio::SimpleAction::new("cycle-timer", None);
        cycle.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                let current = this.timer_secs.get();
                let index = TIMERS.iter().position(|&t| t == current).unwrap_or(0);
                this.set_timer(TIMERS[(index + 1) % TIMERS.len()]);
            }
        ));
        window.add_action(&cycle);
        app.set_accels_for_action("win.cycle-timer", &["<Control>t"]);

        // Escape stops a countdown, leaves text mode, leaves fullscreen.
        let cancel = gio::SimpleAction::new("cancel", None);
        cancel.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                if this.counting_down() {
                    this.cancel_countdown();
                } else if this.frozen.get() || this.reading.get() {
                    this.unfreeze();
                } else if this.compact.get() {
                    this.set_compact(false);
                } else if this.ui.window.is_fullscreen() {
                    this.ui.window.unfullscreen();
                }
            }
        ));
        window.add_action(&cancel);
        app.set_accels_for_action("win.cancel", &["Escape"]);

        // Motion-triggered photos.
        let motion = gio::SimpleAction::new_stateful("motion", None, &false.to_variant());
        motion.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |action, _| {
                let on = !this.motion_on.get();
                this.motion_on.set(on);
                action.set_state(&on.to_variant());
                this.motion_prev.replace(None);
                this.sync_motion_tap();
                this.ui.toasts.add_toast(adw::Toast::new(if on {
                    "Motion capture on: a photo whenever something moves"
                } else {
                    "Motion capture off"
                }));
            }
        ));
        window.add_action(&motion);
        app.set_accels_for_action("win.motion", &["<Control><Shift>m"]);

        // Time-lapse speed for recordings, as a string for the menu.
        let lapse = gio::SimpleAction::new_stateful(
            "lapse",
            Some(glib::VariantTy::STRING),
            &self.lapse.get().to_string().to_variant(),
        );
        lapse.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |action, value| {
                let Some(speed) = value
                    .and_then(|v| v.str())
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|s| LAPSES.contains(s))
                else {
                    return;
                };
                this.lapse.set(speed);
                action.set_state(&speed.to_string().to_variant());
                this.settings.borrow_mut().lapse = speed;
                this.settings.borrow().save();
            }
        ));
        window.add_action(&lapse);

        // Pause and resume the recording.
        let pause = gio::SimpleAction::new_stateful("pause", None, &false.to_variant());
        pause.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.toggle_pause()
        ));
        window.add_action(&pause);
        app.set_accels_for_action("win.pause", &["<Control>p"]);

        // The microphone: "default" or a PipeWire node name.
        let mic = gio::SimpleAction::new_stateful(
            "mic",
            Some(glib::VariantTy::STRING),
            &self
                .settings
                .borrow()
                .mic
                .clone()
                .unwrap_or_else(|| "default".into())
                .to_variant(),
        );
        mic.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |action, value| {
                let id = value.and_then(|v| v.str()).unwrap_or("default");
                action.set_state(&id.to_variant());
                this.settings.borrow_mut().mic = Some(id.to_string()).filter(|m| m != "default");
                this.settings.borrow().save();
            }
        ));
        window.add_action(&mic);

        // The camera's format: "auto" or a Format id, remembered per camera.
        let format = gio::SimpleAction::new_stateful(
            "format",
            Some(glib::VariantTy::STRING),
            &"auto".to_variant(),
        );
        format.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, value| {
                let id = value.and_then(|v| v.str()).unwrap_or("auto");
                let Some(name) = this.current_camera_name() else {
                    return;
                };
                let chosen = Some(id).filter(|id| *id != "auto");
                this.settings.borrow_mut().set_format(&name, chosen);
                this.settings.borrow().save();
                this.open_selected();
            }
        ));
        window.add_action(&format);

        let grid = gio::SimpleAction::new_stateful("grid", None, &self.grid.get().to_variant());
        grid.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |action, _| {
                let on = !this.grid.get();
                this.grid.set(on);
                action.set_state(&on.to_variant());
                this.settings.borrow_mut().grid = on;
                this.settings.borrow().save();
                this.ui.overlay.queue_draw();
            }
        ));
        window.add_action(&grid);
        app.set_accels_for_action("win.grid", &["<Control>g"]);

        let fullscreen = gio::SimpleAction::new_stateful("fullscreen", None, &false.to_variant());
        fullscreen.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                if this.ui.window.is_fullscreen() {
                    this.ui.window.unfullscreen();
                } else {
                    this.ui.window.fullscreen();
                }
            }
        ));
        window.add_action(&fullscreen);
        app.set_accels_for_action("win.fullscreen", &["F11"]);
        // The header bar goes with the window chrome.
        window.connect_fullscreened_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |window| {
                let full = window.is_fullscreen();
                this.ui.toolbar.set_reveal_top_bars(!full);
                if let Some(action) = window.lookup_action("fullscreen") {
                    if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                        action.set_state(&full.to_variant());
                    }
                }
            }
        ));

        for (name, dir) in [("open-photos", photos_dir()), ("open-videos", videos_dir())] {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(move |_, _| {
                let _ = std::fs::create_dir_all(&dir);
                let uri = gio::File::for_path(&dir).uri();
                let _ = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
            });
            window.add_action(&action);
        }

        let refresh = gio::SimpleAction::new("refresh", None);
        refresh.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.refresh_cameras()
        ));
        window.add_action(&refresh);

        let about = gio::SimpleAction::new("about", None);
        about.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                adw::AboutDialog::builder()
                    .application_name("Gaze")
                    .application_icon(crate::APP_ID)
                    .version(env!("CARGO_PKG_VERSION"))
                    .comments("A camera app that shows your camera the way video calls see it.")
                    .license_type(gtk::License::Gpl30)
                    .build()
                    .present(Some(&this.ui.window));
            }
        ));
        window.add_action(&about);
        app.set_accels_for_action("window.close", &["<Control>w"]);
    }

    fn refresh_cameras(self: &Rc<Self>) {
        if self
            .camera
            .borrow()
            .as_ref()
            .is_some_and(Camera::is_recording)
        {
            return;
        }
        if let Some(camera) = self.camera.take() {
            camera.close();
        }
        let cameras = camera::list();
        self.fill_mics_menu();
        let names: Vec<&str> = cameras.iter().map(|c| c.name.as_str()).collect();
        log::debug!("cameras: {names:?}");
        let model = gtk::StringList::new(&names);
        let wanted = self.settings.borrow().camera.clone();
        let index = wanted
            .and_then(|w| cameras.iter().position(|c| c.name == w))
            .unwrap_or(0);
        let empty = cameras.is_empty();
        self.cameras.replace(cameras);
        if empty {
            self.show_status(
                "No Camera Found",
                "Connect a camera, then choose Refresh Cameras.",
            );
            self.rebuilding.set(true);
            self.ui.cameras_dropdown.set_model(Some(&model));
            self.rebuilding.set(false);
            return;
        }
        self.rebuilding.set(true);
        self.ui.cameras_dropdown.set_model(Some(&model));
        self.ui.cameras_dropdown.set_selected(index as u32);
        self.rebuilding.set(false);
        self.open_selected();
    }

    fn open_selected(self: &Rc<Self>) {
        if self
            .camera
            .borrow()
            .as_ref()
            .is_some_and(Camera::is_recording)
        {
            return;
        }
        self.unfreeze();
        if let Some(camera) = self.camera.take() {
            camera.close();
        }
        let index = self.ui.cameras_dropdown.selected() as usize;
        let cameras = self.cameras.borrow();
        let Some(info) = cameras.get(index) else {
            return;
        };
        let format = self
            .settings
            .borrow()
            .format_for(&info.name)
            .and_then(Format::parse)
            .filter(|f| info.formats.contains(f));
        self.fill_formats_menu(info, format.as_ref());
        self.device_path.replace(info.device_path());
        self.fill_controls();
        let weak = Rc::downgrade(self);
        match Camera::open(info, format.as_ref(), move |event| {
            if let Some(this) = weak.upgrade() {
                this.on_event(event);
            }
        }) {
            Ok(camera) => {
                let paintable = camera.paintable();
                log::info!(
                    "opened {} ({})",
                    info.name,
                    format.as_ref().map_or("auto".into(), Format::label)
                );
                self.ui.picture.set_paintable(Some(&paintable));
                self.stats_tap.set(None);
                self.motion_tap.set(None);
                self.scanner.replace(None);
                self.previews.replace(None);
                if let Err(error) = camera.set_effect(self.effect.get()) {
                    log::warn!("{error}");
                    self.effect.set(None);
                }
                self.camera.replace(Some(camera));
                self.sync_stats_tap();
                self.sync_motion_tap();
                self.sync_scanner();
                self.apply_effect_state();
                if self.virtual_on.get() {
                    self.start_virtual_camera();
                }
                if *self.background.borrow() != "none" {
                    self.start_background();
                }
                self.ui.stack.set_visible_child_name("viewfinder");
                self.update_shutter();
            }
            Err(error) => self.show_status("Camera Unavailable", &error),
        }
    }

    /// The Microphone submenu: Default plus each microphone, as radio items
    /// over win.mic.
    fn fill_mics_menu(&self) {
        let menu = &self.ui.mics_menu;
        menu.remove_all();
        let item = gio::MenuItem::new(Some("Default"), None);
        item.set_action_and_target_value(Some("win.mic"), Some(&"default".to_variant()));
        menu.append_item(&item);
        for mic in camera::mics() {
            let item = gio::MenuItem::new(Some(&mic.name), None);
            item.set_action_and_target_value(Some("win.mic"), Some(&mic.device.to_variant()));
            menu.append_item(&item);
        }
    }

    fn toggle_pause(&self) {
        let camera = self.camera.borrow();
        let Some(camera) = camera.as_ref().filter(|c| c.is_recording()) else {
            return;
        };
        if self.recording_since.get().is_none() {
            return; // stopping already
        }
        let paused = !camera.is_paused();
        if paused {
            camera.pause_recording();
            self.paused_at.set(Some(Instant::now()));
        } else {
            camera.resume_recording();
            if let Some(at) = self.paused_at.take() {
                self.paused_total
                    .set(self.paused_total.get() + at.elapsed());
            }
        }
        if let Some(action) = self.ui.window.lookup_action("pause") {
            if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                action.set_state(&paused.to_variant());
            }
        }
        self.ui.pause_button.set_active(paused);
        self.ui.shutter.set_tooltip_text(Some(if paused {
            "Stop Recording (paused)"
        } else {
            "Stop Recording"
        }));
    }

    /// The controls popover: a row per V4L2 control of the open camera,
    /// and Reset.
    fn fill_controls(self: &Rc<Self>) {
        let content = &self.ui.controls_box;
        while let Some(child) = content.first_child() {
            content.remove(&child);
        }
        let device = self.device_path.borrow().clone();
        let controls = device.as_deref().map(controls::list).unwrap_or_default();
        if controls.is_empty() {
            let label = gtk::Label::builder()
                .label("This camera has no adjustable controls.")
                .wrap(true)
                .max_width_chars(32)
                .build();
            label.add_css_class("dim-label");
            content.append(&label);
            return;
        }
        let Some(device) = device else {
            return;
        };
        let apply = {
            let device = device.clone();
            let this = self.clone();
            move |id: u32, value: i64| {
                if let Err(error) = controls::set(&device, id, value) {
                    this.ui
                        .toasts
                        .add_toast(adw::Toast::new(&format!("Control not set: {error}")));
                }
            }
        };
        let apply = Rc::new(apply);
        for control in &controls {
            let row = gtk::Box::builder().spacing(10).build();
            let label = gtk::Label::builder()
                .label(&control.name)
                .xalign(0.0)
                .width_chars(18)
                .max_width_chars(18)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .tooltip_text(&control.name)
                .build();
            row.append(&label);
            let id = control.id;
            let widget: gtk::Widget = match &control.kind {
                controls::Kind::Integer { min, max, step, .. } => {
                    let scale = gtk::Scale::with_range(
                        gtk::Orientation::Horizontal,
                        *min as f64,
                        *max as f64,
                        *step as f64,
                    );
                    scale.set_value(control.value as f64);
                    scale.set_hexpand(true);
                    scale.set_draw_value(true);
                    scale.set_value_pos(gtk::PositionType::Right);
                    scale.set_digits(0);
                    scale.set_width_request(180);
                    let apply = apply.clone();
                    scale.connect_value_changed(move |s| apply(id, s.value().round() as i64));
                    scale.upcast()
                }
                controls::Kind::Boolean { .. } => {
                    let switch = gtk::Switch::builder()
                        .active(control.value != 0)
                        .halign(gtk::Align::End)
                        .hexpand(true)
                        .valign(gtk::Align::Center)
                        .build();
                    let apply = apply.clone();
                    switch.connect_state_set(move |_, on| {
                        apply(id, i64::from(on));
                        glib::Propagation::Proceed
                    });
                    switch.upcast()
                }
                controls::Kind::Menu { items, .. } => {
                    let names: Vec<&str> = items.iter().map(|(_, n)| n.as_str()).collect();
                    let dropdown = gtk::DropDown::from_strings(&names);
                    if let Some(i) = items.iter().position(|(v, _)| *v == control.value) {
                        dropdown.set_selected(i as u32);
                    }
                    dropdown.set_hexpand(true);
                    let values: Vec<i64> = items.iter().map(|(v, _)| *v).collect();
                    let apply = apply.clone();
                    dropdown.connect_selected_notify(move |d| {
                        if let Some(v) = values.get(d.selected() as usize) {
                            apply(id, *v);
                        }
                    });
                    dropdown.upcast()
                }
            };
            widget.set_sensitive(!control.inactive && !control.read_only);
            row.append(&widget);
            content.append(&row);
        }
        let reset = gtk::Button::builder()
            .label("Reset to Defaults")
            .halign(gtk::Align::End)
            .margin_top(6)
            .build();
        reset.add_css_class("pill");
        reset.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                for control in &controls {
                    if !control.read_only {
                        let _ = controls::set(&device, control.id, control.default_value());
                    }
                }
                this.fill_controls();
            }
        ));
        content.append(&reset);
    }

    fn current_camera_name(&self) -> Option<String> {
        let index = self.ui.cameras_dropdown.selected() as usize;
        self.cameras.borrow().get(index).map(|c| c.name.clone())
    }

    /// The Format submenu for this camera: Automatic plus each format it
    /// offers, as radio items over win.format.
    fn fill_formats_menu(&self, info: &CameraInfo, chosen: Option<&Format>) {
        let menu = &self.ui.formats_menu;
        menu.remove_all();
        let item = gio::MenuItem::new(Some("Automatic"), None);
        item.set_action_and_target_value(Some("win.format"), Some(&"auto".to_variant()));
        menu.append_item(&item);
        for format in &info.formats {
            let item = gio::MenuItem::new(Some(&format.label()), None);
            item.set_action_and_target_value(Some("win.format"), Some(&format.id().to_variant()));
            menu.append_item(&item);
        }
        if let Some(action) = self.ui.window.lookup_action("format") {
            if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                let state = chosen.map_or("auto".to_string(), Format::id);
                action.set_state(&state.to_variant());
            }
        }
    }

    fn default_mirror(&self) -> bool {
        let index = self.ui.cameras_dropdown.selected() as usize;
        self.cameras.borrow().get(index).is_some_and(|c| {
            !c.name.to_lowercase().contains("rear") && !c.name.to_lowercase().contains("back")
        })
    }

    fn apply_mirror(&self) {
        if self.mirror.get() {
            self.ui.picture.add_css_class("mirrored");
        } else {
            self.ui.picture.remove_css_class("mirrored");
        }
        if let Some(action) = self.ui.window.lookup_action("mirror") {
            if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                action.set_state(&self.mirror.get().to_variant());
            }
        }
        self.ui.overlay.queue_draw();
    }

    /// Scan the open camera exactly while scanning is on.
    fn sync_scanner(&self) {
        let camera = self.camera.borrow();
        let Some(camera) = camera.as_ref() else {
            self.scanner.replace(None);
            return;
        };
        let running = self.scanner.borrow().is_some();
        match (self.scan_on.get(), running) {
            (true, false) => {
                let scanner = Scanner::start(camera, self.code_sender.clone());
                self.scanner.replace(Some(scanner));
            }
            (false, true) => {
                if let Some(scanner) = self.scanner.take() {
                    scanner.stop(camera);
                }
            }
            _ => {}
        }
    }

    /// A scan came back: outline the codes and put a new one on the card.
    fn on_codes(&self, codes: Vec<Code>) {
        if !self.scan_on.get() {
            return;
        }
        // A dismissed code comes back once it has been out of view.
        let still_there = |text: &str| codes.iter().any(|c| c.text == text);
        if self
            .dismissed_code
            .borrow()
            .as_ref()
            .is_some_and(|d| !still_there(d))
        {
            self.dismissed_code.replace(None);
        }
        let new = codes
            .iter()
            .find(|c| {
                let shown = self.shown_code.borrow();
                let dismissed = self.dismissed_code.borrow();
                shown.as_ref().is_none_or(|(s, _)| s.text != c.text)
                    && dismissed.as_deref() != Some(c.text.as_str())
            })
            .cloned();
        self.codes.replace(codes);
        self.ui.overlay.queue_draw();
        if let Some(code) = new {
            self.show_code(code);
        }
    }

    fn show_code(&self, code: Code) {
        let payload = codes::classify(&code.text);
        let ui = &self.ui;
        let (title, action, text) = match &payload {
            Payload::Link(uri) => ("Link", Some("Open"), uri.clone()),
            Payload::Wifi(wifi) => (
                "Wi-Fi Network",
                Some("Join"),
                match &wifi.password {
                    Some(p) => format!("{}\nPassword: {p}", wifi.ssid),
                    None => format!("{} (open network)", wifi.ssid),
                },
            ),
            Payload::Contact => ("Contact", Some("Save Contact"), summarize_vcard(&code.text)),
            Payload::Event => ("Event", Some("Save Event"), summarize_ical(&code.text)),
            Payload::Text => ("Text", None, code.text.clone()),
        };
        ui.code_title.set_label(title);
        ui.code_format.set_label(&code.format);
        ui.code_text.set_label(&text);
        ui.code_primary.set_visible(action.is_some());
        ui.code_primary.set_label(action.unwrap_or(""));
        ui.code_card.set_visible(true);
        self.shown_code.replace(Some((code, payload)));
    }

    fn close_code(&self) {
        self.ui.code_card.set_visible(false);
        if let Some((code, _)) = self.shown_code.take() {
            self.dismissed_code.replace(Some(code.text));
        }
    }

    /// Copy the code's text (a Wi-Fi code's password) to the clipboard.
    fn copy_code(&self) {
        let shown = self.shown_code.borrow();
        let Some((code, payload)) = shown.as_ref() else {
            return;
        };
        let text = match payload {
            Payload::Wifi(wifi) => wifi.password.clone().unwrap_or_else(|| wifi.ssid.clone()),
            _ => code.text.clone(),
        };
        self.ui.window.clipboard().set_text(&text);
        self.ui.toasts.add_toast(adw::Toast::new("Copied"));
    }

    /// The card's main button: open, join, or save.
    fn code_action(self: &Rc<Self>) {
        let shown = self.shown_code.borrow().clone();
        let Some((code, payload)) = shown else {
            return;
        };
        match payload {
            Payload::Link(uri) => {
                if let Err(error) =
                    gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE)
                {
                    self.ui
                        .toasts
                        .add_toast(adw::Toast::new(&format!("Could not open: {error}")));
                }
            }
            Payload::Wifi(wifi) => self.join_wifi(wifi),
            Payload::Contact => self.save_payload(&code.text, "vcf", "Contact"),
            Payload::Event => self.save_payload(&code.text, "ics", "Event"),
            Payload::Text => {}
        }
    }

    /// Connect to the network with NetworkManager, rescanning first so a
    /// network it has not seen yet is found.
    fn join_wifi(self: &Rc<Self>, wifi: codes::Wifi) {
        let mut argv = vec![
            "nmcli".to_string(),
            "device".into(),
            "wifi".into(),
            "connect".into(),
            wifi.ssid.clone(),
        ];
        if let Some(password) = &wifi.password {
            argv.push("password".into());
            argv.push(password.clone());
        }
        if wifi.hidden {
            argv.push("hidden".into());
            argv.push("yes".into());
        }
        self.ui
            .toasts
            .add_toast(adw::Toast::new(&format!("Joining {}…", wifi.ssid)));
        let this = self.clone();
        glib::spawn_future_local(async move {
            let _ = run(&["nmcli", "device", "wifi", "rescan"]).await;
            glib::timeout_future(Duration::from_secs(2)).await;
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            let message = match run(&argv).await {
                Ok(_) => format!("Joined {}", wifi.ssid),
                Err(error) => format!("Could not join {}: {error}", wifi.ssid),
            };
            this.ui.toasts.add_toast(adw::Toast::new(&message));
        });
    }

    /// Write the payload to the downloads folder and offer to open it.
    fn save_payload(&self, text: &str, extension: &str, what: &str) {
        let dir = special_dir(glib::UserDirectory::Downloads, "Downloads");
        let path = dir.join(format!("Gaze {what} {}.{extension}", timestamp()));
        let result = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text));
        match result {
            Ok(()) => self.toast_file(&format!("{what} saved"), path),
            Err(error) => self
                .ui
                .toasts
                .add_toast(adw::Toast::new(&format!("Could not save: {error}"))),
        }
    }

    /// Tap the open camera for the stats card exactly when it is shown.
    fn sync_stats_tap(&self) {
        let camera = self.camera.borrow();
        let Some(camera) = camera.as_ref() else {
            self.stats_tap.set(None);
            return;
        };
        match (self.stats_on.get(), self.stats_tap.get()) {
            (true, None) => {
                let frames = self.stats_frames.clone();
                let tap = Tap {
                    max_width: Some(480),
                    fps: 4,
                };
                let id = camera.add_tap(tap, move |frame| frames.store(frame));
                self.stats_tap.set(Some(id));
            }
            (false, Some(id)) => {
                camera.remove_tap(id);
                self.stats_tap.set(None);
            }
            _ => {}
        }
    }

    fn set_timer(&self, secs: u32) {
        self.timer_secs.set(secs);
        self.settings.borrow_mut().timer = secs;
        self.settings.borrow().save();
        self.apply_timer();
    }

    /// Freeze the picture on the next frame and read the text in it.
    fn read_text(self: &Rc<Self>) {
        if self.reading.get() || self.camera.borrow().is_none() {
            return;
        }
        self.reading.set(true);
        self.ui.read_text.set_active(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Err(error) = this.read_text_inner().await {
                this.ui.toasts.add_toast(adw::Toast::new(&error));
                this.unfreeze();
            }
            this.reading.set(false);
        });
    }

    async fn read_text_inner(self: &Rc<Self>) -> Result<(), String> {
        if !text::models_ready() {
            self.ui.toasts.add_toast(adw::Toast::new(&format!(
                "Downloading the text recognition models ({})…",
                text::MODELS_SIZE
            )));
            text::download_models().await?;
        }
        let frame = self.grab_frame().await?;
        if !self.reading.get() {
            return Ok(()); // cancelled meanwhile
        }
        self.freeze(&frame);
        self.ui.text_title.set_label("Reading…");
        self.ui.text_card.set_visible(true);
        let lines = text::read(frame).await?;
        if !self.frozen.get() {
            return Ok(());
        }
        self.ui.text_title.set_label(&match lines.len() {
            0 => "No text found".to_string(),
            1 => "1 line".to_string(),
            n => format!("{n} lines"),
        });
        self.text_lines.replace(lines);
        self.build_chips();
        Ok(())
    }

    /// One full-size frame from the analysis tap.
    async fn grab_frame(&self) -> Result<camera::Frame, String> {
        let (tx, rx) = async_channel::bounded::<camera::Frame>(1);
        let tap = {
            let camera = self.camera.borrow();
            let camera = camera.as_ref().ok_or("no camera")?;
            camera.add_tap(
                Tap {
                    max_width: None,
                    fps: 30,
                },
                move |frame| {
                    let _ = tx.try_send(frame.clone());
                },
            )
        };
        let frame = rx.recv().await.map_err(|_| "no frame".to_string());
        if let Some(camera) = self.camera.borrow().as_ref() {
            camera.remove_tap(tap);
        }
        frame
    }

    /// Freeze the picture, find the page in it, straighten it, read its
    /// text, and save a PDF.
    fn scan_document(self: &Rc<Self>) {
        if self.reading.get() || self.frozen.get() || self.camera.borrow().is_none() {
            return;
        }
        self.reading.set(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Err(error) = this.scan_document_inner().await {
                this.ui.toasts.add_toast(adw::Toast::new(&error));
            }
            this.reading.set(false);
            this.unfreeze();
        });
    }

    async fn scan_document_inner(self: &Rc<Self>) -> Result<(), String> {
        let frame = self.grab_frame().await?;
        if !self.reading.get() {
            return Ok(());
        }
        self.freeze(&frame);
        self.ui.text_title.set_label("Finding the page…");
        self.ui.text_card.set_visible(true);
        let found = gio::spawn_blocking({
            let frame = frame.clone();
            move || scan::find_page(&frame)
        })
        .await
        .map_err(|_| "scan thread failed".to_string())?;
        let quad = found.unwrap_or([(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]);
        self.scan_quad.replace(Some(quad));
        self.ui.overlay.queue_draw();
        self.ui.text_title.set_label(if found.is_some() {
            "Straightening…"
        } else {
            "No page found: scanning the whole picture"
        });
        let page = gio::spawn_blocking(move || scan::warp(&frame, &quad))
            .await
            .map_err(|_| "scan thread failed".to_string())?;
        let lines = if text::models_ready() {
            self.ui.text_title.set_label("Reading the text…");
            text::read(page.clone()).await.unwrap_or_default()
        } else {
            Vec::new()
        };
        if !self.frozen.get() {
            return Ok(());
        }
        self.ui.text_title.set_label("Saving…");
        let path = special_dir(glib::UserDirectory::Documents, "Documents")
            .join(format!("Gaze Scan {}.pdf", timestamp()));
        let thumb = scan::thumbnail(&page, camera::THUMBNAIL_WIDTH as usize);
        let saved = gio::spawn_blocking({
            let path = path.clone();
            move || scan::write_pdf(&path, &page, &lines).map(|_| path)
        })
        .await
        .map_err(|_| "scan thread failed".to_string())??;
        self.add_capture(saved.clone(), Some(thumb), false);
        self.notify_capture("Scan saved", &saved);
        self.toast_file("Scan saved", saved);
        Ok(())
    }

    /// Show `frame` instead of the live picture.
    fn freeze(&self, frame: &camera::Frame) {
        let bytes = glib::Bytes::from(&frame.rgb);
        let texture = gtk::gdk::MemoryTexture::new(
            frame.width as i32,
            frame.height as i32,
            gtk::gdk::MemoryFormat::R8g8b8,
            &bytes,
            frame.stride,
        );
        self.ui.picture.set_paintable(Some(&texture));
        self.frozen.set(true);
        self.ui.read_text.set_active(true);
    }

    /// Back to the live picture, without the chips.
    fn unfreeze(&self) {
        self.reading.set(false);
        if self.scan_quad.take().is_some() {
            self.ui.overlay.queue_draw();
        }
        if self.frozen.replace(false) {
            if let Some(camera) = self.camera.borrow().as_ref() {
                self.ui.picture.set_paintable(Some(&camera.paintable()));
            }
        }
        self.text_lines.borrow_mut().clear();
        for chip in self.text_chips.borrow_mut().drain(..) {
            self.ui.text_layer.remove(&chip);
        }
        self.ui.text_card.set_visible(false);
        self.ui.read_text.set_active(false);
    }

    /// A chip per line, placed over it; click copies the line.
    fn build_chips(self: &Rc<Self>) {
        for chip in self.text_chips.borrow_mut().drain(..) {
            self.ui.text_layer.remove(&chip);
        }
        let mut chips = Vec::new();
        for line in self.text_lines.borrow().iter() {
            let chip = gtk::Button::builder()
                .label(&line.text)
                .tooltip_text("Copy")
                .build();
            chip.add_css_class("text-chip");
            let text = line.text.clone();
            chip.connect_clicked(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.ui.window.clipboard().set_text(&text);
                    this.ui.toasts.add_toast(adw::Toast::new("Copied"));
                }
            ));
            self.ui.text_layer.put(&chip, 0.0, 0.0);
            chips.push(chip);
        }
        self.text_chips.replace(chips);
        self.layout_chips();
    }

    /// Put each chip on its line in the displayed (maybe mirrored) picture.
    fn layout_chips(&self) {
        let Some((vx, vy, vw, vh)) = self.video_rect() else {
            return;
        };
        let mirrored = self.mirror.get();
        let lines = self.text_lines.borrow();
        for (chip, line) in self.text_chips.borrow().iter().zip(lines.iter()) {
            let (x, y, w, h) = line.bounds;
            let x = if mirrored { 1.0 - x - w } else { x };
            chip.set_size_request((w * vw) as i32, (h * vh) as i32);
            self.ui.text_layer.move_(chip, vx + x * vw, vy + y * vh);
        }
    }

    /// Turn background blur on (fetching the model the first time) or off,
    /// and remember the choice.
    /// Remember and apply a backdrop: "none", "blur" or a picture's path.
    fn set_background(self: &Rc<Self>, wanted: String) {
        self.background.replace(wanted.clone());
        self.settings.borrow_mut().background = wanted.clone();
        self.settings.borrow().save();
        if let Some(action) = self.ui.window.lookup_action("background") {
            if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                action.set_state(&background_state(&wanted).to_variant());
            }
        }
        if wanted == "none" {
            if let Some(camera) = self.camera.borrow().as_ref() {
                if let Err(error) = camera.set_background(None) {
                    log::warn!("{error}");
                }
            }
        } else {
            self.start_background();
        }
    }

    /// Pick a picture for the backdrop.
    fn choose_background_picture(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Pictures"));
        filter.add_pixbuf_formats();
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Background Picture")
            .filters(&filters)
            .modal(true)
            .build();
        let this = self.clone();
        dialog.open(
            Some(&self.ui.window),
            gio::Cancellable::NONE,
            move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        this.set_background(path.to_string_lossy().to_string());
                    }
                }
            },
        );
    }

    /// Fetch the model if needed, load the picture if any, and hand the
    /// backdrop to the camera.
    fn start_background(self: &Rc<Self>) {
        if self.background_starting.get() || self.camera.borrow().is_none() {
            return;
        }
        self.background_starting.set(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = async {
                if !matte::model_ready() {
                    this.ui.toasts.add_toast(adw::Toast::new(&format!(
                        "Downloading the background matting model ({})…",
                        matte::MODEL_SIZE
                    )));
                    matte::download_model().await?;
                }
                let wanted = this.background.borrow().clone();
                let backdrop = match wanted.as_str() {
                    "none" => return Ok(()),
                    "blur" => matte::Backdrop::Blur,
                    path => {
                        let path = PathBuf::from(path);
                        let image = gio::spawn_blocking(move || load_picture(&path))
                            .await
                            .map_err(|_| "picture thread failed".to_string())??;
                        matte::Backdrop::Image(std::sync::Arc::new(image))
                    }
                };
                let camera = this.camera.borrow();
                let camera = camera.as_ref().ok_or("no camera")?;
                camera.set_background(Some(backdrop))
            }
            .await;
            this.background_starting.set(false);
            if let Err(error) = result {
                this.ui.toasts.add_toast(adw::Toast::new(&error));
                this.set_background("none".into());
            }
        });
    }

    /// Compact: just the picture, small, floating and pinned on Hyprland.
    fn set_compact(&self, on: bool) {
        self.compact.set(on);
        if let Some(action) = self.ui.window.lookup_action("compact") {
            if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                action.set_state(&on.to_variant());
            }
        }
        let ui = &self.ui;
        ui.toolbar
            .set_reveal_top_bars(!on && !ui.window.is_fullscreen());
        ui.controls.set_visible(!on);
        ui.expand.set_visible(on);
        if on {
            ui.stats_card.set_visible(false);
            self.close_code();
            self.unfreeze();
        } else {
            ui.stats_card.set_visible(self.stats_on.get());
        }
        let (w, h) = (480, 270);
        if on {
            ui.window.set_default_size(w, h);
        } else {
            ui.window.set_default_size(1100, 720);
        }
        crate::hypr::set_compact(on, w, h);
    }

    /// Turn the virtual camera on (creating the device if needed) or off,
    /// and remember the choice.
    fn set_virtual_camera(self: &Rc<Self>, on: bool) {
        self.virtual_on.set(on);
        self.settings.borrow_mut().virtual_camera = on;
        self.settings.borrow().save();
        if let Some(action) = self.ui.window.lookup_action("virtual-camera") {
            if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                action.set_state(&on.to_variant());
            }
        }
        if on {
            self.start_virtual_camera();
        } else if let Some(camera) = self.camera.borrow().as_ref() {
            camera.stop_output();
        }
    }

    fn start_virtual_camera(self: &Rc<Self>) {
        if self.virtual_starting.get() {
            return;
        }
        if self.camera.borrow().as_ref().is_none_or(Camera::is_output) {
            return;
        }
        self.virtual_starting.set(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = vcam::ensure().await.and_then(|device| {
                if !this.virtual_on.get() {
                    return Ok(None);
                }
                let camera = this.camera.borrow();
                let camera = camera.as_ref().ok_or("no camera")?;
                camera.start_output(&device).map(|_| Some(device))
            });
            this.virtual_starting.set(false);
            match result {
                Ok(Some(device)) => this.ui.toasts.add_toast(adw::Toast::new(&format!(
                    "Virtual camera on: {} appears as “{}” in other apps",
                    device.display(),
                    vcam::LABEL
                ))),
                Ok(None) => {}
                Err(error) => {
                    this.ui.toasts.add_toast(adw::Toast::new(&error));
                    this.set_virtual_camera(false);
                }
            }
        });
    }

    fn start_previews(&self) {
        if self.previews.borrow().is_some() {
            return;
        }
        let camera = self.camera.borrow();
        let Some(camera) = camera.as_ref() else {
            return;
        };
        match Previews::start(camera, self.thumb_sender.clone()) {
            Ok(previews) => {
                self.previews.replace(Some(previews));
            }
            Err(error) => log::warn!("effect previews: {error}"),
        }
    }

    fn stop_previews(&self) {
        if let Some(previews) = self.previews.take() {
            if let Some(camera) = self.camera.borrow().as_ref() {
                previews.stop(camera);
            }
        }
    }

    /// A thumbnail from the previews: onto its tile.
    fn on_thumb(&self, id: &'static str, frame: camera::Frame) {
        if self.previews.borrow().is_none() {
            return;
        }
        let Some((_, picture)) = self.ui.effect_pictures.iter().find(|(i, _)| *i == id) else {
            return;
        };
        let bytes = glib::Bytes::from(&frame.rgb);
        let texture = gdk::MemoryTexture::new(
            frame.width as i32,
            frame.height as i32,
            gdk::MemoryFormat::R8g8b8,
            &bytes,
            frame.stride,
        );
        picture.set_paintable(Some(&texture));
    }

    /// Stop the GIF capture and encode what it collected.
    fn finish_gif(self: &Rc<Self>) {
        let Some(capture) = self.gif.take() else {
            return;
        };
        self.gif_since.set(None);
        let frames = match self.camera.borrow().as_ref() {
            Some(camera) => capture.stop(camera),
            None => Vec::new(),
        };
        self.update_shutter();
        if frames.is_empty() {
            self.ui
                .toasts
                .add_toast(adw::Toast::new("No frames for the GIF"));
            return;
        }
        self.ui.timer.set_label("Saving…");
        self.ui.timer.set_visible(true);
        let thumb = scan::thumbnail(&frames[0], camera::THUMBNAIL_WIDTH as usize);
        let path = photos_dir().join(format!("Gaze {}.gif", timestamp()));
        let this = self.clone();
        glib::spawn_future_local(async move {
            match gifcap::encode(frames, path).await {
                Ok(path) => {
                    this.add_capture(path.clone(), Some(thumb), false);
                    this.notify_capture("GIF saved", &path);
                    this.toast_file("GIF saved", path);
                }
                Err(error) => this
                    .ui
                    .toasts
                    .add_toast(adw::Toast::new(&format!("Could not save the GIF: {error}"))),
            }
            if this.gif.borrow().is_none() {
                this.ui.timer.set_visible(false);
            }
        });
    }

    /// Take a photo of the frame on screen and file it.
    fn snap_photo(self: &Rc<Self>, title: &'static str) {
        let camera = self.camera.borrow();
        let Some(camera) = camera.as_ref() else {
            return;
        };
        let path = photos_dir().join(format!("Gaze {}.jpg", timestamp()));
        self.flash();
        let this = self.clone();
        let photo = camera.take_photo(path);
        glib::spawn_future_local(async move {
            match photo.await {
                Ok((path, thumb)) => {
                    this.add_capture(path.clone(), thumb, false);
                    this.notify_capture(title, &path);
                    this.toast_file(title, path);
                }
                Err(error) => this.ui.toasts.add_toast(adw::Toast::new(&format!(
                    "Could not save the photo: {error}"
                ))),
            }
        });
    }

    /// Tap the open camera for motion detection exactly while it is on.
    fn sync_motion_tap(&self) {
        let camera = self.camera.borrow();
        let Some(camera) = camera.as_ref() else {
            self.motion_tap.set(None);
            return;
        };
        match (self.motion_on.get(), self.motion_tap.get()) {
            (true, None) => {
                let frames = self.motion_frames.clone();
                let tap = Tap {
                    max_width: Some(320),
                    fps: 5,
                };
                let id = camera.add_tap(tap, move |frame| frames.store(frame));
                self.motion_tap.set(Some(id));
            }
            (false, Some(id)) => {
                camera.remove_tap(id);
                self.motion_tap.set(None);
            }
            _ => {}
        }
    }

    /// Compare the newest motion frame with the last one; a photo when
    /// enough changed, at most one every few seconds.
    fn check_motion(self: &Rc<Self>) {
        let Some(frame) = self.motion_frames.take() else {
            return;
        };
        let moved = self
            .motion_prev
            .borrow()
            .as_ref()
            .map_or(0.0, |prev| motion::changed(prev, &frame));
        self.motion_prev.replace(Some(frame));
        let cooling = self
            .motion_last_shot
            .get()
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        let busy = self.counting_down()
            || self.frozen.get()
            || self
                .camera
                .borrow()
                .as_ref()
                .is_some_and(Camera::is_recording);
        if moved >= motion::MOTION && !cooling && !busy {
            self.motion_last_shot.set(Some(Instant::now()));
            self.snap_photo("Motion: photo saved");
        }
    }

    /// Remember a capture: the thumbnail button shows it and the gallery
    /// lists it.
    fn add_capture(&self, path: PathBuf, thumb: Option<camera::Frame>, video: bool) {
        let paintable: gdk::Paintable = match thumb {
            Some(frame) => {
                let bytes = glib::Bytes::from(&frame.rgb);
                gdk::MemoryTexture::new(
                    frame.width as i32,
                    frame.height as i32,
                    gdk::MemoryFormat::R8g8b8,
                    &bytes,
                    frame.stride,
                )
                .upcast()
            }
            None => gtk::IconTheme::for_display(&WidgetExt::display(&self.ui.window))
                .lookup_icon(
                    if video {
                        "video-x-generic-symbolic"
                    } else {
                        "image-x-generic-symbolic"
                    },
                    &[],
                    64,
                    1,
                    gtk::TextDirection::None,
                    gtk::IconLookupFlags::empty(),
                )
                .upcast(),
        };
        self.ui.thumb_picture.set_paintable(Some(&paintable));
        self.ui.thumb_button.set_visible(true);
        self.ui.thumb_button.set_tooltip_text(Some(if video {
            "Last Video"
        } else {
            "Last Photo"
        }));
        let tile = gallery_tile(&paintable, &path, video);
        self.ui.gallery.insert(&tile, 0);
        let mut captures = self.captures.borrow_mut();
        captures.insert(0, Capture { path, video });
        while captures.len() > GALLERY_SIZE {
            captures.pop();
            if let Some(last) = self.ui.gallery.child_at_index(GALLERY_SIZE as i32) {
                self.ui.gallery.remove(&last);
            }
        }
    }

    /// Put the newest photo on the clipboard as an image.
    fn copy_last_photo(&self) {
        let path = self
            .captures
            .borrow()
            .iter()
            .find(|c| !c.video)
            .map(|c| c.path.clone());
        let Some(path) = path else {
            self.ui.toasts.add_toast(adw::Toast::new("No photo yet"));
            return;
        };
        match gdk::Texture::from_filename(&path) {
            Ok(texture) => {
                self.ui.window.clipboard().set_texture(&texture);
                self.ui.toasts.add_toast(adw::Toast::new("Photo copied"));
            }
            Err(error) => self
                .ui
                .toasts
                .add_toast(adw::Toast::new(&format!("Could not copy: {error}"))),
        }
    }

    /// A desktop notification for a capture made while Gaze is not the
    /// focused window (the self-timer or a script took it).
    fn notify_capture(&self, title: &str, path: &std::path::Path) {
        if self.ui.window.is_active() {
            return;
        }
        let Some(app) = self.ui.window.application() else {
            return;
        };
        let notification = gio::Notification::new(title);
        if let Some(name) = path.file_name() {
            notification.set_body(Some(&name.to_string_lossy()));
        }
        notification.set_icon(&gio::FileIcon::new(&gio::File::for_path(path)));
        notification.set_default_action_and_target_value(
            "app.open-path",
            Some(&path.to_string_lossy().to_variant()),
        );
        app.send_notification(Some("capture"), &notification);
    }

    /// Photos every quarter second while the shutter is held.
    fn start_burst(self: &Rc<Self>) {
        if self.burst.borrow().is_some() {
            return;
        }
        self.burst_count.set(0);
        self.burst_shot();
        let source = glib::timeout_add_local(
            Duration::from_millis(250),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    this.burst_shot();
                    glib::ControlFlow::Continue
                }
            ),
        );
        self.burst.replace(Some(source));
        self.ui.timer.set_visible(true);
    }

    fn burst_shot(self: &Rc<Self>) {
        let n = self.burst_count.get() + 1;
        self.burst_count.set(n);
        self.ui.timer.set_label(&format!("{n} photos"));
        let camera = self.camera.borrow();
        let Some(camera) = camera.as_ref() else {
            return;
        };
        let path = photos_dir().join(format!("Gaze {} {n:02}.jpg", timestamp()));
        let photo = camera.take_photo(path);
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok((path, thumb)) = photo.await {
                this.add_capture(path, thumb, false);
            }
        });
    }

    fn stop_burst(&self) {
        let Some(source) = self.burst.take() else {
            return;
        };
        source.remove();
        self.ui.timer.set_visible(false);
        self.ui.timer.set_label("00:00");
        let n = self.burst_count.get();
        self.ui.toasts.add_toast(adw::Toast::new(&format!(
            "{n} photo{} saved",
            if n == 1 { "" } else { "s" }
        )));
    }

    fn set_effect(&self, effect: Option<&'static Effect>) {
        if let Some(camera) = self.camera.borrow().as_ref() {
            if let Err(error) = camera.set_effect(effect) {
                self.ui.toasts.add_toast(adw::Toast::new(&error));
                return;
            }
        }
        self.effect.set(effect);
        self.apply_effect_state();
    }

    fn apply_effect_state(&self) {
        let id = self.effect.get().map_or("none", |e| e.id);
        if let Some(action) = self.ui.window.lookup_action("effect") {
            if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                action.set_state(&id.to_variant());
            }
        }
    }

    fn apply_timer(&self) {
        let secs = self.timer_secs.get();
        if let Some(action) = self.ui.window.lookup_action("timer") {
            if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                action.set_state(&secs.to_string().to_variant());
            }
        }
        self.ui.timer_label.set_visible(secs > 0);
        self.ui.timer_label.set_label(&format!("{secs} s"));
    }

    /// Count `secs` down on the overlay, then capture.
    fn start_countdown(self: &Rc<Self>, secs: u32) {
        self.cancel_countdown();
        self.countdown_left.set(secs);
        self.show_countdown();
        let source = glib::timeout_add_local(
            Duration::from_secs(1),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    let left = this.countdown_left.get().saturating_sub(1);
                    this.countdown_left.set(left);
                    if left > 0 {
                        this.show_countdown();
                        return glib::ControlFlow::Continue;
                    }
                    this.countdown.replace(None);
                    this.ui.countdown.set_visible(false);
                    this.update_shutter();
                    this.capture();
                    glib::ControlFlow::Break
                }
            ),
        );
        self.countdown.replace(Some(source));
        self.update_shutter();
    }

    fn show_countdown(&self) {
        let label = &self.ui.countdown;
        label.set_label(&self.countdown_left.get().to_string());
        label.set_visible(true);
        // Each second lands bright and fades towards the next.
        label.set_opacity(1.0);
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak]
            label,
            move |value| label.set_opacity(value)
        ));
        let animation = adw::TimedAnimation::new(label, 1.0, 0.35, 900, target);
        animation.set_easing(adw::Easing::EaseInCubic);
        animation.play();
    }

    fn counting_down(&self) -> bool {
        self.countdown.borrow().is_some()
    }

    fn cancel_countdown(&self) {
        if let Some(source) = self.countdown.take() {
            source.remove();
            self.ui.countdown.set_visible(false);
            self.update_shutter();
        }
    }

    fn on_event(self: &Rc<Self>, event: Event) {
        match event {
            Event::Error(text) => {
                if let Some(camera) = self.camera.take() {
                    camera.close();
                }
                self.recording_since.set(None);
                self.update_shutter();
                self.show_status("Camera Stopped", &text);
            }
            Event::RecordingSaved(path) => {
                if let Some(camera) = self.camera.borrow().as_ref() {
                    camera.recording_finished();
                }
                self.recording_since.set(None);
                self.update_shutter();
                let thumb = self.recording_thumb.take();
                self.add_capture(path.clone(), thumb, true);
                self.notify_capture("Video saved", &path);
                self.toast_file("Video saved", path);
            }
            Event::AudioLevel(level) => {
                // Hold peaks a little so the meter is readable.
                let shown = self.ui.level.value();
                self.ui.level.set_value(level.max(shown * 0.8));
            }
            Event::OutputFailed(text) => {
                if let Some(camera) = self.camera.borrow().as_ref() {
                    camera.stop_output();
                }
                self.set_virtual_camera(false);
                self.ui
                    .toasts
                    .add_toast(adw::Toast::new(&format!("Virtual camera stopped: {text}")));
            }
            Event::RecordingFailed(text) => {
                if let Some(camera) = self.camera.borrow().as_ref() {
                    camera.abort_recording();
                }
                self.recording_since.set(None);
                self.update_shutter();
                self.ui
                    .toasts
                    .add_toast(adw::Toast::new(&format!("Recording failed: {text}")));
            }
        }
    }

    /// The shutter button: cancels a countdown, stops a recording, or starts
    /// the self-timer or the capture itself.
    fn shutter(self: &Rc<Self>) {
        if self.counting_down() {
            self.cancel_countdown();
            return;
        }
        if self.frozen.get() || self.reading.get() {
            self.unfreeze();
            return;
        }
        let recording = self
            .camera
            .borrow()
            .as_ref()
            .is_some_and(Camera::is_recording)
            || self.gif.borrow().is_some();
        let secs = self.timer_secs.get();
        if secs > 0 && !recording && self.camera.borrow().is_some() {
            self.start_countdown(secs);
        } else {
            self.capture();
        }
    }

    fn capture(self: &Rc<Self>) {
        let camera = self.camera.borrow();
        let Some(camera) = camera.as_ref() else {
            return;
        };
        match self.mode.get() {
            Mode::Photo => self.snap_photo("Photo saved"),
            Mode::Gif => {
                if self.gif.borrow().is_some() {
                    self.finish_gif();
                } else {
                    self.gif.replace(Some(GifCapture::start(camera)));
                    self.gif_since.set(Some(Instant::now()));
                    self.ui.timer.set_label("GIF 00:00");
                    glib::timeout_add_local_once(
                        Duration::from_secs(gifcap::MAX_SECONDS),
                        glib::clone!(
                            #[weak(rename_to = this)]
                            self,
                            move || {
                                if this.gif_since.get().is_some_and(|s| {
                                    s.elapsed() >= Duration::from_secs(gifcap::MAX_SECONDS)
                                }) {
                                    this.finish_gif();
                                }
                            }
                        ),
                    );
                }
                self.update_shutter();
            }
            Mode::Video => {
                if camera.is_recording() {
                    if self.recording_since.get().is_some() {
                        camera.stop_recording();
                        self.recording_since.set(None);
                        self.ui.timer.set_label("Saving…");
                    }
                } else {
                    let path = videos_dir().join(format!("Gaze {}.mp4", timestamp()));
                    if let Err(error) = std::fs::create_dir_all(videos_dir()) {
                        self.ui
                            .toasts
                            .add_toast(adw::Toast::new(&error.to_string()));
                        return;
                    }
                    let mic = self.settings.borrow().mic.clone();
                    match camera.start_recording(&path, mic.as_deref(), self.lapse.get()) {
                        Ok(()) => {
                            self.recording_since.set(Some(Instant::now()));
                            self.paused_total.set(Duration::ZERO);
                            self.paused_at.set(None);
                            self.ui.pause_button.set_active(false);
                            let snapshot = camera.snapshot();
                            let this = self.clone();
                            glib::spawn_future_local(async move {
                                this.recording_thumb.replace(snapshot.await);
                            });
                        }
                        Err(error) => self.ui.toasts.add_toast(adw::Toast::new(&error)),
                    }
                }
                self.update_shutter();
            }
        }
    }

    fn update_shutter(&self) {
        let recording = self
            .camera
            .borrow()
            .as_ref()
            .is_some_and(Camera::is_recording);
        let capturing_gif = self.gif.borrow().is_some();
        let counting = self.counting_down();
        let shutter = &self.ui.shutter;
        for class in ["photo", "video", "gif", "recording"] {
            shutter.remove_css_class(class);
        }
        match (self.mode.get(), recording, capturing_gif) {
            (_, true, _) => {
                shutter.add_css_class("recording");
                shutter.set_tooltip_text(Some("Stop Recording"));
            }
            (_, _, true) => {
                shutter.add_css_class("gif");
                shutter.add_css_class("recording");
                shutter.set_tooltip_text(Some("Finish GIF"));
            }
            (Mode::Video, ..) => {
                shutter.add_css_class("video");
                shutter.set_tooltip_text(Some("Start Recording"));
            }
            (Mode::Gif, ..) => {
                shutter.add_css_class("gif");
                shutter.set_tooltip_text(Some("Capture GIF"));
            }
            (Mode::Photo, ..) => {
                shutter.add_css_class("photo");
                shutter.set_tooltip_text(Some("Take Photo"));
            }
        }
        if counting {
            shutter.set_tooltip_text(Some("Cancel Timer"));
        }
        self.ui.timer.set_visible(recording || capturing_gif);
        self.ui.pause_button.set_visible(recording);
        self.ui.level.set_visible(recording);
        let recording = recording || capturing_gif;
        if !recording {
            self.ui.level.set_value(0.0);
            self.ui.pause_button.set_active(false);
        }
        self.ui.mode_toggle.set_sensitive(!recording && !counting);
        self.ui
            .cameras_dropdown
            .set_sensitive(!recording && !counting);
        if !recording {
            self.ui.timer.set_label("00:00");
        }
    }

    fn flash(&self) {
        let flash = &self.ui.flash;
        flash.set_opacity(0.8);
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak]
            flash,
            move |value| flash.set_opacity(value)
        ));
        adw::TimedAnimation::new(flash, 0.8, 0.0, 250, target).play();
    }

    fn tick(self: &Rc<Self>) {
        if let Some(since) = self.recording_since.get() {
            let paused = self.paused_total.get()
                + self
                    .paused_at
                    .get()
                    .map_or(Duration::ZERO, |at| at.elapsed());
            let s = since.elapsed().saturating_sub(paused).as_secs();
            let lapse = self.lapse.get();
            self.ui.timer.set_label(&if lapse > 1 {
                format!("{lapse}× {:02}:{:02}", s / 60, s % 60)
            } else {
                format!("{:02}:{:02}", s / 60, s % 60)
            });
        }
        if let Some(since) = self.gif_since.get() {
            let s = since.elapsed().as_secs();
            self.ui
                .timer
                .set_label(&format!("GIF {:02}:{:02}", s / 60, s % 60));
        }
        if self.motion_on.get() {
            self.check_motion();
        }
        if !self.stats_on.get() {
            return;
        }
        let Some(frame) = self.stats_frames.take() else {
            return;
        };
        let (rgb, w, h, stride) = (frame.rgb, frame.width, frame.height, frame.stride);
        // The region is chosen on the displayed picture; the frame is not
        // mirrored.
        let region = self.region.get().map(|r| {
            if self.mirror.get() {
                Region {
                    x: 1.0 - r.x - r.w,
                    ..r
                }
            } else {
                r
            }
        });
        let s = stats::measure(&rgb, w, h, stride, region);
        let mut values = vec![
            s.black.to_string(),
            s.white.to_string(),
            format!("{:.1}", s.mean),
            format!("{:.1}", s.saturation),
        ];
        match s.region {
            Some([r, g, b]) => {
                values.push(format!("{r:.0}, {g:.0}, {b:.0}"));
                values.push(format!("{:.2}", r / b.max(1.0)));
                self.swatch_rgb.set([r, g, b]);
                self.ui.swatch.set_visible(true);
                self.ui.swatch.queue_draw();
            }
            None => {
                values.push("drag on the picture".into());
                values.push("–".into());
                self.ui.swatch.set_visible(false);
            }
        }
        for (label, value) in self.ui.stats_labels.iter().zip(values) {
            label.set_label(&value);
        }
    }

    /// The video's rectangle inside the picture widget (content-fit
    /// contain), in widget coordinates.
    fn video_rect(&self) -> Option<(f64, f64, f64, f64)> {
        let picture = &self.ui.picture;
        let (w, h) = (picture.width() as f64, picture.height() as f64);
        let aspect = picture.paintable()?.intrinsic_aspect_ratio();
        if w <= 0.0 || h <= 0.0 || aspect <= 0.0 {
            return None;
        }
        Some(if w / h > aspect {
            let vw = h * aspect;
            ((w - vw) / 2.0, 0.0, vw, h)
        } else {
            let vh = w / aspect;
            (0.0, (h - vh) / 2.0, w, vh)
        })
    }

    /// Widget coordinates to fractions of the displayed video.
    fn to_video(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (vx, vy, vw, vh) = self.video_rect()?;
        Some((
            ((x - vx) / vw).clamp(0.0, 1.0),
            ((y - vy) / vh).clamp(0.0, 1.0),
        ))
    }

    fn show_status(&self, title: &str, description: &str) {
        self.ui.status.set_title(title);
        self.ui.status.set_description(Some(description));
        self.ui.stack.set_visible_child_name("status");
    }

    fn toast_file(&self, title: &str, path: PathBuf) {
        let toast = adw::Toast::builder()
            .title(title)
            .button_label("Open")
            .timeout(4)
            .build();
        toast.connect_button_clicked(move |_| open_file(&path));
        self.ui.toasts.add_toast(toast);
    }
}

fn build_ui(app: &adw::Application) -> Ui {
    let cameras_dropdown = gtk::DropDown::builder().tooltip_text("Camera").build();
    let menu = gio::Menu::new();
    let formats_menu = gio::Menu::new();
    let picture_menu = gio::Menu::new();
    picture_menu.append_submenu(Some("Format"), &formats_menu);
    let mics_menu = gio::Menu::new();
    picture_menu.append_submenu(Some("Microphone"), &mics_menu);
    let lapse_menu = gio::Menu::new();
    for speed in LAPSES {
        let label = if speed == 1 {
            "Off".to_string()
        } else {
            format!("{speed}× faster")
        };
        let item = gio::MenuItem::new(Some(&label), None);
        item.set_action_and_target_value(Some("win.lapse"), Some(&speed.to_string().to_variant()));
        lapse_menu.append_item(&item);
    }
    picture_menu.append_submenu(Some("Time-lapse"), &lapse_menu);
    picture_menu.append(Some("Grid"), Some("win.grid"));
    picture_menu.append(Some("Fullscreen"), Some("win.fullscreen"));
    menu.append_section(None, &picture_menu);
    let view = gio::Menu::new();
    view.append(Some("Mirror Preview"), Some("win.mirror"));
    let background_menu = gio::Menu::new();
    for (label, id) in [("None", "none"), ("Blur", "blur"), ("Picture", "image")] {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some("win.background"), Some(&id.to_variant()));
        background_menu.append_item(&item);
    }
    let background_pick = gio::Menu::new();
    background_pick.append(Some("Omarchy Wallpaper"), Some("win.background-wallpaper"));
    background_pick.append(Some("Choose Picture…"), Some("win.choose-background"));
    background_menu.append_section(None, &background_pick);
    view.append_submenu(Some("Background"), &background_menu);
    view.append(Some("Compact Mirror"), Some("win.compact"));
    view.append(Some("Scan Codes"), Some("win.scan"));
    view.append(Some("Motion Capture"), Some("win.motion"));
    view.append(Some("Scan Document to PDF"), Some("win.scan-document"));
    view.append(Some("Virtual Camera"), Some("win.virtual-camera"));
    view.append(
        Some("Keep Virtual Camera After Reboot"),
        Some("win.virtual-camera-permanent"),
    );
    view.append(Some("Colour Statistics"), Some("win.stats"));
    menu.append_section(None, &view);
    let files = gio::Menu::new();
    files.append(Some("Open Photos Folder"), Some("win.open-photos"));
    files.append(Some("Open Videos Folder"), Some("win.open-videos"));
    files.append(Some("Refresh Cameras"), Some("win.refresh"));
    menu.append_section(None, &files);
    let about = gio::Menu::new();
    about.append(Some("About Gaze"), Some("win.about"));
    menu.append_section(None, &about);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .tooltip_text("Main Menu")
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(&cameras_dropdown)
        .build();
    header.pack_end(&menu_button);

    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .hexpand(true)
        .vexpand(true)
        .build();
    picture.add_css_class("viewfinder");
    let overlay_area = gtk::DrawingArea::builder().can_target(false).build();
    let countdown = gtk::Label::builder()
        .can_target(false)
        .visible(false)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    countdown.add_css_class("countdown");
    countdown.add_css_class("numeric");
    let flash = gtk::Box::builder().can_target(false).opacity(0.0).build();
    flash.add_css_class("flash");

    // Stats card
    let grid = gtk::Grid::builder()
        .column_spacing(16)
        .row_spacing(4)
        .build();
    let names = [
        "Black level",
        "White level",
        "Mean level",
        "Saturation",
        "Region R, G, B",
        "Region R / B",
    ];
    let mut stats_labels = Vec::new();
    for (row, name) in names.iter().enumerate() {
        let key = gtk::Label::builder().label(*name).xalign(0.0).build();
        key.add_css_class("dim-label");
        let value = gtk::Label::builder().label("–").xalign(1.0).build();
        value.add_css_class("numeric");
        grid.attach(&key, 0, row as i32, 1, 1);
        grid.attach(&value, 1, row as i32, 1, 1);
        stats_labels.push(value);
    }
    let swatch = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(16)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    grid.attach(&swatch, 2, 4, 1, 1);
    let stats_title = gtk::Label::builder().label("Colour").xalign(0.0).build();
    stats_title.add_css_class("heading");
    let stats_card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_top(12)
        .margin_end(12)
        .visible(false)
        .can_target(false)
        .build();
    stats_card.add_css_class("osd");
    stats_card.add_css_class("stats");
    stats_card.append(&stats_title);
    stats_card.append(&grid);

    // Code card: what the scanner found and what to do with it.
    let code_title = gtk::Label::builder()
        .label("Link")
        .xalign(0.0)
        .hexpand(true)
        .build();
    code_title.add_css_class("heading");
    let code_format = gtk::Label::builder().label("QR Code").xalign(0.0).build();
    code_format.add_css_class("dim-label");
    code_format.add_css_class("caption");
    let code_close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .action_name("win.close-code")
        .tooltip_text("Dismiss")
        .valign(gtk::Align::Start)
        .build();
    code_close.add_css_class("flat");
    code_close.add_css_class("circular");
    let code_titles = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    code_titles.append(&code_title);
    code_titles.append(&code_format);
    let code_header = gtk::Box::builder().spacing(8).build();
    code_header.append(&code_titles);
    code_header.append(&code_close);
    let code_text = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(44)
        .lines(5)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .selectable(true)
        .build();
    code_text.add_css_class("payload");
    let code_primary = gtk::Button::builder().label("Open").build();
    code_primary.add_css_class("suggested-action");
    code_primary.add_css_class("pill");
    let code_copy = gtk::Button::builder()
        .label("Copy")
        .action_name("win.copy-code")
        .build();
    code_copy.add_css_class("pill");
    let code_buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::Start)
        .build();
    code_buttons.append(&code_primary);
    code_buttons.append(&code_copy);
    let code_card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_top(12)
        .margin_start(12)
        .visible(false)
        .build();
    code_card.add_css_class("osd");
    code_card.add_css_class("code");
    code_card.append(&code_header);
    code_card.append(&code_text);
    code_card.append(&code_buttons);

    // Text mode: a layer for the chips and a small card with Copy All.
    let text_layer = gtk::Fixed::new();
    let text_title = gtk::Label::builder()
        .label("Reading…")
        .xalign(0.0)
        .hexpand(true)
        .build();
    text_title.add_css_class("heading");
    let text_copy_all = gtk::Button::builder()
        .label("Copy All")
        .action_name("win.copy-all-text")
        .build();
    text_copy_all.add_css_class("pill");
    text_copy_all.add_css_class("suggested-action");
    let text_done = gtk::Button::builder()
        .label("Done")
        .action_name("win.read-text")
        .build();
    text_done.add_css_class("pill");
    let text_buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::Start)
        .build();
    text_buttons.append(&text_copy_all);
    text_buttons.append(&text_done);
    let text_card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_top(12)
        .margin_start(12)
        .visible(false)
        .build();
    text_card.add_css_class("osd");
    text_card.add_css_class("code");
    text_card.append(&text_title);
    text_card.append(&text_buttons);

    // Controls
    let mode_toggle = adw::ToggleGroup::new();
    mode_toggle.add(
        adw::Toggle::builder()
            .name("photo")
            .icon_name("camera-photo-symbolic")
            .tooltip("Photo")
            .build(),
    );
    mode_toggle.add(
        adw::Toggle::builder()
            .name("video")
            .icon_name("camera-video-symbolic")
            .tooltip("Video")
            .build(),
    );
    mode_toggle.add(
        adw::Toggle::builder()
            .name("gif")
            .icon_name("media-playlist-repeat-symbolic")
            .tooltip("GIF")
            .build(),
    );
    mode_toggle.set_active_name(Some("photo"));
    mode_toggle.add_css_class("osd");
    mode_toggle.add_css_class("round");

    // Self-timer picker: a radio menu over win.timer.
    let timer_menu = gio::Menu::new();
    for secs in TIMERS {
        let label = match secs {
            0 => "Off".to_string(),
            s => format!("{s} seconds"),
        };
        let item = gio::MenuItem::new(Some(&label), None);
        item.set_action_and_target_value(Some("win.timer"), Some(&secs.to_string().to_variant()));
        timer_menu.append_item(&item);
    }
    let timer_label = gtk::Label::builder().label("0 s").visible(false).build();
    timer_label.add_css_class("numeric");
    let timer_content = gtk::Box::builder().spacing(6).build();
    timer_content.append(&gtk::Image::from_icon_name("alarm-symbolic"));
    timer_content.append(&timer_label);
    let timer_button = gtk::MenuButton::builder()
        .child(&timer_content)
        .menu_model(&timer_menu)
        .tooltip_text("Self-Timer")
        .valign(gtk::Align::Center)
        .build();
    timer_button.add_css_class("osd");
    timer_button.add_css_class("pill");
    timer_button.add_css_class("self-timer");
    let inner = gtk::Box::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    inner.add_css_class("inner");
    let shutter = gtk::Button::builder()
        .tooltip_text("Take Photo")
        .child(&inner)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    shutter.add_css_class("shutter");
    shutter.add_css_class("photo");
    let timer = gtk::Label::builder().label("00:00").visible(false).build();
    timer.add_css_class("timer");
    timer.add_css_class("osd");
    timer.add_css_class("numeric");
    let controls = gtk::CenterBox::builder()
        .valign(gtk::Align::End)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let left = gtk::Box::builder()
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .spacing(10)
        .build();
    left.append(&mode_toggle);
    left.append(&timer_button);
    let (effects_button, effect_pictures) = effects_button();
    left.append(&effects_button);
    let controls_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(10)
        .margin_end(10)
        .width_request(380)
        .build();
    let controls_popover = gtk::Popover::builder().child(&controls_box).build();
    let controls_button = gtk::MenuButton::builder()
        .icon_name("emblem-system-symbolic")
        .popover(&controls_popover)
        .tooltip_text("Camera Controls")
        .valign(gtk::Align::Center)
        .build();
    controls_button.add_css_class("osd");
    controls_button.add_css_class("circular");
    left.append(&controls_button);
    controls.set_start_widget(Some(&left));
    controls.set_center_widget(Some(&shutter));
    let scan_button = gtk::ToggleButton::builder()
        .icon_name("scanner-symbolic")
        .action_name("win.scan")
        .tooltip_text("Scan Codes")
        .valign(gtk::Align::Center)
        .build();
    scan_button.add_css_class("osd");
    scan_button.add_css_class("circular");
    // Last capture: a thumbnail that opens the session's gallery.
    // A picture wants its texture's size; allocating it as an overlay over
    // a fixed 44 px box, clipped, keeps the button small whatever it shows.
    let thumb_picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .build();
    let thumb_frame = gtk::Overlay::builder()
        .child(
            &gtk::Box::builder()
                .width_request(44)
                .height_request(44)
                .build(),
        )
        .overflow(gtk::Overflow::Hidden)
        .build();
    thumb_frame.add_css_class("thumb-frame");
    thumb_frame.add_overlay(&thumb_picture);
    thumb_frame.set_clip_overlay(&thumb_picture, true);
    let gallery = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(4)
        .min_children_per_line(2)
        .column_spacing(4)
        .row_spacing(4)
        .homogeneous(true)
        .activate_on_single_click(true)
        .build();
    gallery.add_css_class("gallery");
    let gallery_scroller = gtk::ScrolledWindow::builder()
        .child(&gallery)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(420)
        .build();
    let copy_last = gtk::Button::builder()
        .label("Copy Photo")
        .action_name("win.copy-last")
        .build();
    copy_last.add_css_class("pill");
    let open_photos = gtk::Button::builder()
        .label("Open Folder")
        .action_name("win.open-photos")
        .build();
    open_photos.add_css_class("pill");
    let gallery_buttons = gtk::Box::builder().spacing(8).build();
    gallery_buttons.append(&copy_last);
    gallery_buttons.append(&open_photos);
    let gallery_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    gallery_box.append(&gallery_scroller);
    gallery_box.append(&gallery_buttons);
    let gallery_popover = gtk::Popover::builder().child(&gallery_box).build();
    let thumb_button = gtk::MenuButton::builder()
        .child(&thumb_frame)
        .popover(&gallery_popover)
        .tooltip_text("Last Photo")
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    thumb_button.add_css_class("osd");
    thumb_button.add_css_class("thumb");

    let right = gtk::Box::builder()
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .spacing(10)
        .build();
    let read_text = gtk::ToggleButton::builder()
        .icon_name("font-x-generic-symbolic")
        .tooltip_text("Read Text")
        .valign(gtk::Align::Center)
        .build();
    read_text.add_css_class("osd");
    read_text.add_css_class("circular");
    read_text.set_action_name(Some("win.read-text"));
    let pause_button = gtk::ToggleButton::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text("Pause Recording")
        .action_name("win.pause")
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    pause_button.add_css_class("osd");
    pause_button.add_css_class("circular");
    let level = gtk::LevelBar::builder()
        .min_value(0.0)
        .max_value(1.0)
        .width_request(72)
        .valign(gtk::Align::Center)
        .visible(false)
        .tooltip_text("Microphone level")
        .build();
    level.add_offset_value(gtk::LEVEL_BAR_OFFSET_LOW, 0.5);
    level.add_offset_value(gtk::LEVEL_BAR_OFFSET_HIGH, 0.85);
    level.add_offset_value(gtk::LEVEL_BAR_OFFSET_FULL, 0.97);
    level.add_css_class("mic-level");
    right.append(&level);
    right.append(&pause_button);
    right.append(&timer);
    right.append(&thumb_button);
    right.append(&read_text);
    right.append(&scan_button);
    controls.set_end_widget(Some(&right));

    let overlay = gtk::Overlay::builder().child(&picture).build();
    overlay.add_css_class("viewfinder-frame");
    overlay.add_overlay(&overlay_area);
    overlay.add_overlay(&countdown);
    overlay.add_overlay(&flash);
    overlay.add_overlay(&stats_card);
    overlay.add_overlay(&text_layer);
    overlay.add_overlay(&code_card);
    overlay.add_overlay(&text_card);
    overlay.add_overlay(&controls);
    let expand = gtk::Button::builder()
        .icon_name("view-restore-symbolic")
        .action_name("win.compact")
        .tooltip_text("Leave Compact Mirror")
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .margin_end(8)
        .margin_bottom(8)
        .visible(false)
        .build();
    expand.add_css_class("osd");
    expand.add_css_class("circular");
    overlay.add_overlay(&expand);

    let status = adw::StatusPage::builder()
        .icon_name("camera-disabled-symbolic")
        .title("Starting Camera")
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&overlay, Some("viewfinder"));
    stack.add_named(&status, Some("status"));
    stack.set_visible_child_name("status");

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let toolbar = adw::ToolbarView::builder().content(&toasts).build();
    toolbar.add_top_bar(&header);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Gaze")
        .default_width(1100)
        .default_height(720)
        .content(&toolbar)
        .build();

    Ui {
        window,
        toasts,
        stack,
        status,
        picture,
        overlay: overlay_area,
        countdown,
        timer_label,
        stats_card,
        code_card,
        code_title,
        code_format,
        code_text,
        code_primary,
        text_layer,
        text_card,
        text_title,
        read_text,
        stats_labels,
        swatch,
        cameras_dropdown,
        shutter,
        mode_toggle,
        timer,
        flash,
        controls,
        expand,
        toolbar,
        formats_menu,
        mics_menu,
        pause_button,
        level,
        thumb_button,
        thumb_picture,
        gallery,
        controls_box,
        controls_button,
        effects_button,
        effect_pictures,
    }
}

/// A gallery entry: the thumbnail, a play badge for videos.
fn gallery_tile(paintable: &gdk::Paintable, path: &std::path::Path, video: bool) -> gtk::Widget {
    let picture = gtk::Picture::builder()
        .paintable(paintable)
        .content_fit(gtk::ContentFit::Cover)
        .width_request(128)
        .height_request(72)
        .build();
    let frame = gtk::Box::builder().overflow(gtk::Overflow::Hidden).build();
    frame.add_css_class("thumb-frame");
    frame.append(&picture);
    let overlay = gtk::Overlay::builder().child(&frame).build();
    if video {
        let badge = gtk::Image::builder()
            .icon_name("media-playback-start-symbolic")
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .pixel_size(24)
            .build();
        badge.add_css_class("osd");
        badge.add_css_class("circular");
        overlay.add_overlay(&badge);
    }
    let button = gtk::Button::builder()
        .child(&overlay)
        .tooltip_text(
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        )
        .build();
    button.add_css_class("flat");
    button.set_can_focus(false);
    // The flow box child is what gets activated; the button is for looks.
    button.set_can_target(false);
    button.upcast()
}

fn open_file(path: &std::path::Path) {
    let uri = gio::File::for_path(path).uri();
    let _ = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
}

fn special_dir(dir: glib::UserDirectory, fallback: &str) -> PathBuf {
    glib::user_special_dir(dir).unwrap_or_else(|| glib::home_dir().join(fallback))
}

pub fn photos_dir() -> PathBuf {
    special_dir(glib::UserDirectory::Pictures, "Pictures").join("Camera")
}

pub fn videos_dir() -> PathBuf {
    special_dir(glib::UserDirectory::Videos, "Videos").join("Camera")
}

/// Run a command to completion; its stderr is the error.
async fn run(argv: &[&str]) -> Result<(), String> {
    let process = gio::Subprocess::newv(
        &argv
            .iter()
            .map(|a| a.as_ref())
            .collect::<Vec<&std::ffi::OsStr>>(),
        gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE,
    )
    .map_err(|e| e.to_string())?;
    let (_, stderr) = process
        .communicate_utf8_future(None)
        .await
        .map_err(|e| e.to_string())?;
    if process.is_successful() {
        Ok(())
    } else {
        let stderr = stderr.map(|s| s.to_string()).unwrap_or_default();
        Err(stderr.trim().lines().last().unwrap_or("failed").to_string())
    }
}

/// The effects picker: a popover with a grid of live tiles (radio buttons
/// over win.effect), distortions first, then colours, like Photo Booth's
/// pages. Returns the button and each tile's picture by effect id.
fn effects_button() -> (gtk::MenuButton, Vec<(&'static str, gtk::Picture)>) {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    let mut pictures = Vec::new();
    let mut tile = |label: &str, id: &'static str| {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .build();
        let frame = gtk::Overlay::builder()
            .child(
                &gtk::Box::builder()
                    .width_request(112)
                    .height_request(63)
                    .build(),
            )
            .overflow(gtk::Overflow::Hidden)
            .halign(gtk::Align::Center)
            .build();
        frame.add_css_class("tile-frame");
        frame.add_overlay(&picture);
        frame.set_clip_overlay(&picture, true);
        let inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .build();
        inner.append(&frame);
        inner.append(&gtk::Label::new(Some(label)));
        let button = gtk::ToggleButton::builder()
            .child(&inner)
            .action_name("win.effect")
            .action_target(&id.to_variant())
            .build();
        button.add_css_class("flat");
        pictures.push((id, picture));
        button
    };
    let none = tile("Normal", "none");
    none.set_halign(gtk::Align::Center);
    content.append(&none);
    for (title, distorts) in [("Distort", true), ("Colour", false)] {
        let heading = gtk::Label::builder().label(title).xalign(0.0).build();
        heading.add_css_class("heading");
        heading.add_css_class("dim-label");
        content.append(&heading);
        let grid = gtk::Grid::builder()
            .row_spacing(4)
            .column_spacing(4)
            .column_homogeneous(true)
            .build();
        for (i, effect) in EFFECTS
            .iter()
            .filter(|e| e.distorts == distorts)
            .enumerate()
        {
            grid.attach(
                &tile(effect.name, effect.id),
                (i % 3) as i32,
                (i / 3) as i32,
                1,
                1,
            );
        }
        content.append(&grid);
    }
    let popover = gtk::Popover::builder().child(&content).build();
    popover.add_css_class("effects");
    let button = gtk::MenuButton::builder()
        .icon_name("color-select-symbolic")
        .popover(&popover)
        .tooltip_text("Effects")
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("osd");
    button.add_css_class("circular");
    (button, pictures)
}

/// Menu radio state for a backdrop setting.
fn background_state(setting: &str) -> &'static str {
    match setting {
        "none" => "none",
        "blur" => "blur",
        _ => "image",
    }
}

/// Omarchy's current wallpaper, if this is an Omarchy desktop.
fn omarchy_wallpaper() -> Option<PathBuf> {
    let link = glib::home_dir().join(".local/state/omarchy/current/background");
    std::fs::canonicalize(link).ok().filter(|p| p.is_file())
}

/// A picture file as a packed RGB frame, at most 1920 px wide.
fn load_picture(path: &std::path::Path) -> Result<camera::Frame, String> {
    let pixbuf =
        gtk::gdk_pixbuf::Pixbuf::from_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    let pixbuf = if pixbuf.width() > 1920 {
        let h = (pixbuf.height() as i64 * 1920 / pixbuf.width() as i64).max(1) as i32;
        pixbuf
            .scale_simple(1920, h, gtk::gdk_pixbuf::InterpType::Bilinear)
            .ok_or("cannot scale the picture")?
    } else {
        pixbuf
    };
    let (w, h) = (pixbuf.width() as usize, pixbuf.height() as usize);
    let channels = pixbuf.n_channels() as usize;
    let stride = pixbuf.rowstride() as usize;
    let pixels = pixbuf.read_pixel_bytes();
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x * channels;
            rgb.extend_from_slice(&pixels[i..i + 3]);
        }
    }
    Ok(camera::Frame {
        rgb,
        width: w,
        height: h,
        stride: w * 3,
    })
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// The name and a line or two of a vCard, for the card.
fn summarize_vcard(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let (key, value) = line.split_once(':').unwrap_or((line, ""));
        let key = key.split(';').next().unwrap_or("").to_ascii_uppercase();
        match key.as_str() {
            "FN" | "ORG" | "TEL" | "EMAIL" => lines.push(value.trim().to_string()),
            _ => {}
        }
    }
    if lines.is_empty() {
        text.to_string()
    } else {
        lines.join("\n")
    }
}

/// The summary, date and place of an iCalendar event, for the card.
fn summarize_ical(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let (key, value) = line.split_once(':').unwrap_or((line, ""));
        let key = key.split(';').next().unwrap_or("").to_ascii_uppercase();
        match key.as_str() {
            "SUMMARY" | "DTSTART" | "LOCATION" => lines.push(value.trim().to_string()),
            _ => {}
        }
    }
    if lines.is_empty() {
        text.to_string()
    } else {
        lines.join("\n")
    }
}

fn timestamp() -> String {
    glib::DateTime::now_local()
        .and_then(|t| t.format("%Y-%m-%d %H-%M-%S"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "photo".into())
}
