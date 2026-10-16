use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{
    self, Color32, FontId, Margin, Pos2, Rect, Response, RichText, Sense, Stroke, Vec2,
    ViewportCommand,
};

use crate::config::{OutputFormat, RecorderConfig, VideoCodec};
use crate::display::{
    get_available_displays, open_microphone_settings, open_screen_recording_settings,
    reset_screen_recording_permission, DisplayItem, DisplayQueryError,
};
use crate::recorder::{RecorderEvent, RecorderHandle};
use crate::tray::{TrayAction, TrayManager};
use crate::ui_kit::{
    button, kbd, logo_texture, section_label, theme, toggle_switch as settings_toggle,
    truncate_middle, ButtonKind,
};

#[derive(PartialEq, Eq)]
enum AppState {
    Idle,
    CountingDown(u32),
    Starting,
    Recording,
    Stopping,
    Finished,
    Error(String),
}

pub struct DexHowToApp {
    config: RecorderConfig,
    displays: Vec<DisplayItem>,
    display_error: Option<String>,
    permission_denied: bool,
    recorder: RecorderHandle,
    tray: Option<TrayManager>,
    state: AppState,
    settings_open: bool,
    settings_page: SettingsPage,
    pub editor: Option<crate::editor::VideoEditor>,

    // Toggles for dock view
    screen_enabled: bool,

    // Live recording stats
    active_display_label: String,
    current_output_path: PathBuf,
    recording_duration_secs: f64,
    recording_file_size_bytes: u64,

    // Last completed recording
    last_saved_path: Option<PathBuf>,
    last_telemetry_path: Option<PathBuf>,
    last_camera_path: Option<PathBuf>,
    /// Name (without number) for the recording in progress, from its start time
    pending_recording_base: Option<String>,
    last_saved_duration: f64,
    last_saved_size: u64,

    // One-time window resize flag
    did_initial_resize: bool,

    // Set once the minimized window has lost focus; see `recover_from_user_restore`
    minimized_lost_focus: bool,

    // While the dock is being dragged: mouse position minus window position, in screen points
    window_drag_offset: Option<Vec2>,

    // While recording, the dock window becomes a click-through overlay that shows
    // the countdown and live ⌘ arrows (left out of the recording)
    overlay: Option<crate::overlay::LiveOverlay>,
    dock_position_before_overlay: Option<Pos2>,
    // Saved preferences (Settings window) and the copy last written to disk
    prefs: crate::prefs::Preferences,
    saved_prefs: crate::prefs::Preferences,
}

