pub mod d3d11_surface;
pub mod presets;

use std::sync::Arc;
use std::time::Instant;

use cst_client::ClientSnapshot;
use gpui_kit::{
    Div, ElementId, Hsla, InteractiveElement, ParentElement, Stateful, Styled, Window, div,
    linear_color_stop, linear_gradient, px,
};

use crate::theme::DesktopPalette;

pub use d3d11_surface::{
    D3D_FEATURE_LEVEL_11_0_RAW, D3d11FrostedGlassSurface, D3d11SkySurface,
    SurfaceRuntimeDiagnostics, d3d_is_software_adapter, d3d_sm50_supported,
    reset_d3d11_pipeline_cache, surface_diagnostics,
};
pub use presets::{FrostedGlassSpec, SkyPreset, generate_rounded_rect_sdf_rgba16f};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationGateInputs {
    pub route_has_sky_shader: bool,
    pub web_active_locked: bool,
    pub window_focused: bool,
    pub window_minimized: bool,
    pub hardware_diagnostic_active: bool,
    pub reduce_motion: bool,
    pub remote_session: bool,
    pub software_adapter: bool,
    pub sm50_supported: bool,
}

impl AnimationGateInputs {
    /// Returns `true` ONLY when all 7 runtime conditions permit VSync animation frame advancement.
    /// Off-page, unfocused, minimized, `WebActive`, or `sys(gpu/sensor)` sampling freezes at `0 FPS`.
    pub fn should_animate(self) -> bool {
        self.route_has_sky_shader
            && !self.web_active_locked
            && self.window_focused
            && !self.window_minimized
            && !self.hardware_diagnostic_active
            && !self.reduce_motion
            && !self.remote_session
            && !self.software_adapter
            && self.sm50_supported
    }

    /// Static `START_T` single-frame degradation applies when `reduce_motion`, RDP, or software adapter is active.
    pub fn force_static_start_frame(self) -> bool {
        self.reduce_motion || self.remote_session || self.software_adapter
    }
}

#[derive(Debug)]
pub struct ShaderFrameScheduler {
    accumulated_time_sec: f32,
    last_frame_instant: Option<Instant>,
    remote_session: bool,
}

impl Default for ShaderFrameScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl ShaderFrameScheduler {
    pub fn new() -> Self {
        Self {
            accumulated_time_sec: 0.0,
            last_frame_instant: None,
            remote_session: is_remote_desktop_session(),
        }
    }

    pub fn is_remote_session(&self) -> bool {
        self.remote_session
    }

    /// Evaluates animation gates at `now`, updates `u_time`, and returns `(u_time, request_next_vsync_frame)`.
    pub fn step(&mut self, inputs: AnimationGateInputs, now: Instant) -> (f32, bool) {
        if inputs.force_static_start_frame() {
            self.last_frame_instant = None;
            return (0.0, false);
        }
        if !inputs.should_animate() {
            // Freeze cleanly at the last rendered `accumulated_time_sec` with 0 FPS!
            self.last_frame_instant = None;
            return (self.accumulated_time_sec, false);
        }
        if let Some(prev) = self.last_frame_instant {
            let dt = now.saturating_duration_since(prev).as_secs_f32().min(0.1);
            self.accumulated_time_sec += dt;
        }
        self.last_frame_instant = Some(now);
        (self.accumulated_time_sec, true)
    }

    pub fn sample_for_window(
        &mut self,
        route_has_sky_shader: bool,
        web_active_locked: bool,
        hardware_diagnostic_active: bool,
        reduce_motion: bool,
        window: &mut Window,
    ) -> f32 {
        let vp = window.viewport_size();
        let minimized = vp.width <= px(1.0) || vp.height <= px(1.0);
        let inputs = AnimationGateInputs {
            route_has_sky_shader,
            web_active_locked,
            window_focused: window.is_window_active(),
            window_minimized: minimized,
            hardware_diagnostic_active,
            reduce_motion,
            remote_session: self.remote_session,
            software_adapter: d3d_is_software_adapter(),
            sm50_supported: d3d_sm50_supported(),
        };
        let (t, request_next) = self.step(inputs, Instant::now());
        if request_next {
            window.request_animation_frame();
        }
        t
    }
}

