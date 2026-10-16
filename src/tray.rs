use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub enum TrayAction {
    ToggleRecord,
    ShowWindow,
    OpenSettings,
    OpenEditor,
    Quit,
}

pub struct TrayManager {
    tray_icon: TrayIcon,
    header_item: MenuItem,
    toggle_item: MenuItem,
    #[allow(dead_code)]
    show_item: MenuItem,
    #[allow(dead_code)]
    open_editor_item: MenuItem,
    #[allow(dead_code)]
    settings_item: MenuItem,
    #[allow(dead_code)]
    quit_item: MenuItem,
    idle_icon: Icon,
    recording_icon: Icon,
}

impl TrayManager {
    pub fn new() -> Option<Self> {
        let idle_icon = create_idle_icon();
        let recording_icon = create_recording_icon();

        let menu = Menu::new();
        let header_item = MenuItem::with_id("header", "📹 DexHowTo Recorder (Ready)", false, None);
        let toggle_item = MenuItem::with_id("toggle_record", "🔴 Start Recording", true, None);
        let show_item = MenuItem::with_id("show_window", "👁️ Show Recorder Dock", true, None);
        let open_editor_item = MenuItem::with_id("open_editor", "✂️ Open Video in Editor...", true, None);
        let settings_item = MenuItem::with_id("open_settings", "⚙️ Settings...", true, None);
        let separator = PredefinedMenuItem::separator();
        let quit_item = MenuItem::with_id("quit", "❌ Quit", true, None);

        let _ = menu.append_items(&[
            &header_item,
            &toggle_item,
            &show_item,
            &open_editor_item,
            &settings_item,
            &separator,
            &quit_item,
        ]);

        let tray_icon = match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(idle_icon.clone())
            .with_tooltip("DexHowTo Recorder")
            .with_menu_on_left_click(true)
            .build()
        {
            Ok(tray) => tray,
            Err(e) => {
                eprintln!("Failed to initialize system tray icon: {e:?}");
                return None;
            }
        };

        Some(Self {
            tray_icon,
            header_item,
            toggle_item,
            show_item,
            open_editor_item,
            settings_item,
            quit_item,
            idle_icon,
            recording_icon,
        })
    }

    pub fn set_countdown(&self, remaining_secs: u32) {
        let _ = self.tray_icon.set_icon(Some(self.recording_icon.clone()));
        self.tray_icon.set_title(Some(format!("⏳ {}s", remaining_secs)));
        self.header_item.set_text(format!("⏳ Starting in {}s...", remaining_secs));
        self.toggle_item.set_text("⏹ Cancel Countdown");
    }

    pub fn set_recording(&self, duration_str: &str) {
        let _ = self.tray_icon.set_icon(Some(self.recording_icon.clone()));
        self.tray_icon.set_title(Some(format!("🔴 {}", duration_str)));
        self.header_item.set_text(format!("🔴 Recording: {}", duration_str));
        self.toggle_item.set_text("⏹ Stop Recording");
    }

    pub fn set_idle(&self) {
        let _ = self.tray_icon.set_icon(Some(self.idle_icon.clone()));
        self.tray_icon.set_title(None::<&str>);
        self.header_item.set_text("📹 DexHowTo Recorder (Ready)");
        self.toggle_item.set_text("🔴 Start Recording");
    }

    pub fn check_events(&self) -> Option<TrayAction> {
        // Check clicks on menu items
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == MenuId::new("toggle_record") {
                return Some(TrayAction::ToggleRecord);
            } else if event.id == MenuId::new("show_window") {
                return Some(TrayAction::ShowWindow);
            } else if event.id == MenuId::new("open_editor") {
                return Some(TrayAction::OpenEditor);
            } else if event.id == MenuId::new("open_settings") {
                return Some(TrayAction::OpenSettings);
            } else if event.id == MenuId::new("quit") {
                return Some(TrayAction::Quit);
            }
        }

        // Check clicks on tray icon itself
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click { .. } = event {
                // If clicked, default action is to show window if not showing menu
                // On macOS, menu opens on left click if configured
            }
        }

        None
    }
}

/// Create a 32x32 camera icon for the idle system tray state
fn create_idle_icon() -> Icon {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];

    for y in 0..size {
        for x in 0..size {
            let idx = ((y * size + x) * 4) as usize;

            // Camera body: rounded rectangle from x: 5..20, y: 9..23
            let in_body = x >= 5 && x <= 20 && y >= 9 && y <= 23;

            // Camera lens: small trapezoid on the right from x: 21..27
            let in_lens = if x >= 21 && x <= 27 {
                let center_y = 16i32;
                let half_h = (x as i32 - 18) * 3 / 4 + 1;
                (y as i32 - center_y).abs() <= half_h
            } else {
                false
            };

            if in_body || in_lens {
                rgba[idx] = 255;
                rgba[idx + 1] = 255;
                rgba[idx + 2] = 255;
                rgba[idx + 3] = 230;
            }
        }
    }

    Icon::from_rgba(rgba, size, size).expect("valid idle icon")
}

/// Create a 32x32 vibrant red recording dot icon with a clean circular boundary
fn create_recording_icon() -> Icon {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let center = 15.5f32;
    let radius = 10.0f32;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = ((y * size + x) * 4) as usize;

            if dist <= radius - 0.5 {
                // Bright macOS recording red (#FF3B30)
                rgba[idx] = 255;
                rgba[idx + 1] = 59;
                rgba[idx + 2] = 48;
                rgba[idx + 3] = 255;
            } else if dist < radius + 1.2 {
                // Anti-aliased outer edge
                let alpha = ((radius + 1.2 - dist) / 1.7).clamp(0.0, 1.0);
                rgba[idx] = 255;
                rgba[idx + 1] = 59;
                rgba[idx + 2] = 48;
                rgba[idx + 3] = (alpha * 255.0) as u8;
            }
        }
    }

    Icon::from_rgba(rgba, size, size).expect("valid recording icon")
}