impl DexHowToApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::platform::trace("Window created, setting up the app");
        // Transparent window styling for floating dock
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::TRANSPARENT;
        visuals.window_fill = Color32::TRANSPARENT;
        visuals.window_stroke = Stroke::NONE;
        visuals.faint_bg_color = Color32::from_rgb(38, 40, 48);
        visuals.extreme_bg_color = Color32::from_rgb(18, 19, 22);
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(26, 27, 32);
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(40, 42, 50);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(52, 55, 66);
        visuals.widgets.active.bg_fill = Color32::from_rgb(60, 64, 76);
        cc.egui_ctx.set_visuals(visuals);

        crate::platform::trace("Creating the tray icon");
        let tray = TrayManager::new();
        crate::platform::trace(if tray.is_some() { "Tray icon ready" } else { "No tray icon" });

        let prefs = crate::prefs::Preferences::load();

        let mut app = Self {
            config: prefs.recorder.clone(),
            displays: Vec::new(),
            display_error: None,
            permission_denied: false,
            recorder: RecorderHandle::new(),
            tray,
            state: AppState::Idle,
            settings_open: false,
            settings_page: SettingsPage::default(),
            editor: None,

            screen_enabled: true,

            active_display_label: String::new(),
            current_output_path: PathBuf::new(),
            recording_duration_secs: 0.0,
            recording_file_size_bytes: 0,

            last_saved_path: None,
            last_telemetry_path: None,
            last_camera_path: None,
            pending_recording_base: None,
            last_saved_duration: 0.0,
            last_saved_size: 0,
            did_initial_resize: false,
            minimized_lost_focus: false,
            window_drag_offset: None,
            overlay: None,
            dock_position_before_overlay: None,
            prefs: prefs.clone(),
            saved_prefs: prefs.clone(),
        };

        let cli_file = std::env::var("DEXHOWTO_OPEN_EDITOR_VIDEO")
            .ok()
            .or_else(|| std::env::args().nth(1).filter(|a| !a.starts_with("-psn")));
        if let Some(target) = cli_file {
            let p = PathBuf::from(target);
            if p.exists() {
                let (vid_opt, tel_opt) = crate::editor::resolve_recording_pair(&p);
                if let Some(vid) = vid_opt {
                    app.last_saved_path = Some(vid.clone());
                    app.last_telemetry_path = tel_opt.clone();
                    let mut ed = open_editor(&app.prefs, vid, tel_opt);
                    if std::env::var("DEXHOWTO_DEMO_CUTS").is_ok() && ed.cuts.is_empty() {
                        ed.add_cut_scene_at(2.5, 4.0);
                    }
                    app.editor = Some(ed);
                }
            }
        }

        app.refresh_displays();
        app
    }

    fn refresh_displays(&mut self) {
        match get_available_displays() {
            Ok(list) => {
                self.displays = list;
                self.display_error = None;
                self.permission_denied = false;

                if self.config.selected_display_id.is_none()
                    || !self
                        .displays
                        .iter()
                        .any(|d| Some(d.id) == self.config.selected_display_id)
                {
                    self.config.selected_display_id = self.displays.first().map(|d| d.id);
                }
            }
            Err(DisplayQueryError::PermissionDenied(msg)) => {
                self.displays.clear();
                self.display_error = Some(msg);
                self.permission_denied = true;
            }
            Err(DisplayQueryError::Other(msg)) => {
                self.displays.clear();
                self.display_error = Some(msg);
                self.permission_denied = false;
            }
        }
    }

    fn handle_recorder_events(&mut self, ctx: &egui::Context) {
        while let Some(event) = self.recorder.try_recv() {
            match event {
                RecorderEvent::CountdownTick { remaining_secs } => {
                    self.state = AppState::CountingDown(remaining_secs);
                    if let Some(ref tray) = self.tray {
                        tray.set_countdown(remaining_secs);
                    }
                    ctx.request_repaint();
                }
                RecorderEvent::CountdownCancelled => {
                    self.state = AppState::Idle;
                    if let Some(ref tray) = self.tray {
                        tray.set_idle();
                    }
                    self.show_window(ctx);
                }
                RecorderEvent::Started {
                    output_path,
                    display_label,
                    telemetry_path,
                } => {
                    self.state = AppState::Recording;
                    self.current_output_path = output_path;
                    self.last_telemetry_path = telemetry_path;
                    self.active_display_label = display_label;
                    self.recording_duration_secs = 0.0;
                    self.recording_file_size_bytes = 0;
                    if let Some(ref tray) = self.tray {
                        tray.set_recording("00:00");
                    }
                    ctx.request_repaint();
                }
                RecorderEvent::Tick {
                    duration_secs,
                    file_size_bytes,
                } => {
                    self.recording_duration_secs = duration_secs;
                    self.recording_file_size_bytes = file_size_bytes;
                    let dur_str = Self::format_duration(duration_secs);
                    if let Some(ref tray) = self.tray {
                        tray.set_recording(&dur_str);
                    }
                    ctx.request_repaint();
                }
                RecorderEvent::Stopped {
                    output_path,
                    telemetry_path,
                    camera_path,
                    duration_secs,
                    file_size_bytes,
                } => {
                    self.state = AppState::Finished;
                    self.last_saved_duration = duration_secs;
                    self.last_saved_size = file_size_bytes;

                    if let Some(ref tray) = self.tray {
                        tray.set_idle();
                    }

                    // The recording was written to a hidden temporary file. Give it its
                    // real name: the one the user picks, or the numbered default.
                    let base = self
                        .pending_recording_base
                        .take()
                        .unwrap_or_else(RecorderConfig::recording_base_name);
                    let default_dest = self.config.final_output_path(&base);
                    let dest = if self.config.ask_save_location {
                        self.show_window(ctx);
                        ask_where_to_save(&default_dest).unwrap_or(default_dest)
                    } else {
                        default_dest
                    };
                    let (output_path, telemetry_path, camera_path) =
                        match crate::recording_files::move_recording(
                            &output_path,
                            &dest,
                            telemetry_path.as_deref(),
                            camera_path.as_deref(),
                        ) {
                            Ok(moved) => (moved.video, moved.telemetry, moved.camera),
                            Err(e) => {
                                self.state = AppState::Error(format!(
                                    "Could not save the recording ({e}). It is in {}",
                                    output_path.display()
                                ));
                                (output_path, telemetry_path, camera_path)
                            }
                        };
                    self.last_camera_path = camera_path;
                    self.last_saved_path = Some(output_path.clone());
                    self.last_telemetry_path = telemetry_path.clone();

                    // Automatically open VideoEditor if enabled
                    if self.config.auto_open_editor {
                        self.editor = Some(open_editor(&self.prefs, 
                            output_path,
                            telemetry_path,
                        ));
                    }

                    // Restore window so user sees completed recording
                    self.show_window(ctx);
                }
                RecorderEvent::Error(err) => {
                    self.state = AppState::Error(err);
                    if let Some(ref tray) = self.tray {
                        tray.set_idle();
                    }
                    self.show_window(ctx);
                }
            }
        }
    }

    fn handle_tray_events(&mut self, ctx: &egui::Context) {
        let action = if let Some(ref tray) = self.tray {
            tray.check_events()
        } else {
            None
        };

        if let Some(action) = action {
            match action {
                TrayAction::ToggleRecord => match self.state {
                    AppState::Idle | AppState::Finished => {
                        self.start_countdown(ctx);
                    }
                    AppState::CountingDown(_) => {
                        self.cancel_countdown(ctx);
                    }
                    AppState::Recording => {
                        self.stop_recording();
                    }
                    _ => {}
                },
                TrayAction::ShowWindow => {
                    self.show_window(ctx);
                }
                TrayAction::OpenEditor => {
                    self.show_window(ctx);
                    let dialog = rfd::FileDialog::new()
                        .add_filter(
                            "Recorded Video & Telemetry (*.mp4, *.mov, *.jsonl)",
                            &["mp4", "mov", "jsonl"],
                        )
                        .add_filter("All Files", &["*"])
                        .set_directory(&self.config.output_dir);
                    if let Some(picked) = dialog.pick_file() {
                        let (vid_opt, tel_opt) = crate::editor::resolve_recording_pair(&picked);
                        if let Some(vid) = vid_opt {
                            self.last_saved_path = Some(vid.clone());
                            self.last_telemetry_path = tel_opt.clone();
                            self.editor = Some(open_editor(&self.prefs, vid, tel_opt));
                        }
                    }
                }
                TrayAction::OpenSettings => {
                    self.show_window(ctx);
                    self.settings_open = true;
                }
                TrayAction::Quit => {
                    self.exit_app(ctx);
                }
            }
        }
    }

    pub fn exit_app(&mut self, ctx: &egui::Context) {
        if self.state == AppState::Recording {
            self.stop_recording();
        }
        if let Some(ref tray) = self.tray {
            tray.set_idle();
        }
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn format_duration(seconds: f64) -> String {
        let total_secs = seconds.floor() as u64;
        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        let secs = total_secs % 60;
        if hours > 0 {
            format!("{:02}:{:02}:{:02}", hours, minutes, secs)
        } else {
            format!("{:02}:{:02}", minutes, secs)
        }
    }

    fn format_file_size(bytes: u64) -> String {
        const KB: f64 = 1024.0;
        const MB: f64 = KB * 1024.0;
        const GB: f64 = MB * 1024.0;

        let b = bytes as f64;
        if b >= GB {
            format!("{:.2} GB", b / GB)
        } else if b >= MB {
            format!("{:.1} MB", b / MB)
        } else if b >= KB {
            format!("{:.1} KB", b / KB)
        } else {
            format!("{} B", bytes)
        }
    }

    /// Starts the 5-second countdown in the background thread and minimizes the app into system tray
    fn start_countdown(&mut self, ctx: &egui::Context) {
        if self.displays.is_empty() {
            return;
        }
        if self.config.capture_microphone {
            crate::av_access::request(crate::av_access::Device::Microphone);
        }
        if self.config.capture_camera {
            crate::av_access::request(crate::av_access::Device::Camera);
        }

        // Record to a hidden temporary file; it gets its real name when it stops
        let target_path = self.config.temporary_recording_path();
        self.pending_recording_base = Some(RecorderConfig::recording_base_name());
        self.state = AppState::CountingDown(5);

        if let Some(ref tray) = self.tray {
            tray.set_countdown(5);
        }

        // Start countdown in worker thread
        self.recorder
            .start_with_countdown(self.config.clone(), target_path, 5);

        // Turn the dock into the on-screen overlay for the countdown and recording
        self.enter_overlay(ctx);
        ctx.request_repaint();
    }

    /// Turns the dock window into a transparent, click-through layer covering the
    /// display being recorded.
    fn enter_overlay(&mut self, ctx: &egui::Context) {
        let display_id = self
            .config
            .selected_display_id
            .or_else(|| self.displays.first().map(|d| d.id));
        let Some(display_id) = display_id else {
            return;
        };
        let (x, y, w, h) = crate::display::display_bounds_points(display_id);
        let display = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
        self.dock_position_before_overlay = ctx.input(|i| i.viewport().outer_rect).map(|r| r.min);
        self.overlay = Some(crate::overlay::LiveOverlay::new(display));
        crate::window::set_exclude_from_capture(!self.config.record_own_windows);
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        if !crate::window::OVERLAY_IN_DOCK {
            // Windows: the dock stays a dock; the overlay is a native window
            return;
        }
        ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(true));
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(display.min));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(display.size()));
    }

    /// Puts the dock back where it was before the overlay.
    fn exit_overlay(&mut self, ctx: &egui::Context) {
        if self.overlay.take().is_none() {
            return;
        }
        crate::window::restore_normal_window();
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
        if !crate::window::OVERLAY_IN_DOCK {
            self.dock_position_before_overlay = None;
            return;
        }
        ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(false));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(940.0, 210.0)));
        let position = self.dock_position_before_overlay.take().unwrap_or_else(|| {
            let (ox, oy, sw, sh) = crate::display::get_main_display_bounds_points();
            Pos2::new(ox + (sw - 940.0) / 2.0, oy + (sh - 210.0 - 115.0).max(20.0))
        });
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(position));
    }

    fn cancel_countdown(&mut self, ctx: &egui::Context) {
        self.recorder.cancel_countdown();
        self.state = AppState::Idle;
        if let Some(ref tray) = self.tray {
            tray.set_idle();
        }
        self.show_window(ctx);
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.exit_overlay(ctx);
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    /// On macOS, egui only clears its "minimized" flag when the app itself sends
    /// `Minimized(false)`. If the user restores the dock from the macOS Dock instead,
    /// the flag stays set, eframe keeps skipping the UI, and the window shows a frozen
    /// frame that ignores clicks. `logic` still runs in that state, so detect the
    /// restore (the window regains focus after having lost it) and clear the flag.
    fn recover_from_user_restore(&mut self, ctx: &egui::Context) {
        let (minimized, focused) =
            ctx.input(|i| (i.viewport().minimized, i.viewport().focused));
        if minimized != Some(true) {
            self.minimized_lost_focus = false;
            return;
        }
        match focused {
            Some(false) => self.minimized_lost_focus = true,
            Some(true) if self.minimized_lost_focus => {
                self.minimized_lost_focus = false;
                self.show_window(ctx);
                ctx.request_repaint();
            }
            _ => {}
        }
        // Keep polling while minimized so a restore is noticed even without other events
        ctx.request_repaint_after(Duration::from_millis(300));
    }

    /// Starts moving the dock when the mouse is pressed on `resp`.
    ///
    /// The window is moved by us, following the absolute screen position of the mouse.
    /// The native macOS drag (`ViewportCommand::StartDrag`) is not used: it needs the
    /// original mouse-down event, which is already gone by the time egui sends it, so
    /// it fails at random. Window-relative drag deltas are not used either, because
    /// they change as the window moves, which makes it jump.
    fn begin_window_drag(&mut self, ui: &egui::Ui, resp: &egui::Response) {
        let pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
        if !pressed || !resp.is_pointer_button_down_on() {
            return;
        }
        let window = ui.ctx().input(|i| i.viewport().outer_rect);
        if let (Some(window), Some((mx, my))) = (window, crate::display::global_mouse_position()) {
            self.window_drag_offset = Some(Vec2::new(mx, my) - window.min.to_vec2());
        }
    }

    /// Keeps the dock under the mouse while a drag started by `begin_window_drag` lasts.
    fn update_window_drag(&mut self, ctx: &egui::Context) {
        let Some(offset) = self.window_drag_offset else {
            return;
        };
        if !crate::display::left_mouse_button_down() {
            self.window_drag_offset = None;
            return;
        }
        if let Some((mx, my)) = crate::display::global_mouse_position() {
            let target = Pos2::new(mx, my) - offset;
            let current = ctx.input(|i| i.viewport().outer_rect).map(|r| r.min);
            if current.is_none_or(|c| (c - target).length() > 0.5) {
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(target));
            }
        }
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        // Keep frames coming while the mouse moves, even if the cursor leaves the window
        ctx.request_repaint();
    }

    /// Saves Settings changes and applies them to the current session too.
    /// Only fields that changed are copied, so quick toggles made in the dock
    /// this session are kept.
    fn persist_settings_changes(&mut self) {
        if self.prefs == self.saved_prefs {
            return;
        }
        let (old, new) = (&self.saved_prefs.recorder, &self.prefs.recorder);
        let c = &mut self.config;
        macro_rules! apply {
            ($($field:ident),*) => {
                $(if old.$field != new.$field {
                    c.$field = new.$field.clone();
                })*
            };
        }
        apply!(
            capture_system_audio,
            capture_microphone,
            capture_camera,
            shows_cursor,
            export_ai_telemetry,
            best_quality,
            auto_open_editor,
            ask_save_location,
            record_own_windows,
            fps,
            codec,
            format,
            output_dir
        );
        // An open editor reads arrows and Clean Move with the new keys right away
        if self.prefs.keys != self.saved_prefs.keys {
            if let Some(editor) = self.editor.as_mut() {
                editor.set_key_bindings(self.prefs.keys.clone());
            }
        }
        self.prefs.save();
        self.saved_prefs = self.prefs.clone();
    }

    fn stop_recording(&mut self) {
        self.state = AppState::Stopping;
        self.recorder.stop();
    }

}