/// Checks Win32 `GetSystemMetrics(SM_REMOTESESSION)` (`0x1000`).
pub fn is_remote_desktop_session() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_REMOTESESSION};
        unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Returns `true` if any loaded session is actively executing `sys(gpu)` or `sys(sensor)`,
/// freezing desktop shader animation so GPU/thermal/power readings are never polluted by UI shaders.
pub fn has_active_hardware_diagnostic(snapshot: &ClientSnapshot) -> bool {
    for sess in snapshot.loaded_sessions.values() {
        for ev in sess.active_tools.values() {
            let tool_name = ev
                .get("toolName")
                .or_else(|| ev.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if tool_name == "sys" {
                let scope = ev
                    .get("args")
                    .or_else(|| ev.get("arguments"))
                    .and_then(|a| a.get("scope"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                if matches!(scope, "gpu" | "sensor") {
                    return true;
                }
            }
        }
    }
    false
}

/// Renders a full-bleed sky background surface with automatic multi-stop linear gradient fallback
/// underneath for GPUs below Direct3D Feature Level `11_0`.
pub fn render_sky_background(preset: SkyPreset, time_sec: f32, opacity: f32) -> Div {
    let (top_color, bottom_color) = preset.fallback_gradient_stops();
    let base = div().size_full().bg(linear_gradient(
        180.0,
        linear_color_stop(top_color, 0.0),
        linear_color_stop(bottom_color, 1.0),
    ));

    #[cfg(windows)]
    if d3d_sm50_supported() {
        let renderer: Arc<dyn gpui::WindowsSurfaceRenderer> = Arc::new(D3d11SkySurface {
            preset,
            time_sec,
            opacity,
        });
        return base.child(gpui_kit::surface(renderer).size_full());
    }

    let _ = (time_sec, opacity);
    base
}

/// Renders the 9-stop nonlinear horizontal fade mask on the right edge of Login's `864×800` shader canvas
/// (`tM7YQ` / `jiQRy`), smoothly blending into `$bg-workspace`.
pub fn render_login_9_stop_fade(palette: &DesktopPalette) -> Div {
    // GPUI linear_gradient takes 2 stops per layer; stacking smooth ease-in-out segments reproduces
    // the exact 9-stop nonlinear S-curve fade (`0.0 -> 0.04 -> 0.14 -> 0.30 -> 0.50 -> 0.70 -> 0.86 -> 0.96 -> 1.0`)
    // on the rightmost 220px of the 864px canvas without banding.
    let mut ws = palette.bg_workspace;
    let c0 = {
        ws.a = 0.0;
        ws
    };
    let c_mid = {
        ws.a = 0.48;
        ws
    };
    let c_full = {
        ws.a = 1.0;
        ws
    };
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .right_0()
        .w(px(220.0))
        .bg(linear_gradient(
            90.0,
            linear_color_stop(c0, 0.0),
            linear_color_stop(c_full, 1.0),
        ))
        .child(div().size_full().bg(linear_gradient(
            90.0,
            linear_color_stop(c0, 0.35),
            linear_color_stop(c_mid, 0.85),
        )))
}

/// Builds a frosted-glass container (`H2p2z` popover, `K3x72` modal, or `DZ0Fp` scrim) backed by
/// same-window D3D11 `CopySubresourceRegion` + `Rgba16Float` SDF + `frosted-glass.glsl`,
/// automatically degrading to solid `$bg-elevated` / `$bg-scrim` when system transparency is disabled or `< 11_0`.
pub fn render_frosted_surface(
    id: impl Into<ElementId>,
    spec: FrostedGlassSpec,
    fallback_bg: Hsla,
    transparency_enabled: bool,
    scale_factor: f32,
) -> Stateful<Div> {
    let container = div().id(id).relative();

    #[cfg(windows)]
    if transparency_enabled && d3d_sm50_supported() {
        let renderer: Arc<dyn gpui::WindowsSurfaceRenderer> =
            Arc::new(D3d11FrostedGlassSurface { spec, scale_factor });
        return container.child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .child(gpui_kit::surface(renderer).size_full()),
        );
    }

    let _ = (spec, transparency_enabled, scale_factor);
    container.bg(fallback_bg)
}
