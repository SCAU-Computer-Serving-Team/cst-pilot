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
pub use presets::{FrostedGlassSpec, SkyPreset};

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
        let minimized = !window.is_visible() || vp.width <= px(1.0) || vp.height <= px(1.0);
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
    render_sky_background_rounded(preset, time_sec, opacity, [0.0; 4], 1.0)
}

/// Renders a sky background surface clipped to per-corner logical radii (`[tl, tr, br, bl]`).
pub fn render_sky_background_rounded(
    preset: SkyPreset,
    time_sec: f32,
    opacity: f32,
    corner_radii_logical: [f32; 4],
    scale_factor: f32,
) -> Div {
    let clamped_opacity = opacity.clamp(0.0, 1.0);
    let sf = scale_factor.max(1.0);
    let base = div()
        .size_full()
        .rounded_tl(px(corner_radii_logical[0]))
        .rounded_tr(px(corner_radii_logical[1]))
        .rounded_br(px(corner_radii_logical[2]))
        .rounded_bl(px(corner_radii_logical[3]));

    #[cfg(windows)]
    if d3d_sm50_supported() {
        let renderer: Arc<dyn gpui::WindowsSurfaceRenderer> = Arc::new(D3d11SkySurface {
            preset,
            time_sec,
            opacity: clamped_opacity,
            corner_radii_phys: [
                corner_radii_logical[0] * sf,
                corner_radii_logical[1] * sf,
                corner_radii_logical[2] * sf,
                corner_radii_logical[3] * sf,
            ],
        });
        // Skip the opaque fallback gradient while the D3D11 surface is semi-transparent during
        // route transitions so it alpha-blends cleanly against `$bg-workspace` under feather masks.
        let surface_wrap = if clamped_opacity >= 0.999 {
            let (top_color, bottom_color) = preset.fallback_gradient_stops();
            base.bg(linear_gradient(
                180.0,
                linear_color_stop(top_color, 0.0),
                linear_color_stop(bottom_color, 1.0),
            ))
        } else {
            base
        };
        return surface_wrap.child(gpui_kit::surface(renderer).size_full());
    }

    let (mut top_color, mut bottom_color) = preset.fallback_gradient_stops();
    top_color.a *= clamped_opacity;
    bottom_color.a *= clamped_opacity;
    let _ = time_sec;
    base.bg(linear_gradient(
        180.0,
        linear_color_stop(top_color, 0.0),
        linear_color_stop(bottom_color, 1.0),
    ))
}