impl eframe::App for DexHowToApp {
    /// Fully transparent window background: the dock draws its own rounded panel,
    /// and while recording the window is an invisible overlay over the screen.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if crate::window::OVERLAY_IN_DOCK {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            // Windows: an opaque dock (the system rounds its corners)
            theme::PANEL.to_normalized_gamma_f32()
        }
    }

    /// Called continuously by eframe even when the window is minimized or hidden
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_recorder_events(ctx);
        self.handle_tray_events(ctx);
        self.recover_from_user_restore(ctx);

        if matches!(
            self.state,
            AppState::Recording | AppState::CountingDown(_) | AppState::Starting
        ) {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Enforce ideal window geometry and bottom position on startup
        if !self.did_initial_resize {
            self.did_initial_resize = true;
            crate::platform::first_frame_drawn();
            crate::window::style_dock();
            let (origin_x, origin_y, screen_w, screen_h) =
                crate::display::get_main_display_bounds_points();
            let dock_w = 940.0;
            let dock_h = 210.0;
            let dock_x = origin_x + (screen_w - dock_w) / 2.0;
            let dock_y = origin_y + (screen_h - dock_h - 115.0).max(20.0);

            ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(dock_w, dock_h)));
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(dock_x, dock_y)));
        }

        // Process events during UI passes as well
        self.handle_recorder_events(&ctx);
        self.handle_tray_events(&ctx);

        if matches!(
            self.state,
            AppState::Recording | AppState::CountingDown(_) | AppState::Starting
        ) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }

        // Any empty part of the dock moves the window, like a normal Mac window
        // background. Registered first, so every widget drawn later sits on top of it.
        let countdown = match self.state {
            AppState::CountingDown(s) => Some(s),
            _ => None,
        };
        let recording = matches!(self.state, AppState::Starting | AppState::Recording);
        if self.overlay.is_some() && crate::window::OVERLAY_IN_DOCK {
            // Recording: transparent overlay with the countdown and live arrows
            // Stay above every window (full-screen apps, remote desktops…) and on every Space
            if let Some(overlay) = self.overlay.as_ref() {
                let d = overlay.display;
                crate::window::raise_overlay_window((
                    d.min.x as f64,
                    d.min.y as f64,
                    d.width() as f64,
                    d.height() as f64,
                ));
            }
            let style = self.prefs.arrows.clone();
            let arrow_key = self.prefs.keys.arrow;
            if let Some(overlay) = self.overlay.as_mut() {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| overlay.show(ui, countdown, recording, &style, arrow_key));
            }
        } else {
            // Windows: the overlay is a native window next to the dock
            if let Some(overlay) = self.overlay.as_mut() {
                let d = overlay.display;
                let bounds = (d.min.x as f64, d.min.y as f64, d.width() as f64, d.height() as f64);
                crate::window::raise_overlay_window(bounds);
                let style = &self.prefs.arrows;
                overlay.update(d.min, recording, style, self.prefs.keys.arrow);
                crate::window::show_overlay(bounds, &overlay.scene(countdown, recording, style));
                ctx.request_repaint_after(Duration::from_millis(overlay.repaint_ms(countdown)));
            }
            self.update_window_drag(&ctx);
            let background = ui.interact(
                ui.max_rect(),
                egui::Id::new("dock_background_drag"),
                Sense::drag(),
            );
            self.begin_window_drag(ui, &background);

            // The main window is the compact floating dock
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .stroke(Stroke::new(1.0, theme::BORDER))
                        .corner_radius(if crate::window::OVERLAY_IN_DOCK { 16.0 } else { 8.0 })
                        .inner_margin(Margin::symmetric(14, 10)),
                )
                .show(ui, |ui| {
                    render_floating_dock_view(ui, self);
                });
        }

        // Open Settings in a completely separate native window
        if self.settings_open {
            let settings_id = egui::ViewportId::from_hash_of("dexhowto_settings_window");
            let builder = egui::ViewportBuilder::default()
                .with_title("DexHowTo Recorder — Settings")
                .with_inner_size([800.0, 580.0])
                .with_min_inner_size([700.0, 480.0])
                .with_icon(crate::ui_kit::app_icon());

            let mut close_settings = false;

            ctx.show_viewport_immediate(settings_id, builder, |ctx, _class| {
                if ctx.input(|i| i.viewport().close_requested()) {
                    close_settings = true;
                }

                // The settings view lays out its own sidebar and page panels
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        render_settings_view(ui, self);
                    });
            });

            if close_settings {
                self.settings_open = false;
            }
            self.persist_settings_changes();
        }

        // Open Video Editor in a dedicated studio window positioned at the top of the display
        if let Some(ref mut editor) = self.editor {
            let (origin_x, origin_y, screen_w, screen_h) =
                crate::display::get_main_display_bounds_points();
            let editor_w = 1140.0_f32.min(screen_w - 40.0);
            let editor_h = 740.0_f32.min(screen_h - 100.0);
            let editor_x = origin_x + (screen_w - editor_w) / 2.0;
            let editor_y = origin_y + 36.0; // Place at top of screen

            let editor_id = egui::ViewportId::from_hash_of("dexhowto_editor_window");
            let builder = egui::ViewportBuilder::default()
                .with_title("DexHowTo Studio")
                .with_icon(crate::ui_kit::app_icon())
                .with_position([editor_x, editor_y])
                .with_inner_size([editor_w, editor_h])
                .with_min_inner_size([960.0, 620.0])
                .with_active(true);

            let mut close_editor = false;

            ctx.show_viewport_immediate(editor_id, builder, |ctx, _class| {
                if ctx.input(|i| i.viewport().close_requested()) {
                    close_editor = true;
                }

                // The editor lays out its own header, sidebar and timeline panels
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        crate::editor::render_editor_view(ui, editor, &mut close_editor);
                    });
            });

            if close_editor {
                self.editor = None;
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Floating Dock
// ─────────────────────────────────────────────────────────────────────────────

/// The four capture sources shown as tiles in the dock.
#[derive(Clone, Copy, PartialEq)]
enum Source {
    Screen,
    Camera,
    Microphone,
    SystemAudio,
}

impl Source {
    fn title(self) -> &'static str {
        match self {
            Source::Screen => "Screen",
            Source::Camera => "Camera",
            Source::Microphone => "Microphone",
            Source::SystemAudio => "System audio",
        }
    }

    fn tint(self) -> Color32 {
        match self {
            Source::Screen => Color32::from_rgb(88, 136, 255),
            Source::Camera => Color32::from_rgb(52, 190, 120),
            Source::Microphone => Color32::from_rgb(255, 159, 67),
            Source::SystemAudio => Color32::from_rgb(160, 110, 255),
        }
    }
}

/// Lets the user pick a recording (or its .jsonl) and opens it in the editor.
fn open_recording_in_editor(app: &mut DexHowToApp) {
    let dialog = rfd::FileDialog::new()
        .add_filter(
            "Recorded Video & Telemetry (*.mp4, *.mov, *.jsonl)",
            &["mp4", "mov", "jsonl"],
        )
        .add_filter("All Files", &["*"])
        .set_directory(&app.config.output_dir);
    if let Some(picked) = dialog.pick_file() {
        let (vid_opt, tel_opt) = crate::editor::resolve_recording_pair(&picked);
        if let Some(vid) = vid_opt {
            app.last_saved_path = Some(vid.clone());
            app.last_telemetry_path = tel_opt.clone();
            app.editor = Some(open_editor(&app.prefs, vid, tel_opt));
        }
    }
}

/// Opens a recording in the editor, starting from the saved export and arrow defaults.
fn open_editor(
    prefs: &crate::prefs::Preferences,
    video: PathBuf,
    telemetry: Option<PathBuf>,
) -> crate::editor::VideoEditor {
    let mut editor = crate::editor::VideoEditor::open(video, telemetry);
    editor.cursor_style = prefs.export_cursor.clone();
    editor.arrow_style = prefs.arrows.clone();
    editor.set_key_bindings(prefs.keys.clone());
    editor
}

/// macOS Save dialog for a finished recording, starting at the suggested name and
/// folder. Returns `None` when the user cancels (the suggested name is then used).
fn ask_where_to_save(video: &std::path::Path) -> Option<PathBuf> {
    let ext = video.extension().and_then(|e| e.to_str()).unwrap_or("mp4").to_string();
    let mut dialog = rfd::FileDialog::new()
        .set_title("Save Recording")
        .add_filter("Video", &[ext.as_str()]);
    if let Some(dir) = video.parent() {
        dialog = dialog.set_directory(dir);
    }
    if let Some(name) = video.file_name().and_then(|n| n.to_str()) {
        dialog = dialog.set_file_name(name);
    }
    dialog.save_file()
}

fn render_floating_dock_view(ui: &mut egui::Ui, app: &mut DexHowToApp) {
    let is_busy = app.state == AppState::Recording || matches!(app.state, AppState::CountingDown(_));

    render_dock_header(ui, app);
    ui.add_space(10.0);

    // ── Sources + record button ──────────────────────────────────────────────
    let rec_w = 112.0;
    let gap = 10.0;
    let tile_h = 100.0;
    let tile_w = ((ui.available_width() - rec_w - gap * 4.0) / 4.0).floor();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(gap, 0.0);
        for source in [Source::Screen, Source::Camera, Source::Microphone, Source::SystemAudio] {
            render_source_tile(ui, app, source, Vec2::new(tile_w, tile_h), is_busy);
        }
        render_dock_rec_section(ui, app, rec_w, tile_h);
    });

    ui.add_space(10.0);
    render_dock_footer(ui, app, is_busy);
}

