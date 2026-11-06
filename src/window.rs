//! The main window: viewfinder, shutter, camera picker and the colour
//! statistics card.

use crate::camera::{self, Camera, CameraInfo, Event};
use crate::settings::Settings;
use crate::stats::{self, Region};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Photo,
    Video,
}

struct Ui {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    status: adw::StatusPage,
    picture: gtk::Picture,
    roi_area: gtk::DrawingArea,
    stats_card: gtk::Box,
    stats_labels: Vec<gtk::Label>,
    swatch: gtk::DrawingArea,
    cameras_dropdown: gtk::DropDown,
    shutter: gtk::Button,
    mode_toggle: adw::ToggleGroup,
    timer: gtk::Label,
    flash: gtk::Box,
}

pub struct Window {
    ui: Ui,
    settings: RefCell<Settings>,
    cameras: RefCell<Vec<CameraInfo>>,
    camera: RefCell<Option<Camera>>,
    mode: Cell<Mode>,
    mirror: Cell<bool>,
    stats_on: Cell<bool>,
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
        let this = Rc::new(Self {
            ui,
            mirror: Cell::new(settings.mirror),
            stats_on: Cell::new(false),
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
        this.refresh_cameras();
        this.apply_mirror();
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
        ui.mode_toggle.connect_active_name_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |toggle| {
                let mode = if toggle.active_name().as_deref() == Some("video") {
                    Mode::Video
                } else {
                    Mode::Photo
                };
                this.mode.set(mode);
                this.update_shutter();
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
                    this.ui.roi_area.queue_draw();
                }
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, dx, dy| {
                if this.drag_start.take().is_some() && dx.abs() < 4.0 && dy.abs() < 4.0 {
                    this.region.set(None);
                    this.ui.roi_area.queue_draw();
                }
            }
        ));
        ui.stack.add_controller(drag);

        ui.roi_area.set_draw_func(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, cr, _, _| {
                let (Some(r), Some((vx, vy, vw, vh))) = (this.region.get(), this.video_rect())
                else {
                    return;
                };
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
                cr.set_line_width(2.0);
                cr.set_dash(&[6.0, 4.0], 0.0);
                cr.rectangle(vx + r.x * vw, vy + r.y * vh, r.w * vw, r.h * vh);
                let _ = cr.stroke();
            }
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
                this.ui.roi_area.set_visible(on);
                if let Some(camera) = this.camera.borrow().as_ref() {
                    camera.set_stats(on);
                }
            }
        ));
        window.add_action(&stats);
        app.set_accels_for_action("win.stats", &["<Control>i"]);

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
        app.set_accels_for_action("window.close", &["<Control>w", "<Control>q"]);
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
        let names: Vec<&str> = cameras.iter().map(|c| c.name.as_str()).collect();
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
        if let Some(camera) = self.camera.take() {
            camera.close();
        }
        let index = self.ui.cameras_dropdown.selected() as usize;
        let cameras = self.cameras.borrow();
        let Some(info) = cameras.get(index) else {
            return;
        };
        let weak = Rc::downgrade(self);
        match Camera::open(info, move |event| {
            if let Some(this) = weak.upgrade() {
                this.on_event(event);
            }
        }) {
            Ok(camera) => {
                let paintable = camera.paintable();
                log::info!("opened {}", info.name);
                self.ui.picture.set_paintable(Some(&paintable));
                camera.set_stats(self.stats_on.get());
                self.camera.replace(Some(camera));
                self.ui.stack.set_visible_child_name("viewfinder");
                self.update_shutter();
            }
            Err(error) => self.show_status("Camera Unavailable", &error),
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
        self.ui.roi_area.queue_draw();
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
                self.toast_file("Video saved", path);
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

    fn shutter(self: &Rc<Self>) {
        let camera = self.camera.borrow();
        let Some(camera) = camera.as_ref() else {
            return;
        };
        match self.mode.get() {
            Mode::Photo => {
                let path = photos_dir().join(format!("Gaze {}.jpg", timestamp()));
                self.flash();
                let this = self.clone();
                let photo = camera.take_photo(path);
                glib::spawn_future_local(async move {
                    match photo.await {
                        Ok(path) => this.toast_file("Photo saved", path),
                        Err(error) => this.ui.toasts.add_toast(adw::Toast::new(&format!(
                            "Could not save the photo: {error}"
                        ))),
                    }
                });
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
                    match camera.start_recording(&path) {
                        Ok(()) => self.recording_since.set(Some(Instant::now())),
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
        let shutter = &self.ui.shutter;
        for class in ["photo", "video", "recording"] {
            shutter.remove_css_class(class);
        }
        match (self.mode.get(), recording) {
            (_, true) => {
                shutter.add_css_class("recording");
                shutter.set_tooltip_text(Some("Stop Recording"));
            }
            (Mode::Video, false) => {
                shutter.add_css_class("video");
                shutter.set_tooltip_text(Some("Start Recording"));
            }
            (Mode::Photo, false) => {
                shutter.add_css_class("photo");
                shutter.set_tooltip_text(Some("Take Photo"));
            }
        }
        self.ui.timer.set_visible(recording);
        self.ui.mode_toggle.set_sensitive(!recording);
        self.ui.cameras_dropdown.set_sensitive(!recording);
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

    fn tick(&self) {
        if let Some(since) = self.recording_since.get() {
            let s = since.elapsed().as_secs();
            self.ui
                .timer
                .set_label(&format!("{:02}:{:02}", s / 60, s % 60));
        }
        if !self.stats_on.get() {
            return;
        }
        let frame = self.camera.borrow().as_ref().and_then(Camera::stats_frame);
        let Some((rgb, w, h, stride)) = frame else {
            return;
        };
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
        toast.connect_button_clicked(move |_| {
            let uri = gio::File::for_path(&path).uri();
            let _ = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
        });
        self.ui.toasts.add_toast(toast);
    }
}

fn build_ui(app: &adw::Application) -> Ui {
    let cameras_dropdown = gtk::DropDown::builder().tooltip_text("Camera").build();
    let menu = gio::Menu::new();
    let view = gio::Menu::new();
    view.append(Some("Mirror Preview"), Some("win.mirror"));
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
    let roi_area = gtk::DrawingArea::builder()
        .can_target(false)
        .visible(false)
        .build();
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
    mode_toggle.set_active_name(Some("photo"));
    mode_toggle.add_css_class("osd");
    mode_toggle.add_css_class("round");
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
        .build();
    left.append(&mode_toggle);
    controls.set_start_widget(Some(&left));
    controls.set_center_widget(Some(&shutter));
    let right = gtk::Box::builder()
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .build();
    right.append(&timer);
    controls.set_end_widget(Some(&right));

    let overlay = gtk::Overlay::builder().child(&picture).build();
    overlay.add_css_class("viewfinder-frame");
    overlay.add_overlay(&roi_area);
    overlay.add_overlay(&flash);
    overlay.add_overlay(&stats_card);
    overlay.add_overlay(&controls);

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
        roi_area,
        stats_card,
        stats_labels,
        swatch,
        cameras_dropdown,
        shutter,
        mode_toggle,
        timer,
        flash,
    }
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

fn timestamp() -> String {
    glib::DateTime::now_local()
        .and_then(|t| t.format("%Y-%m-%d %H-%M-%S"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "photo".into())
}