/// Renders the 9-stop nonlinear horizontal fade mask on the right side of Login's `864×800` shader canvas
/// (`V8hpnD` `524px` in light mode / `rTIhI` `504px` in dark mode), smoothly blending into `$bg-workspace`.
pub fn render_login_9_stop_fade(palette: &DesktopPalette) -> Div {
    let total_w: f32 = if palette.mode.is_dark() { 504.0 } else { 524.0 };
    let stops: &[(f32, f32)] = if palette.mode.is_dark() {
        &[
            (0.00, 0.00),
            (0.14, 0x1C as f32 / 255.0),
            (0.30, 0x47 as f32 / 255.0),
            (0.46, 0x7D as f32 / 255.0),
            (0.62, 0xB5 as f32 / 255.0),
            (0.77, 0xE0 as f32 / 255.0),
            (0.89, 0xF7 as f32 / 255.0),
            (0.97, 1.00),
            (1.00, 1.00),
        ]
    } else {
        &[
            (0.00, 0.00),
            (0.14, 0x17 as f32 / 255.0),
            (0.30, 0x3D as f32 / 255.0),
            (0.46, 0x70 as f32 / 255.0),
            (0.62, 0xA8 as f32 / 255.0),
            (0.77, 0xD6 as f32 / 255.0),
            (0.89, 0xF5 as f32 / 255.0),
            (0.97, 1.00),
            (1.00, 1.00),
        ]
    };

    let mut row = div()
        .absolute()
        .top_0()
        .bottom_0()
        .right_0()
        .w(px(total_w))
        .flex()
        .flex_row();

    let last_idx = stops.len().saturating_sub(2);
    for (idx, pair) in stops.windows(2).enumerate() {
        let (p0, a0) = pair[0];
        let (p1, a1) = pair[1];
        let seg_w = ((p1 - p0) * total_w).max(1.0);
        let is_solid_tail = idx == last_idx && a0 >= 1.0 && a1 >= 1.0;
        let seg = if is_solid_tail {
            div()
                .flex_1()
                .min_w(px(seg_w))
                .h_full()
                .bg(palette.bg_workspace)
        } else {
            let mut c0 = palette.bg_workspace;
            c0.a = a0;
            let mut c1 = palette.bg_workspace;
            c1.a = a1;
            div().w(px(seg_w)).h_full().bg(linear_gradient(
                90.0,
                linear_color_stop(c0, 0.0),
                linear_color_stop(c1, 1.0),
            ))
        };
        row = row.child(seg);
    }
    row
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
    let is_elevated_card = spec.specular > 0.0;
    let is_light_tint = spec.tint_rgb[0] > 0.5;
    let shadow_contact: Hsla = gpui_kit::rgba(if is_light_tint {
        0x1111_110F
    } else {
        0x0000_003D
    })
    .into();
    let shadow_mid: Hsla = gpui_kit::rgba(if is_light_tint {
        0x1111_110F
    } else {
        0x0000_0047
    })
    .into();
    let shadow_lg: Hsla = gpui_kit::rgba(if is_light_tint {
        0x1111_1117
    } else {
        0x0000_0061
    })
    .into();
    let mut highlight_top: Hsla = gpui_kit::rgba(if is_light_tint {
        0xFFFF_FFF2
    } else {
        0xFFFF_FF1F
    })
    .into();
    highlight_top.a *= spec.opacity.clamp(0.0, 1.0);

    let mut container = div().id(id).relative();
    if is_elevated_card {
        container = container.shadow(vec![
            gpui_kit::BoxShadow {
                color: highlight_top,
                offset: gpui_kit::point(px(0.0), px(1.0)),
                blur_radius: px(0.0),
                spread_radius: px(0.0),
                inset: true,
            },
            gpui_kit::BoxShadow {
                color: shadow_contact,
                offset: gpui_kit::point(px(0.0), px(1.0)),
                blur_radius: px(if is_light_tint { 2.0 } else { 3.0 }),
                spread_radius: px(0.0),
                inset: false,
            },
            gpui_kit::BoxShadow {
                color: shadow_mid,
                offset: gpui_kit::point(px(0.0), px(6.0)),
                blur_radius: px(14.0),
                spread_radius: px(if is_light_tint { -2.0 } else { -1.0 }),
                inset: false,
            },
            gpui_kit::BoxShadow {
                color: shadow_lg,
                offset: gpui_kit::point(px(0.0), px(if is_light_tint { 18.0 } else { 16.0 })),
                blur_radius: px(if is_light_tint { 36.0 } else { 34.0 }),
                spread_radius: px(if is_light_tint { -6.0 } else { -4.0 }),
                inset: false,
            },
        ]);
    }

    #[cfg(windows)]
    if transparency_enabled && d3d_sm50_supported() {
        let renderer: Arc<dyn gpui::WindowsSurfaceRenderer> =
            Arc::new(D3d11FrostedGlassSurface { spec, scale_factor });
        let mut out = container.child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .child(gpui_kit::surface(renderer).size_full()),
        );
        if is_elevated_card {
            out = out.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(spec.corner_radius_px * 0.5))
                    .right(px(spec.corner_radius_px * 0.5))
                    .h(px(1.0))
                    .bg(highlight_top),
            );
        }
        return out;
    }

    let _ = (spec, transparency_enabled, scale_factor);
    container.bg(fallback_bg)
}