/// Logo, name, live status and the window buttons.
fn render_dock_header(ui: &mut egui::Ui, app: &mut DexHowToApp) {
    ui.horizontal(|ui| {
        ui.set_height(28.0);
        let (logo, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
        ui.painter().image(
            logo_texture(ui.ctx()).id(),
            logo,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        ui.label(RichText::new("DexHowTo Recorder").size(13.0).strong().color(theme::TEXT));
        ui.add_space(6.0);
        render_dock_status(ui, app);

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if window_dot_button(ui, WindowButton::Close)
                .on_hover_text("Quit DexHowTo Recorder")
                .clicked()
            {
                let ctx = ui.ctx().clone();
                app.exit_app(&ctx);
            }
            if window_dot_button(ui, WindowButton::Minimize)
                .on_hover_text(if cfg!(windows) { "Minimize to the taskbar" } else { "Minimize to the macOS Dock" })
                .clicked()
            {
                ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
            }
            ui.add_space(4.0);
            if gear_button(ui).on_hover_text("Settings").clicked() {
                app.settings_open = true;
            }
        });
    });
}

/// Status pill: Ready, countdown, live recording time and size, saved, or error.
fn render_dock_status(ui: &mut egui::Ui, app: &DexHowToApp) {
    let (text, color, blink) = match &app.state {
        AppState::Idle => ("Ready".to_string(), theme::SUCCESS, false),
        AppState::CountingDown(rem) => (format!("Starting in {rem}…"), theme::WARN, false),
        AppState::Starting => ("Starting…".to_string(), theme::WARN, false),
        AppState::Recording => (
            format!(
                "REC  {}  ·  {}",
                DexHowToApp::format_duration(app.recording_duration_secs),
                DexHowToApp::format_file_size(app.recording_file_size_bytes)
            ),
            theme::CUT,
            true,
        ),
        AppState::Stopping => ("Saving…".to_string(), theme::WARN, false),
        AppState::Finished => (
            format!(
                "Saved  {}  ·  {}",
                DexHowToApp::format_duration(app.last_saved_duration),
                DexHowToApp::format_file_size(app.last_saved_size)
            ),
            theme::SUCCESS,
            false,
        ),
        AppState::Error(err) => (truncate_middle(&format!("Error: {err}"), 48), theme::CUT, false),
    };

    let galley = ui
        .painter()
        .layout_no_wrap(text, FontId::proportional(11.5), color);
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(galley.size().x + 30.0, 22.0),
        Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 11.0, color.gamma_multiply(0.12));
    painter.rect_stroke(rect, 11.0, Stroke::new(1.0, color.gamma_multiply(0.4)), egui::StrokeKind::Inside);
    let dot_on = !blink || ui.input(|i| (i.time * 2.5).sin() > 0.0);
    let dot = Pos2::new(rect.left() + 12.0, rect.center().y);
    painter.circle_filled(dot, 3.5, if dot_on { color } else { color.gamma_multiply(0.35) });
    painter.galley(
        Pos2::new(rect.left() + 21.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if let AppState::Error(err) = &app.state {
        resp.on_hover_text(err);
    }
    if blink {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum WindowButton {
    Minimize,
    Close,
}

/// Small round window button that shows its color and symbol on hover.
fn window_dot_button(ui: &mut egui::Ui, kind: WindowButton) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
    let hovered = resp.hovered();
    let (bg, fg) = match (kind, hovered) {
        (WindowButton::Close, true) => (Color32::from_rgb(255, 95, 87), Color32::from_rgb(90, 10, 10)),
        (WindowButton::Minimize, true) => (Color32::from_rgb(254, 188, 46), Color32::from_rgb(90, 55, 0)),
        _ => (theme::SURFACE_HI, theme::TEXT_DIM),
    };
    let c = rect.center();
    let painter = ui.painter();
    painter.circle_filled(c, 8.0, bg);
    let stroke = Stroke::new(1.4, fg);
    match kind {
        WindowButton::Minimize => {
            painter.line_segment([c - Vec2::new(3.5, 0.0), c + Vec2::new(3.5, 0.0)], stroke);
        }
        WindowButton::Close => {
            painter.line_segment([c - Vec2::splat(3.0), c + Vec2::splat(3.0)], stroke);
            painter.line_segment([c + Vec2::new(-3.0, 3.0), c + Vec2::new(3.0, -3.0)], stroke);
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Drawn gear icon button for Settings.
fn gear_button(ui: &mut egui::Ui) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 6.0, theme::SURFACE_HI);
    }
    let color = if resp.hovered() { theme::TEXT } else { theme::TEXT_DIM };
    let c = rect.center();
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = Vec2::new(a.cos(), a.sin());
        painter.line_segment([c + dir * 5.0, c + dir * 7.6], Stroke::new(2.4, color));
    }
    painter.circle_stroke(c, 5.2, Stroke::new(1.8, color));
    painter.circle_filled(c, 1.8, color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Draws the source symbol (monitor, camera, microphone, speaker) centered in `rect`.
fn draw_source_icon(painter: &egui::Painter, rect: Rect, source: Source, color: Color32) {
    let c = rect.center();
    let stroke = Stroke::new(1.8, color);
    match source {
        Source::Screen => {
            let screen = Rect::from_center_size(c - Vec2::new(0.0, 2.0), Vec2::new(20.0, 13.0));
            painter.rect_stroke(screen, 2.5, stroke, egui::StrokeKind::Middle);
            painter.line_segment([Pos2::new(c.x, screen.bottom()), Pos2::new(c.x, screen.bottom() + 3.5)], stroke);
            painter.line_segment(
                [Pos2::new(c.x - 5.0, screen.bottom() + 4.0), Pos2::new(c.x + 5.0, screen.bottom() + 4.0)],
                stroke,
            );
        }
        Source::Camera => {
            let body = Rect::from_center_size(c - Vec2::new(2.5, 0.0), Vec2::new(14.0, 11.0));
            painter.rect_stroke(body, 2.5, stroke, egui::StrokeKind::Middle);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(body.right() + 1.5, c.y - 1.5),
                    Pos2::new(body.right() + 6.0, c.y - 4.5),
                    Pos2::new(body.right() + 6.0, c.y + 4.5),
                    Pos2::new(body.right() + 1.5, c.y + 1.5),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Source::Microphone => {
            let capsule = Rect::from_center_size(c - Vec2::new(0.0, 3.0), Vec2::new(7.0, 12.0));
            painter.rect_filled(capsule, 3.5, color);
            let arc: Vec<Pos2> = (0..=12)
                .map(|i| {
                    let a = std::f32::consts::PI * (i as f32 / 12.0);
                    Pos2::new(c.x - 6.5 * a.cos(), c.y - 1.0 + 6.5 * a.sin())
                })
                .collect();
            painter.add(egui::Shape::line(arc, stroke));
            painter.line_segment([Pos2::new(c.x, c.y + 5.5), Pos2::new(c.x, c.y + 8.5)], stroke);
        }
        Source::SystemAudio => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(c.x - 8.0, c.y - 3.0),
                    Pos2::new(c.x - 4.5, c.y - 3.0),
                    Pos2::new(c.x + 0.5, c.y - 7.5),
                    Pos2::new(c.x + 0.5, c.y + 7.5),
                    Pos2::new(c.x - 4.5, c.y + 3.0),
                    Pos2::new(c.x - 8.0, c.y + 3.0),
                ],
                color,
                Stroke::NONE,
            ));
            for r in [5.0, 9.0] {
                let arc: Vec<Pos2> = (0..=10)
                    .map(|i| {
                        let a = -0.8 + 1.6 * (i as f32 / 10.0);
                        Pos2::new(c.x + 0.5 + r * a.cos(), c.y + r * a.sin())
                    })
                    .collect();
                painter.add(egui::Shape::line(arc, stroke));
            }
        }
    }
}

