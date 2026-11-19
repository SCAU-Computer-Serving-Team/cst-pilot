use std::rc::Rc;

use cst_client::{DesktopSettingsDto, DesktopSettingsPatchDto};
use gpui_kit::{
    App, Bounds, DisplayId, Pixels, PlatformDisplay, Point, Size, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowOptions, point, px, size,
};
use sha2::{Digest, Sha256};

use crate::theme::layout::{
    DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH,
};

pub const GEOMETRY_DEBOUNCE_MS: u64 = 300;
const MIN_DRAG_INTERSECTION_WIDTH: f32 = 120.0;
const MIN_DRAG_INTERSECTION_HEIGHT: f32 = 24.0;
const DRAG_BAR_HEIGHT: f32 = 40.0;

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayWorkArea {
    pub display_id: Option<DisplayId>,
    pub hint: String,
    pub work_x: f32,
    pub work_y: f32,
    pub work_width: f32,
    pub work_height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedWindowGeometry {
    pub display_id: Option<DisplayId>,
    pub display_hint: Option<String>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_width: f32,
    pub min_height: f32,
    pub maximized: bool,
}

impl ResolvedWindowGeometry {
    pub fn to_gpui_bounds(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.x), px(self.y)),
            size(px(self.width), px(self.height)),
        )
    }

    pub fn to_window_options(&self) -> WindowOptions {
        let bounds = self.to_gpui_bounds();
        let window_bounds = if self.maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        };
        WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: Some(TitlebarOptions {
                title: Some("CST Pilot".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            focus: true,
            show: true,
            is_movable: true,
            is_resizable: true,
            is_minimizable: true,
            inactive_frame_interval: None,
            display_id: self.display_id,
            window_background: WindowBackgroundAppearance::Opaque,
            window_min_size: Some(Size {
                width: px(self.min_width),
                height: px(self.min_height),
            }),
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some("cst-pilot".to_owned()),
            ..WindowOptions::default()
        }
    }
}