/// One capture source: icon, name, device and an on/off switch. Clicking the tile toggles it.
fn render_source_tile(ui: &mut egui::Ui, app: &mut DexHowToApp, source: Source, size: Vec2, is_busy: bool) {
    let (rect, tile_resp) = ui.allocate_exact_size(size, Sense::click());
    // Sources this system cannot record yet stay off and cannot be switched on
    let supported = match source {
        Source::Camera => crate::platform::CAMERA_SUPPORTED,
        Source::SystemAudio => crate::platform::SYSTEM_AUDIO_SUPPORTED,
        _ => true,
    };
    let on = supported
        && match source {
            Source::Screen => app.screen_enabled,
            Source::Camera => app.config.capture_camera,
            Source::Microphone => app.config.capture_microphone,
            Source::SystemAudio => app.config.capture_system_audio,
        };
    let tint = source.tint();
    let hovered = tile_resp.hovered() && !is_busy;

    let painter = ui.painter();
    // Lighter on hover, but still darker than the switch track so the switch stays visible
    let fill = if hovered {
        theme::SURFACE.lerp_to_gamma(theme::SURFACE_HI, 0.45)
    } else {
        theme::SURFACE
    };
    painter.rect_filled(rect, 12.0, fill);
    painter.rect_stroke(
        rect,
        12.0,
        Stroke::new(1.0, if on { tint.gamma_multiply(0.55) } else { theme::BORDER }),
        egui::StrokeKind::Inside,
    );

    // Icon tile
    let icon = Rect::from_min_size(rect.min + Vec2::new(12.0, 12.0), Vec2::splat(36.0));
    if on {
        painter.rect_filled(icon, 9.0, tint);
        draw_source_icon(painter, icon, source, Color32::WHITE);
    } else {
        painter.rect_filled(icon, 9.0, theme::SURFACE_HI);
        draw_source_icon(painter, icon, source, theme::TEXT_FAINT);
    }

    painter.text(
        Pos2::new(rect.left() + 12.0, rect.top() + 58.0),
        egui::Align2::LEFT_TOP,
        source.title(),
        FontId::proportional(13.0),
        if on { theme::TEXT } else { theme::TEXT_DIM },
    );

    // Switch in the top-right corner
    let switch_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 42.0, rect.top() + 21.0),
        Vec2::new(30.0, 17.0),
    );
    // Child UIs placed inside the tile (new_child does not take space in the row layout)
    let mut switch_ui = ui.new_child(egui::UiBuilder::new().max_rect(switch_rect));
    let switched = switch_ui
        .add_enabled_ui(!is_busy && supported, |ui| {
            let mut value = on;
            settings_toggle(ui, &mut value).changed()
        })
        .inner;

    // Microphone and camera need macOS permission on top of the switch
    let av_device = match source {
        Source::Microphone => Some(crate::av_access::Device::Microphone),
        Source::Camera => Some(crate::av_access::Device::Camera),
        _ => None,
    };
    let blocked = av_device.is_some_and(|d| {
        matches!(
            crate::av_access::status(d),
            crate::av_access::Access::Denied | crate::av_access::Access::Restricted
        )
    });

    // Device line under the name
    let device_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 8.0, rect.top() + 74.0),
        Pos2::new(rect.right() - 8.0, rect.bottom() - 6.0),
    );
    let device_color = if on { theme::TEXT_DIM } else { theme::TEXT_FAINT };
    match source {
        Source::Screen => {
            let selected = app
                .displays
                .iter()
                .enumerate()
                .find(|(_, d)| Some(d.id) == app.config.selected_display_id)
                .map(|(i, d)| {
                    if d.is_main {
                        format!("Main display · {}×{}", d.width, d.height)
                    } else {
                        format!("Display {} · {}×{}", i + 1, d.width, d.height)
                    }
                })
                .unwrap_or_else(|| "No display found".to_string());
            let mut picker_ui = ui.new_child(egui::UiBuilder::new().max_rect(device_rect));
            picker_ui.add_enabled_ui(!is_busy, |ui| {
                {
                    egui::ComboBox::from_id_salt("dock_display_picker")
                        .selected_text(RichText::new(selected).size(11.5).color(device_color))
                        .width(device_rect.width())
                        .show_ui(ui, |ui| {
                            for (idx, d) in app.displays.iter().enumerate() {
                                let tag = if d.is_main { " (main)" } else { "" };
                                ui.selectable_value(
                                    &mut app.config.selected_display_id,
                                    Some(d.id),
                                    format!("Display {}{tag} · {}×{}", idx + 1, d.width, d.height),
                                );
                            }
                        });
                }
            });
        }
        _ => {
            let device = match source {
                Source::Camera => "FaceTime HD Camera",
                Source::Microphone if cfg!(windows) => "Default microphone",
                Source::Microphone => "Built-in microphone",
                _ => "Mac sound output",
            };
            let (line, color) = if !supported {
                ("Not on Windows yet", theme::TEXT_FAINT)
            } else if on && blocked {
                ("No access · click to allow", theme::WARN)
            } else if on {
                (device, device_color)
            } else {
                ("Off", device_color)
            };
            ui.painter().text(
                Pos2::new(rect.left() + 12.0, rect.top() + 78.0),
                egui::Align2::LEFT_TOP,
                line,
                FontId::proportional(11.5),
                color,
            );
        }
    }

    // A blocked device that is switched on: clicking the tile opens its privacy settings
    if on && blocked && tile_resp.clicked() && !switched {
        if let Some(d) = av_device {
            crate::av_access::open_privacy_settings(d);
        }
        return;
    }

    // Clicking anywhere else on the tile toggles it too
    let toggle = supported && (switched || (tile_resp.clicked() && !is_busy));
    if toggle && !on {
        // Turning it on: ask macOS for access now, so the recording does not lack it
        if let Some(d) = av_device {
            match crate::av_access::status(d) {
                crate::av_access::Access::NotDetermined => crate::av_access::request(d),
                crate::av_access::Access::Denied | crate::av_access::Access::Restricted => {
                    crate::av_access::open_privacy_settings(d)
                }
                crate::av_access::Access::Allowed => {}
            }
        }
    }
    if toggle {
        match source {
            Source::Screen => app.screen_enabled = !app.screen_enabled,
            Source::Camera => app.config.capture_camera = !app.config.capture_camera,
            Source::Microphone => app.config.capture_microphone = !app.config.capture_microphone,
            Source::SystemAudio => {
                app.config.capture_system_audio = !app.config.capture_system_audio
            }
        }
    }
    if !is_busy {
        tile_resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
}

/// Compact on/off chip with a status dot, used for the quick options in the footer.
fn option_chip(ui: &mut egui::Ui, label: &str, on: bool, tint: Color32, enabled: bool) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(12.0), theme::TEXT);
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(galley.size().x + 32.0, 28.0),
        if enabled { Sense::click() } else { Sense::hover() },
    );
    let hovered = enabled && resp.hovered();
    let painter = ui.painter();
    let (fill, border, text) = if on {
        (tint.gamma_multiply(0.14), tint.gamma_multiply(0.55), tint.lerp_to_gamma(Color32::WHITE, 0.3))
    } else {
        (theme::SURFACE, theme::BORDER, theme::TEXT_DIM)
    };
    let fill = if hovered { fill.lerp_to_gamma(Color32::WHITE, 0.05) } else { fill };
    painter.rect_filled(rect, 14.0, fill);
    painter.rect_stroke(rect, 14.0, Stroke::new(1.0, border), egui::StrokeKind::Inside);
    let dot = Pos2::new(rect.left() + 13.0, rect.center().y);
    if on {
        painter.circle_filled(dot, 3.5, tint);
    } else {
        painter.circle_stroke(dot, 3.0, Stroke::new(1.2, theme::TEXT_FAINT));
    }
    let text = if enabled { text } else { text.gamma_multiply(0.5) };
    painter.galley(
        Pos2::new(rect.left() + 22.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text,
    );
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Quick options on the left, actions for the current state on the right.
fn render_dock_footer(ui: &mut egui::Ui, app: &mut DexHowToApp, is_busy: bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;

        let best = app.config.best_quality;
        if option_chip(ui, "Best quality", best, Color32::from_rgb(160, 110, 255), !is_busy)
            .on_hover_text("Full Retina resolution with Display P3 color")
            .clicked()
        {
            app.config.best_quality = !best;
        }
        let cursor = app.config.shows_cursor;
        if option_chip(ui, "Cursor", cursor, theme::ACCENT, !is_busy)
            .on_hover_text(if cursor {
                "The pointer is recorded into the video. Click to hide it (telemetry still records it)."
            } else {
                "The pointer is hidden from the video. Click to show it."
            })
            .clicked()
        {
            app.config.shows_cursor = !cursor;
        }
        let telemetry = app.config.export_ai_telemetry;
        if option_chip(ui, "Telemetry", telemetry, theme::SUCCESS, !is_busy)
            .on_hover_text("Save mouse and keyboard data (.mouse.jsonl) next to the video")
            .clicked()
        {
            app.config.export_ai_telemetry = !telemetry;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let finished_path = (app.state == AppState::Finished)
                .then(|| app.last_saved_path.clone())
                .flatten();

            if let Some(path) = finished_path {
                if button(ui, ButtonKind::Primary, "Edit recording", true).clicked() {
                    app.editor = Some(open_editor(&app.prefs, 
                        path.clone(),
                        app.last_telemetry_path.clone(),
                    ));
                }
                if button(ui, ButtonKind::Subtle, "Play", true).clicked() {
                    crate::platform::open_path(&path);
                }
                let show = format!("Show in {}", crate::platform::FILE_MANAGER);
                if button(ui, ButtonKind::Ghost, &show, true).clicked() {
                    crate::platform::reveal_in_file_manager(&path);
                }
                if let Some(camera) = app.last_camera_path.clone() {
                    if button(ui, ButtonKind::Ghost, "Camera", true)
                        .on_hover_text(format!(
                            "Show the camera recording in {}",
                            crate::platform::FILE_MANAGER
                        ))
                        .clicked()
                    {
                        crate::platform::reveal_in_file_manager(&camera);
                    }
                }
            } else {
                if button(ui, ButtonKind::Subtle, "Editor", !is_busy)
                    .on_hover_text("Open the last recording in the editor")
                    .clicked()
                {
                    if let Some(path) = app.last_saved_path.clone() {
                        app.editor = Some(open_editor(&app.prefs, 
                            path,
                            app.last_telemetry_path.clone(),
                        ));
                    } else {
                        open_recording_in_editor(app);
                    }
                }
                if button(ui, ButtonKind::Ghost, "Open…", !is_busy)
                    .on_hover_text("Open a recording (.mp4/.mov) or its telemetry (.jsonl)")
                    .clicked()
                {
                    open_recording_in_editor(app);
                }
            }

            if app.permission_denied
                && crate::platform::HAS_PRIVACY_PERMISSIONS
                && button(ui, ButtonKind::Tinted(theme::WARN), "Allow screen recording", true)
                    .on_hover_text("Opens System Settings › Privacy & Security › Screen Recording")
                    .clicked()
            {
                open_screen_recording_settings();
            }
        });
    });
}

/// Record button: white ring with a red dot (the app icon), a stop square while
/// recording, and the countdown number before it starts.
fn render_dock_rec_section(ui: &mut egui::Ui, app: &mut DexHowToApp, width: f32, height: f32) {
    let can_start = !app.displays.is_empty()
        && app.state != AppState::Recording
        && app.state != AppState::Starting
        && !matches!(app.state, AppState::CountingDown(_));
    let is_recording = app.state == AppState::Recording;
    let countdown = if let AppState::CountingDown(rem) = app.state {
        Some(rem)
    } else {
        None
    };
    let active = can_start || is_recording || countdown.is_some();

    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    if response.clicked() {
        let ctx = ui.ctx().clone();
        if is_recording {
            app.stop_recording();
        } else if countdown.is_some() {
            app.cancel_countdown(&ctx);
        } else if can_start {
            app.start_countdown(&ctx);
        }
    }

    let hovered = response.hovered() && active;
    let t = ui.ctx().animate_bool(response.id.with("hover"), hovered);
    let painter = ui.painter();
    let center = Pos2::new(rect.center().x, rect.top() + 38.0);

    let red = if active { Color32::from_rgb(242, 54, 66) } else { Color32::from_rgb(110, 50, 56) };
    let ring = if is_recording {
        // Gently pulsing red ring while recording
        let pulse = ((ui.input(|i| i.time) * 3.0).sin() * 0.5 + 0.5) as f32;
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        red.gamma_multiply(0.55 + 0.45 * pulse)
    } else if active {
        Color32::from_rgb(235, 238, 245)
    } else {
        theme::TEXT_FAINT
    };

    if active {
        painter.circle_filled(center, 40.0, red.gamma_multiply(0.10 + 0.10 * t));
    }
    painter.circle_stroke(center, 33.0, Stroke::new(3.5, ring));

    if let Some(secs) = countdown {
        painter.circle_filled(center, 27.0, red);
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            format!("{secs}"),
            FontId::proportional(24.0),
            Color32::WHITE,
        );
    } else if is_recording {
        painter.rect_filled(Rect::from_center_size(center, Vec2::splat(24.0)), 5.0, red);
    } else {
        painter.circle_filled(center, 25.0 + 2.5 * t, red);
    }

    let (label, color) = if is_recording {
        ("Stop", theme::CUT)
    } else if countdown.is_some() {
        ("Cancel", theme::WARN)
    } else if can_start {
        ("Record", theme::TEXT)
    } else {
        ("No display", theme::TEXT_FAINT)
    };
    painter.text(
        Pos2::new(center.x, rect.bottom() - 10.0),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.5),
        color,
    );
    if active {
        response.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Settings Window
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum SettingsPage {
    #[default]
    Recording,
    Defaults,
    Video,
    Telemetry,
    /// macOS only (see `SettingsPage::ALL`)
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Permissions,
}

impl SettingsPage {
    #[cfg(target_os = "macos")]
    const ALL: &[SettingsPage] = &[
        SettingsPage::Recording,
        SettingsPage::Defaults,
        SettingsPage::Video,
        SettingsPage::Telemetry,
        SettingsPage::Permissions,
    ];
    /// Windows has no privacy permissions to manage, so no Permissions page
    #[cfg(not(target_os = "macos"))]
    const ALL: &[SettingsPage] = &[
        SettingsPage::Recording,
        SettingsPage::Defaults,
        SettingsPage::Video,
        SettingsPage::Telemetry,
    ];

    fn title(self) -> &'static str {
        match self {
            SettingsPage::Recording => "Recording",
            SettingsPage::Defaults => "Defaults",
            SettingsPage::Video => "Video",
            SettingsPage::Telemetry => "AI Telemetry",
            SettingsPage::Permissions => "Permissions",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            SettingsPage::Recording => "Where recordings are saved and what happens after.",
            SettingsPage::Defaults => "What the recorder and the editor start with.",
            SettingsPage::Video => "Frame rate, format and quality of the recorded video.",
            SettingsPage::Telemetry => "Mouse and keyboard data saved next to each recording.",
            SettingsPage::Permissions => "macOS access the recorder needs to capture your screen.",
        }
    }

    fn tint(self) -> Color32 {
        match self {
            SettingsPage::Recording => Color32::from_rgb(236, 72, 90),
            SettingsPage::Defaults => Color32::from_rgb(255, 159, 67),
            SettingsPage::Video => Color32::from_rgb(88, 136, 255),
            SettingsPage::Telemetry => Color32::from_rgb(160, 110, 255),
            SettingsPage::Permissions => Color32::from_rgb(52, 190, 120),
        }
    }
}

/// Small rounded tile with a drawn white symbol, like the icons in macOS System Settings.
fn settings_page_icon(painter: &egui::Painter, rect: Rect, page: SettingsPage) {
    painter.rect_filled(rect, 6.0, page.tint());
    let c = rect.center();
    let s = rect.width() / 22.0;
    let white = Color32::WHITE;
    match page {
        SettingsPage::Recording => {
            painter.circle_stroke(c, 6.5 * s, Stroke::new(1.6 * s, white));
            painter.circle_filled(c, 3.6 * s, white);
        }
        SettingsPage::Defaults => {
            // Three sliders
            for (i, knob) in [(-4.0, -2.5), (0.0, 3.0), (4.0, -0.5)] {
                let y = c.y + i * s;
                painter.line_segment(
                    [Pos2::new(c.x - 6.5 * s, y), Pos2::new(c.x + 6.5 * s, y)],
                    Stroke::new(1.4 * s, white),
                );
                painter.circle_filled(Pos2::new(c.x + knob * s, y), 2.0 * s, white);
            }
        }
        SettingsPage::Video => {
            let body = Rect::from_center_size(c - Vec2::new(2.0 * s, 0.0), Vec2::new(10.0 * s, 8.0 * s));
            painter.rect_filled(body, 2.0 * s, white);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(body.right() + 0.8 * s, c.y - 1.2 * s),
                    Pos2::new(body.right() + 5.0 * s, c.y - 3.8 * s),
                    Pos2::new(body.right() + 5.0 * s, c.y + 3.8 * s),
                    Pos2::new(body.right() + 0.8 * s, c.y + 1.2 * s),
                ],
                white,
                Stroke::NONE,
            ));
        }
        SettingsPage::Telemetry => {
            let tip = c + Vec2::new(-4.0 * s, -6.5 * s);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    tip,
                    tip + Vec2::new(0.0, 11.5 * s),
                    tip + Vec2::new(3.2 * s, 8.6 * s),
                    tip + Vec2::new(8.6 * s, 8.4 * s),
                ],
                white,
                Stroke::NONE,
            ));
        }
        SettingsPage::Permissions => {
            let body = Rect::from_center_size(c + Vec2::new(0.0, 2.2 * s), Vec2::new(10.0 * s, 7.5 * s));
            let shackle = Rect::from_center_size(c - Vec2::new(0.0, 2.6 * s), Vec2::new(6.4 * s, 8.0 * s));
            painter.rect_stroke(shackle, 3.2 * s, Stroke::new(1.6 * s, white), egui::StrokeKind::Middle);
            painter.rect_filled(body, 1.8 * s, white);
        }
    }
}

/// One settings row: title and explanation on the left, the control on the right.
fn setting_row(
    ui: &mut egui::Ui,
    title: &str,
    help: &str,
    control_width: f32,
    add_control: impl FnOnce(&mut egui::Ui),
) {
    let text_width = (ui.available_width() - control_width - 16.0).max(120.0);
    ui.horizontal(|ui| {
        let left = ui
            .allocate_ui_with_layout(
                Vec2::new(text_width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(title).size(13.0).color(theme::TEXT));
                    if !help.is_empty() {
                        ui.label(RichText::new(help).size(11.5).color(theme::TEXT_DIM));
                    }
                },
            )
            .response
            .rect;
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), left.height().max(28.0)),
            egui::Layout::right_to_left(egui::Align::Center),
            add_control,
        );
    });
}

fn row_separator(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::BORDER);
    ui.add_space(8.0);
}

/// Grouped box of settings rows.
fn settings_group(ui: &mut egui::Ui, heading: &str, add_rows: impl FnOnce(&mut egui::Ui)) {
    if !heading.is_empty() {
        section_label(ui, heading);
    }
    egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(10.0)
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_rows(ui);
        });
    ui.add_space(18.0);
}