/// Computes a stable 16-hex SHA-256 hint from a display's persistent identifier (never raw `HMONITOR`).
pub fn compute_display_hint(raw_identity: &str) -> String {
    let digest = Sha256::digest(raw_identity.as_bytes());
    let mut hex = String::with_capacity(16);
    for b in &digest[..8] {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

pub fn collect_app_displays(cx: &App) -> Vec<DisplayWorkArea> {
    let mut out = Vec::new();
    for (idx, disp) in cx.displays().into_iter().enumerate() {
        out.push(display_to_work_area(idx, &disp));
    }
    out
}

fn display_to_work_area(index: usize, disp: &Rc<dyn PlatformDisplay>) -> DisplayWorkArea {
    let vis = disp.visible_bounds();
    let vx = f32::from(vis.origin.x);
    let vy = f32::from(vis.origin.y);
    let vw = f32::from(vis.size.width);
    let vh = f32::from(vis.size.height);
    let raw_id = disp.uuid().map(|u| u.to_string()).unwrap_or_else(|_| {
        format!(
            "display:{index}:{}:{}:{}:{}",
            vx.round() as i32,
            vy.round() as i32,
            vw.round() as i32,
            vh.round() as i32
        )
    });
    DisplayWorkArea {
        display_id: Some(disp.id()),
        hint: compute_display_hint(&raw_id),
        work_x: vx,
        work_y: vy,
        work_width: vw.max(320.0),
        work_height: vh.max(240.0),
    }
}

/// Resolves startup window bounds on the matched display (or primary display fallback),
/// clamping dimensions into `[min(1024×640, work_area), work_area * 0.90]` and verifying
/// that the top `40px` drag region intersects `work_area` by at least `120×24px`.
pub fn resolve_startup_window_geometry(
    settings: &DesktopSettingsDto,
    displays: &[DisplayWorkArea],
) -> ResolvedWindowGeometry {
    let fallback = DisplayWorkArea {
        display_id: None,
        hint: compute_display_hint("primary-fallback"),
        work_x: 0.0,
        work_y: 0.0,
        work_width: 1920.0,
        work_height: 1080.0,
    };
    let target = settings
        .display_hint
        .as_deref()
        .and_then(|hint| displays.iter().find(|d| d.hint == hint))
        .or_else(|| displays.first())
        .unwrap_or(&fallback);

    let min_w = MIN_WINDOW_WIDTH.min(target.work_width);
    let min_h = MIN_WINDOW_HEIGHT.min(target.work_height);
    let max_w = (target.work_width * 0.90).floor().max(min_w);
    let max_h = (target.work_height * 0.90).floor().max(min_h);

    let req_w = settings
        .window_width
        .map(|w| w as f32)
        .unwrap_or(DEFAULT_WINDOW_WIDTH);
    let req_h = settings
        .window_height
        .map(|h| h as f32)
        .unwrap_or(DEFAULT_WINDOW_HEIGHT);
    let width = req_w.clamp(min_w, max_w);
    let height = req_h.clamp(min_h, max_h);

    let centered_x = (target.work_x + (target.work_width - width) * 0.5).round();
    let centered_y = (target.work_y + (target.work_height - height) * 0.5).round();

    let (mut x, mut y) = match (settings.window_x, settings.window_y) {
        (Some(rx), Some(ry)) => (rx as f32, ry as f32),
        _ => (centered_x, centered_y),
    };

    // Verify top 40px drag strip visible intersection against `target` work area
    let inter_left = x.max(target.work_x);
    let inter_right = (x + width).min(target.work_x + target.work_width);
    let inter_top = y.max(target.work_y);
    let inter_bottom = (y + DRAG_BAR_HEIGHT).min(target.work_y + target.work_height);
    let inter_w = (inter_right - inter_left).max(0.0);
    let inter_h = (inter_bottom - inter_top).max(0.0);

    if inter_w < MIN_DRAG_INTERSECTION_WIDTH || inter_h < MIN_DRAG_INTERSECTION_HEIGHT {
        x = centered_x;
        y = centered_y;
    }

    let _ = Point::<Pixels>::default();
    ResolvedWindowGeometry {
        display_id: target.display_id,
        display_hint: Some(target.hint.clone()),
        x,
        y,
        width,
        height,
        min_width: min_w,
        min_height: min_h,
        maximized: settings.window_maximized,
    }
}

/// Tracks normal (non-maximized, non-minimized) window bounds so maximized/minimized transitions
/// never overwrite the user's normal restore rectangle in `desktop.json`.
#[derive(Debug, Clone)]
pub struct NormalBoundsTracker {
    normal_x: i32,
    normal_y: i32,
    normal_width: u32,
    normal_height: u32,
    display_hint: Option<String>,
    maximized: bool,
    initialized: bool,
    pending_patch: Option<DesktopSettingsPatchDto>,
}

impl NormalBoundsTracker {
    pub fn from_resolved(resolved: &ResolvedWindowGeometry) -> Self {
        Self {
            normal_x: resolved.x.round() as i32,
            normal_y: resolved.y.round() as i32,
            normal_width: resolved.width.round().max(320.0) as u32,
            normal_height: resolved.height.round().max(240.0) as u32,
            display_hint: resolved.display_hint.clone(),
            maximized: resolved.maximized,
            initialized: false,
            pending_patch: None,
        }
    }

    /// Observes a window geometry notification (`bounds`, `is_maximized`, `is_minimized`, `display_hint`).
    /// Returns `true` when a user-driven geometry change occurred after initial window creation.
    pub fn observe(
        &mut self,
        bounds: Bounds<Pixels>,
        is_maximized: bool,
        is_minimized: bool,
        display_hint: Option<String>,
    ) -> bool {
        let bx = f32::from(bounds.origin.x);
        let by = f32::from(bounds.origin.y);
        let bw = f32::from(bounds.size.width);
        let bh = f32::from(bounds.size.height);
        if is_minimized || bw <= 1.0 || bh <= 1.0 {
            return false;
        }
        if !self.initialized {
            // Startup clamping stays in memory until the user actually moves/resizes after first frame!
            self.initialized = true;
            if !is_maximized {
                self.normal_x = bx.round() as i32;
                self.normal_y = by.round() as i32;
                self.normal_width = bw.round().max(320.0) as u32;
                self.normal_height = bh.round().max(240.0) as u32;
            }
            self.maximized = is_maximized;
            if display_hint.is_some() {
                self.display_hint = display_hint;
            }
            return false;
        }

        let mut changed = false;
        if self.maximized != is_maximized {
            self.maximized = is_maximized;
            changed = true;
        }
        if !is_maximized {
            let nx = bx.round() as i32;
            let ny = by.round() as i32;
            let nw = bw.round().max(320.0) as u32;
            let nh = bh.round().max(240.0) as u32;
            if (nx, ny, nw, nh)
                != (
                    self.normal_x,
                    self.normal_y,
                    self.normal_width,
                    self.normal_height,
                )
            {
                self.normal_x = nx;
                self.normal_y = ny;
                self.normal_width = nw;
                self.normal_height = nh;
                changed = true;
            }
        }
        if display_hint.is_some() && display_hint != self.display_hint {
            self.display_hint = display_hint;
            changed = true;
        }

        if changed {
            self.pending_patch = Some(DesktopSettingsPatchDto {
                display_hint: Some(self.display_hint.clone()),
                window_width: Some(Some(self.normal_width)),
                window_height: Some(Some(self.normal_height)),
                window_x: Some(Some(self.normal_x)),
                window_y: Some(Some(self.normal_y)),
                window_maximized: Some(self.maximized),
                ..DesktopSettingsPatchDto::default()
            });
        }
        changed
    }

    pub fn take_pending_patch(&mut self) -> Option<DesktopSettingsPatchDto> {
        self.pending_patch.take()
    }
}