/// Compact status label with a colored dot, sized to its text.
fn status_pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(11.5), color);
    let size = Vec2::new(galley.size().x + 30.0, 24.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 12.0, color.gamma_multiply(0.14));
    painter.rect_stroke(rect, 12.0, Stroke::new(1.0, color.gamma_multiply(0.5)), egui::StrokeKind::Inside);
    painter.circle_filled(Pos2::new(rect.left() + 12.0, rect.center().y), 3.5, color);
    painter.galley(
        Pos2::new(rect.left() + 21.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}

/// Segmented picker (like a macOS segmented control). Returns true when it changed.
fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let font = FontId::proportional(12.5);
    let galleys: Vec<_> = options
        .iter()
        .map(|(_, label)| ui.painter().layout_no_wrap(label.to_string(), font.clone(), theme::TEXT))
        .collect();
    let seg_widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 24.0).collect();
    let total = Vec2::new(seg_widths.iter().sum::<f32>() + 4.0, 30.0);
    // Base segment ids on this control's own auto id: `ui.id()` is shared by sibling
    // child UIs, so two pickers in one settings group would steal each other's clicks.
    let (rect, base) = ui.allocate_exact_size(total, Sense::hover());
    let base_id = base.id;
    let enabled = ui.is_enabled();
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, theme::BG);
    painter.rect_stroke(rect, 8.0, Stroke::new(1.0, theme::BORDER), egui::StrokeKind::Inside);

    let mut changed = false;
    let mut x = rect.left() + 2.0;
    for (i, ((option, _), galley)) in options.iter().zip(galleys).enumerate() {
        let seg = Rect::from_min_size(Pos2::new(x, rect.top() + 2.0), Vec2::new(seg_widths[i], 26.0));
        x += seg_widths[i];
        let resp = ui.interact(seg, base_id.with(("segment", i)), Sense::click());
        let selected = *value == *option;
        if selected {
            painter.rect_filled(seg, 6.0, theme::SURFACE_HI.lerp_to_gamma(theme::ACCENT, 0.35));
        } else if resp.hovered() && enabled {
            painter.rect_filled(seg, 6.0, theme::SURFACE);
        }
        let color = if !enabled {
            theme::TEXT_FAINT
        } else if selected {
            Color32::WHITE
        } else {
            theme::TEXT_DIM
        };
        painter.galley_with_override_text_color(seg.center() - galley.size() / 2.0, galley, color);
        if enabled && resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !selected {
            *value = *option;
            changed = true;
        }
    }
    changed
}

fn render_settings_view(ui: &mut egui::Ui, app: &mut DexHowToApp) {
    let is_busy = app.state == AppState::Recording || matches!(app.state, AppState::CountingDown(_));

    // ── Sidebar ──────────────────────────────────────────────────────────────
    egui::Panel::left("settings_sidebar")
        .resizable(false)
        .exact_size(210.0)
        .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(12, 16)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (logo, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                ui.painter().image(
                    logo_texture(ui.ctx()).id(),
                    logo,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add_space(3.0);
                    ui.label(RichText::new("DexHowTo").size(14.0).strong().color(theme::TEXT));
                    ui.label(RichText::new("Recorder settings").size(11.5).color(theme::TEXT_DIM));
                });
            });
            ui.add_space(18.0);

            for &page in SettingsPage::ALL {
                let (row, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
                let selected = app.settings_page == page;
                let fill = if selected {
                    theme::SURFACE_HI
                } else if resp.hovered() {
                    theme::SURFACE
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(row, 8.0, fill);
                let icon = Rect::from_min_size(
                    Pos2::new(row.left() + 8.0, row.center().y - 11.0),
                    Vec2::splat(22.0),
                );
                settings_page_icon(ui.painter(), icon, page);
                ui.painter().text(
                    Pos2::new(icon.right() + 10.0, row.center().y),
                    egui::Align2::LEFT_CENTER,
                    page.title(),
                    FontId::proportional(13.0),
                    if selected { theme::TEXT } else { theme::TEXT_DIM },
                );
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    app.settings_page = page;
                }
                ui.add_space(2.0);
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.label(
                    RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                );
            });
        });

    // ── Page content ─────────────────────────────────────────────────────────
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin::symmetric(28, 22)))
        .show(ui, |ui| {
            let page = app.settings_page;
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(page.title()).size(20.0).strong().color(theme::TEXT));
                    ui.label(RichText::new(page.subtitle()).size(12.0).color(theme::TEXT_DIM));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if button(ui, ButtonKind::Primary, "Done", true).clicked() {
                        app.settings_open = false;
                    }
                });
            });
            ui.add_space(18.0);

            if is_busy {
                egui::Frame::new()
                    .fill(theme::WARN.gamma_multiply(0.12))
                    .stroke(Stroke::new(1.0, theme::WARN.gamma_multiply(0.5)))
                    .corner_radius(8.0)
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("Recording in progress. Settings are locked until it stops.")
                                .size(12.0)
                                .color(theme::WARN),
                        );
                    });
                ui.add_space(14.0);
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_enabled_ui(!is_busy, |ui| match page {
                        SettingsPage::Recording => settings_recording_page(ui, app),
                        SettingsPage::Defaults => settings_defaults_page(ui, app),
                        SettingsPage::Video => settings_video_page(ui, app),
                        SettingsPage::Telemetry => settings_telemetry_page(ui, app),
                        SettingsPage::Permissions => settings_permissions_page(ui, app),
                    });
                });
        });
}

fn settings_recording_page(ui: &mut egui::Ui, app: &mut DexHowToApp) {
    let home = crate::platform::home_dir();
    let movies = crate::platform::videos_dir();
    let movies_label = if cfg!(windows) { "Videos" } else { "Movies" };
    let desktop = home.join("Desktop");

    settings_group(ui, "SAVE LOCATION", |ui| {
        let shown = app
            .config
            .output_dir
            .strip_prefix(&home)
            .map(|p| format!("~/{}", p.display()))
            .unwrap_or_else(|_| app.prefs.recorder.output_dir.display().to_string());
        setting_row(ui, "Folder", &truncate_middle(&shown, 48), 190.0, |ui| {
            if button(ui, ButtonKind::Subtle, "Choose…", true).clicked() {
                if let Some(folder) = rfd::FileDialog::new()
                    .set_directory(&app.prefs.recorder.output_dir)
                    .pick_folder()
                {
                    app.prefs.recorder.output_dir = folder;
                }
            }
            if button(ui, ButtonKind::Ghost, "Show", true)
                .on_hover_text(format!("Open the folder in {}", crate::platform::FILE_MANAGER))
                .clicked()
            {
                crate::platform::open_path(&app.prefs.recorder.output_dir);
            }
        });
        row_separator(ui);
        setting_row(ui, "Quick picks", "Common places to save recordings.", 190.0, |ui| {
            for (label, dir) in [("Desktop", &desktop), (movies_label, &movies)] {
                if !dir.exists() {
                    continue;
                }
                let kind = if &app.prefs.recorder.output_dir == dir {
                    ButtonKind::Tinted(theme::ACCENT)
                } else {
                    ButtonKind::Subtle
                };
                if button(ui, kind, label, true).clicked() {
                    app.prefs.recorder.output_dir = dir.clone();
                }
            }
        });
        row_separator(ui);
        let example = chrono::Local::now().format("Screen Recording %Y-%m-%d at %H.%M.%S");
        setting_row(
            ui,
            "File name",
            &format!("{example}.{}", app.prefs.recorder.format.extension()),
            0.0,
            |_| {},
        );
    });

    settings_group(ui, "WHAT GETS RECORDED", |ui| {
        setting_row(
            ui,
            "Record DexHowTo Recorder windows",
            "Off: the recorder's dock, editor, countdown and live arrows stay out of the video. On: a full recording of the screen, including them.",
            60.0,
            |ui| {
                settings_toggle(ui, &mut app.prefs.recorder.record_own_windows);
            },
        );
    });

    settings_group(ui, "AFTER RECORDING", |ui| {
        setting_row(
            ui,
            "Ask for a name and folder",
            "Show a Save dialog when a recording stops. When off, recordings are saved to the folder above automatically.",
            60.0,
            |ui| {
                settings_toggle(ui, &mut app.prefs.recorder.ask_save_location);
            },
        );
        row_separator(ui);
        setting_row(
            ui,
            "Open in the editor",
            "Show the new recording in the editor as soon as it is saved.",
            60.0,
            |ui| {
                settings_toggle(ui, &mut app.prefs.recorder.auto_open_editor);
            },
        );
    });

}

fn settings_defaults_page(ui: &mut egui::Ui, app: &mut DexHowToApp) {
    settings_group(ui, "WHEN THE APP STARTS", |ui| {
        let rec = &mut app.prefs.recorder;
        let pointer = if cfg!(windows) { "mouse" } else { "macOS" };
        setting_row(
            ui,
            "Cursor in the video",
            &format!("Record the {pointer} pointer. Turn off for a clean video: telemetry still records the mouse, and the editor can draw a cursor on export."),
            60.0,
            |ui| {
                settings_toggle(ui, &mut rec.shows_cursor);
            },
        );
        row_separator(ui);
        setting_row(ui, "Microphone", "Record your voice.", 60.0, |ui| {
            settings_toggle(ui, &mut rec.capture_microphone);
        });
        if crate::platform::SYSTEM_AUDIO_SUPPORTED {
            row_separator(ui);
            setting_row(ui, "System audio", "Record the sound your Mac plays.", 60.0, |ui| {
                settings_toggle(ui, &mut rec.capture_system_audio);
            });
        }
        if crate::platform::CAMERA_SUPPORTED {
            row_separator(ui);
            setting_row(
                ui,
                "Camera",
                "Save the camera as its own file next to each recording.",
                60.0,
                |ui| {
                    settings_toggle(ui, &mut rec.capture_camera);
                },
            );
        }
    });
    ui.label(
        RichText::new("Switching these in the dock changes only the current session.")
            .size(11.5)
            .color(theme::TEXT_FAINT),
    );
    ui.add_space(16.0);

    settings_group(ui, "KEYS (HOLD WHILE MOVING THE MOUSE)", |ui| {
        let keys = &mut app.prefs.keys;
        let options: Vec<(crate::keys::ModifierKey, &str)> = crate::keys::ModifierKey::ALL
            .iter()
            .map(|k| (*k, k.label()))
            .collect();
        setting_row(
            ui,
            "Draw an arrow",
            "Press where the arrow starts, move to what you point at, release.",
            340.0,
            |ui| {
                segmented(ui, &mut keys.arrow, &options);
            },
        );
        row_separator(ui);
        setting_row(
            ui,
            "Keep exact movement",
            "Clean Move keeps mouse movement made while holding this key (and the arrow key).",
            340.0,
            |ui| {
                segmented(ui, &mut keys.keep_movement, &options);
            },
        );
    });
    ui.label(
        RichText::new(
            "Only modifier keys are recorded with the mouse, so these are the keys you can choose. Changing a key also applies to earlier recordings.",
        )
        .size(11.5)
        .color(theme::TEXT_FAINT),
    );
    ui.add_space(16.0);

    let arrow_key = app.prefs.keys.arrow.label();
    settings_group(ui, &format!("ARROWS (HOLD {} WHILE MOVING THE MOUSE)", arrow_key.to_uppercase()), |ui| {
        let arrows = &mut app.prefs.arrows;
        setting_row(
            ui,
            "Show arrows",
            "Live on screen while recording, in the editor, and in the export.",
            60.0,
            |ui| {
                settings_toggle(ui, &mut arrows.enabled);
            },
        );
        row_separator(ui);
        setting_row(
            ui,
            "Stays on screen",
            &format!("How long an arrow stays after you release {arrow_key}."),
            260.0,
            |ui| {
                ui.spacing_mut().slider_width = 180.0;
                ui.add(
                    egui::Slider::new(&mut arrows.hold_secs, 1.0..=10.0)
                        .step_by(0.5)
                        .custom_formatter(|v, _| format!("{v:.1} s")),
                );
            },
        );
        row_separator(ui);
        setting_row(ui, "Color", "", 260.0, |ui| {
            for preset in [
                [250, 250, 252, 255],
                [52, 190, 120, 255],
                [88, 136, 255, 255],
                [255, 196, 40, 255],
                [236, 52, 68, 255],
            ] {
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                let [r, g, b, _] = preset;
                ui.painter().circle_filled(rect.center(), 8.5, Color32::from_rgb(r, g, b));
                ui.painter()
                    .circle_stroke(rect.center(), 8.5, Stroke::new(1.0, theme::BORDER));
                if arrows.rgba == preset {
                    ui.painter()
                        .circle_stroke(rect.center(), 10.5, Stroke::new(1.5, theme::TEXT));
                }
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    arrows.rgba = preset;
                }
            }
        });
        row_separator(ui);
        setting_row(ui, "Thickness", "", 260.0, |ui| {
            ui.spacing_mut().slider_width = 180.0;
            ui.add(
                egui::Slider::new(&mut arrows.thickness, 3.0..=14.0)
                    .custom_formatter(|v, _| format!("{v:.0} px")),
            );
        });
    });

    settings_group(ui, "EXPORT CURSOR", |ui| {
        let cursor = &mut app.prefs.export_cursor;
        setting_row(
            ui,
            "Draw cursor on export",
            "Burn the telemetry cursor into exported videos.",
            60.0,
            |ui| {
                settings_toggle(ui, &mut cursor.burn_in);
            },
        );
        row_separator(ui);
        setting_row(ui, "Icon", "", 330.0, |ui| {
            let options: Vec<(crate::export::CursorIcon, &str)> = crate::export::CursorIcon::ALL
                .iter()
                .map(|i| (*i, i.label()))
                .collect();
            segmented(ui, &mut cursor.icon, &options);
        });
        row_separator(ui);
        setting_row(ui, "Size", "", 260.0, |ui| {
            ui.spacing_mut().slider_width = 180.0;
            ui.add(
                egui::Slider::new(&mut cursor.size, 0.5..=3.0)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            );
        });
        row_separator(ui);
        setting_row(
            ui,
            "Highlight",
            "Soft circle behind the cursor.",
            60.0,
            |ui| {
                settings_toggle(ui, &mut cursor.highlight);
            },
        );
    });

    ui.horizontal(|ui| {
        if button(ui, ButtonKind::Ghost, "Reset to factory defaults", true)
            .on_hover_text("Restore every setting, including the save folder")
            .clicked()
        {
            app.prefs = crate::prefs::Preferences::default();
        }
    });
}

fn settings_video_page(ui: &mut egui::Ui, app: &mut DexHowToApp) {
    settings_group(ui, "FORMAT", |ui| {
        setting_row(
            ui,
            "Frame rate",
            "60 fps looks smoother, especially for fast mouse movement.",
            160.0,
            |ui| {
                segmented(ui, &mut app.prefs.recorder.fps, &[(30, "30 fps"), (60, "60 fps")]);
            },
        );
        row_separator(ui);
        setting_row(
            ui,
            "Codec",
            "H.264 plays everywhere. HEVC makes smaller files at the same quality.",
            160.0,
            |ui| {
                segmented(
                    ui,
                    &mut app.prefs.recorder.codec,
                    &[(VideoCodec::H264, "H.264"), (VideoCodec::HEVC, "HEVC")],
                );
            },
        );
        row_separator(ui);
        setting_row(ui, "Container", "MP4 is the most widely supported.", 160.0, |ui| {
            segmented(
                ui,
                &mut app.prefs.recorder.format,
                &[(OutputFormat::MP4, "MP4"), (OutputFormat::MOV, "MOV")],
            );
        });
    });

    settings_group(ui, "QUALITY", |ui| {
        setting_row(
            ui,
            "Best quality",
            "Records at the display's full Retina resolution (for example 4K or 5K) with Display P3 color. Files are larger.",
            60.0,
            |ui| {
                settings_toggle(ui, &mut app.prefs.recorder.best_quality);
            },
        );
    });
}

fn settings_telemetry_page(ui: &mut egui::Ui, app: &mut DexHowToApp) {
    settings_group(ui, "", |ui| {
        setting_row(
            ui,
            "Save mouse telemetry",
            "Writes <recording name>.mouse.jsonl next to each video.",
            60.0,
            |ui| {
                settings_toggle(ui, &mut app.prefs.recorder.export_ai_telemetry);
            },
        );
    });

    settings_group(ui, "WHAT IS RECORDED", |ui| {
        for (title, detail) in [
            ("Pointer position", "Display pixels and normalized 0–1 coordinates, many times per second."),
            ("Clicks", "Left, right and middle button presses, releases, double clicks and drags."),
            ("Keys held", "Cmd, Shift, Ctrl and Alt while the mouse moves or clicks."),
            ("Timing", "Each record is matched to the video time and frame number."),
        ] {
            ui.horizontal_top(|ui| {
                let (dot, _) = ui.allocate_exact_size(Vec2::new(14.0, 18.0), Sense::hover());
                ui.painter()
                    .circle_filled(Pos2::new(dot.center().x, dot.top() + 9.0), 3.0, SettingsPage::Telemetry.tint());
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(title).size(12.5).color(theme::TEXT));
                    ui.label(RichText::new(detail).size(11.5).color(theme::TEXT_DIM));
                });
            });
            ui.add_space(6.0);
        }
    });

    egui::Frame::new()
        .fill(theme::ACCENT.gamma_multiply(0.1))
        .stroke(Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.45)))
        .corner_radius(10.0)
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new("Tip").size(12.5).strong().color(theme::ACCENT));
                ui.label(RichText::new("Hold").size(12.5).color(theme::TEXT));
                let keys = app.prefs.keys.clone();
                kbd(ui, keys.keep_movement.label());
                ui.label(RichText::new("or").size(12.5).color(theme::TEXT));
                kbd(ui, keys.arrow.label());
                ui.label(
                    RichText::new(format!(
                        "while moving the mouse to keep that exact movement when you use Clean Move in the editor. {} also draws an arrow. Change the keys in Defaults.",
                        keys.arrow.label()
                    ))
                    .size(12.5)
                    .color(theme::TEXT),
                );
            });
        });
}

fn settings_permissions_page(ui: &mut egui::Ui, app: &mut DexHowToApp) {
    let granted = crate::display::has_screen_capture_permission();
    settings_group(ui, "ACCESS", |ui| {
        setting_row(
            ui,
            "Screen Recording",
            "Required to capture the screen. After turning it on, quit and reopen the app.",
            230.0,
            |ui| {
                if button(ui, ButtonKind::Subtle, "Open Settings", true).clicked() {
                    open_screen_recording_settings();
                }
                if granted {
                    status_pill(ui, "Allowed", theme::SUCCESS);
                } else {
                    status_pill(ui, "Not allowed", theme::WARN);
                }
            },
        );
        row_separator(ui);
        setting_row(
            ui,
            "Microphone",
            "Needed only to record your voice.",
            230.0,
            |ui| {
                if button(ui, ButtonKind::Subtle, "Open Settings", true).clicked() {
                    open_microphone_settings();
                }
            },
        );
    });

    settings_group(ui, "TROUBLESHOOTING", |ui| {
        let count = app.displays.len();
        setting_row(
            ui,
            "Displays",
            &format!("{count} display{} found.", if count == 1 { "" } else { "s" }),
            230.0,
            |ui| {
                if button(ui, ButtonKind::Subtle, "Re-scan", true).clicked() {
                    app.refresh_displays();
                }
            },
        );
        row_separator(ui);
        setting_row(
            ui,
            "Reset permission",
            "If macOS says access is missing although it is turned on, reset it and allow it again.",
            230.0,
            |ui| {
                if button(ui, ButtonKind::Tinted(theme::CUT), "Reset", true).clicked() {
                    reset_screen_recording_permission();
                    app.refresh_displays();
                }
            },
        );
    });
}
