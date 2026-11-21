//! Read-only UX & interaction-performance probe for desktop E2E verification (`doc/desktop/test/ux-e2e.md`).
//!
//! Strictly compiled only under `#[cfg(feature = "test-support")]` and enabled only when
//! `CST_DESKTOP_UX_PROBE=1` is explicitly set in the process environment. Default builds compile
//! every hook away to an empty inline no-op with zero runtime overhead.

#[cfg(feature = "test-support")]
mod imp {
    use std::collections::{BTreeMap, VecDeque};
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use gpui_kit::{IntoElement, Styled as _};
    use serde::Serialize;

    use crate::background::{SurfaceRuntimeDiagnostics, surface_diagnostics};

    const MAX_FRAME_SAMPLES: usize = 2048;
    const MAX_INPUT_SAMPLES: usize = 256;
    const MAX_HEADER_LINES: usize = 64;

    static PROBE_ENABLED: AtomicBool = AtomicBool::new(false);
    static PROBE_PORT: AtomicU16 = AtomicU16::new(0);
    static PROCESS_ORIGIN: OnceLock<Instant> = OnceLock::new();
    static FORCED_WARP_ENV: OnceLock<bool> = OnceLock::new();
    static FORCED_WARP_OVERRIDE: AtomicBool = AtomicBool::new(false);
    static RUNNING_LIGHT_FRAMES: AtomicU64 = AtomicU64::new(0);
    static PROBE_STATE: OnceLock<Mutex<ProbeInner>> = OnceLock::new();
    static SERVER_STARTED: AtomicBool = AtomicBool::new(false);

    #[derive(Debug, Clone, Default, Serialize)]
    pub struct StartupMilestones {
        pub bootstrap_done_ms: Option<f64>,
        pub fonts_loaded_ms: Option<f64>,
        pub window_opened_ms: Option<f64>,
        pub first_frame_ms: Option<f64>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct InputLatencySample {
        pub label: String,
        pub latency_ms: f64,
        pub at_ms: f64,
    }

    #[derive(Debug, Clone, Default, Serialize)]
    pub struct ScrollProbeState {
        pub offset_y: f32,
        pub max_offset_y: f32,
        pub auto_follow_bottom: bool,
        pub show_scroll_bottom_btn: bool,
    }

    #[derive(Debug, Clone, Default, Serialize)]
    pub struct ComputedStyleState {
        pub composer_focused: bool,
        pub composer_border_rgba: [f32; 4],
        pub composer_shadow_count: usize,
    }

    #[derive(Debug, Clone, Default, Serialize)]
    pub struct ShellProbeState {
        pub route: String,
        pub active_session_id: Option<String>,
        pub viewport_width: f32,
        pub viewport_height: f32,
        pub scale_factor: f32,
        pub sidebar_collapsed: bool,
        pub window_active: bool,
        pub reduce_motion: bool,
        pub transparency_enabled: bool,
        pub web_active: bool,
        pub forced_warp: bool,
    }

    #[derive(Debug)]
    struct ProbeInner {
        startup: StartupMilestones,
        total_frames: u64,
        window_frames: u64,
        last_frame_at: Option<Instant>,
        frame_render_us: VecDeque<u64>,
        frame_paint_us: VecDeque<u64>,
        frame_interval_us: VecDeque<u64>,
        frames_over_16ms: u64,
        frames_over_33ms: u64,
        pending_input: Option<(String, Instant)>,
        input_samples: VecDeque<InputLatencySample>,
        baseline_sky_draws: u64,
        baseline_frosted_draws: u64,
        baseline_running_light: u64,
        shell: ShellProbeState,
        scroll: ScrollProbeState,
        bounds: BTreeMap<String, [f32; 4]>,
        styles: ComputedStyleState,
    }

    impl Default for ProbeInner {
        fn default() -> Self {
            Self {
                startup: StartupMilestones::default(),
                total_frames: 0,
                window_frames: 0,
                last_frame_at: None,
                frame_render_us: VecDeque::with_capacity(MAX_FRAME_SAMPLES),
                frame_paint_us: VecDeque::with_capacity(MAX_FRAME_SAMPLES),
                frame_interval_us: VecDeque::with_capacity(MAX_FRAME_SAMPLES),
                frames_over_16ms: 0,
                frames_over_33ms: 0,
                pending_input: None,
                input_samples: VecDeque::with_capacity(MAX_INPUT_SAMPLES),
                baseline_sky_draws: 0,
                baseline_frosted_draws: 0,
                baseline_running_light: 0,
                shell: ShellProbeState::default(),
                scroll: ScrollProbeState::default(),
                bounds: BTreeMap::new(),
                styles: ComputedStyleState::default(),
            }
        }
    }

    #[derive(Serialize)]
    struct FramePacingReport {
        total_frames: u64,
        window_frames: u64,
        last_frame_ms_ago: Option<f64>,
        render_us_avg: f64,
        render_us_p50: u64,
        render_us_p95: u64,
        render_us_p99: u64,
        render_us_max: u64,
        paint_us_avg: f64,
        paint_us_p50: u64,
        paint_us_p95: u64,
        paint_us_p99: u64,
        paint_us_max: u64,
        interval_us_p50: u64,
        interval_us_p95: u64,
        frames_over_16ms: u64,
        frames_over_33ms: u64,
    }

    #[derive(Serialize)]
    struct InputLatencyReport {
        count: usize,
        p50_ms: f64,
        p95_ms: f64,
        max_ms: f64,
        recent: Vec<InputLatencySample>,
    }

    #[derive(Serialize)]
    struct SurfaceDeltaReport {
        supports_sm50: bool,
        is_software_adapter: bool,
        sky_draw_count: u64,
        frosted_draw_count: u64,
        device_build_count: u64,
        sky_draw_delta: u64,
        frosted_draw_delta: u64,
        running_light_frames: u64,
        running_light_delta: u64,
    }

    #[derive(Serialize)]
    struct ProbeSnapshot {
        pid: u32,
        uptime_ms: f64,
        startup: StartupMilestones,
        frame_pacing: FramePacingReport,
        input_latency: InputLatencyReport,
        surface: SurfaceDeltaReport,
        shell: ShellProbeState,
        scroll: ScrollProbeState,
        bounds: BTreeMap<String, [f32; 4]>,
        styles: ComputedStyleState,
    }

    fn origin() -> Instant {
        *PROCESS_ORIGIN.get_or_init(Instant::now)
    }

    fn state() -> &'static Mutex<ProbeInner> {
        PROBE_STATE.get_or_init(|| Mutex::new(ProbeInner::default()))
    }

    pub fn is_enabled() -> bool {
        PROBE_ENABLED.load(Ordering::Relaxed)
    }

    pub fn probe_port() -> Option<u16> {
        let port = PROBE_PORT.load(Ordering::Acquire);
        (port != 0).then_some(port)
    }

    pub fn forced_warp_enabled() -> bool {
        FORCED_WARP_OVERRIDE.load(Ordering::Relaxed)
            || *FORCED_WARP_ENV
                .get_or_init(|| std::env::var("CST_DESKTOP_FORCE_WARP").as_deref() == Ok("1"))
    }

    pub fn set_forced_warp_for_test(enabled: bool) {
        FORCED_WARP_OVERRIDE.store(enabled, Ordering::Release);
    }

    pub fn init_if_requested(kit_root: Option<&Path>) {
        let _ = origin();
        if std::env::var("CST_DESKTOP_UX_PROBE").as_deref() != Ok("1") {
            return;
        }
        start_probe_server(kit_root);
    }

    pub fn start_probe_server(kit_root: Option<&Path>) -> Option<u16> {
        let _ = origin();
        PROBE_ENABLED.store(true, Ordering::Release);
        if SERVER_STARTED.swap(true, Ordering::AcqRel) {
            return probe_port();
        }
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(_) => {
                SERVER_STARTED.store(false, Ordering::Release);
                return None;
            }
        };
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(_) => {
                SERVER_STARTED.store(false, Ordering::Release);
                return None;
            }
        };
        PROBE_PORT.store(port, Ordering::Release);

        let info_path = std::env::var("CST_DESKTOP_UX_PROBE_FILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| kit_root.map(|r| r.join(".state").join("ux-probe.json")));
        if let Some(path) = info_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let payload = format!("{{\"port\":{port},\"pid\":{}}}", std::process::id());
            let _ = std::fs::write(&path, payload);
        }

        let _ = std::thread::Builder::new()
            .name("cst-ux-probe".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let _ = handle_connection(stream);
                }
            });
        Some(port)
    }

    fn handle_connection(mut stream: std::net::TcpStream) -> std::io::Result<()> {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut req_line = String::new();
        reader.read_line(&mut req_line)?;
        for _ in 0..MAX_HEADER_LINES {
            let mut hdr = String::new();
            let n = reader.read_line(&mut hdr)?;
            if n == 0 || hdr == "\r\n" || hdr == "\n" {
                break;
            }
        }
        let parts: Vec<&str> = req_line.split_whitespace().collect();
        let method = parts.first().copied().unwrap_or("GET");
        let target = parts.get(1).copied().unwrap_or("/probe");

        let (status, body) = if method == "POST" && target.starts_with("/probe/reset") {
            reset_window_counters();
            ("200 OK", "{\"ok\":true}".to_owned())
        } else if method == "POST" && target.starts_with("/probe/mark") {
            let label: String = target
                .split_once("label=")
                .map(|(_, v)| v.split('&').next().unwrap_or(v))
                .filter(|s| !s.is_empty())
                .unwrap_or("external")
                .chars()
                .take(64)
                .collect();
            mark_input(&label);
            ("200 OK", "{\"ok\":true}".to_owned())
        } else if target.starts_with("/probe") {
            let snap = snapshot();
            let json = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_owned());
            ("200 OK", json)
        } else {
            ("404 Not Found", "{\"error\":\"not_found\"}".to_owned())
        };

        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(resp.as_bytes())?;
        stream.flush()
    }

    fn elapsed_ms() -> f64 {
        origin().elapsed().as_secs_f64() * 1000.0
    }

    pub fn mark_bootstrap_done() {
        if !is_enabled() {
            return;
        }
        if let Ok(mut g) = state().lock() {
            g.startup.bootstrap_done_ms.get_or_insert_with(elapsed_ms);
        }
    }

    pub fn mark_fonts_loaded() {
        if !is_enabled() {
            return;
        }
        if let Ok(mut g) = state().lock() {
            g.startup.fonts_loaded_ms.get_or_insert_with(elapsed_ms);
        }
    }

    pub fn mark_window_opened() {
        if !is_enabled() {
            return;
        }
        if let Ok(mut g) = state().lock() {
            g.startup.window_opened_ms.get_or_insert_with(elapsed_ms);
        }
    }

    pub fn mark_input(label: &str) {
        if !is_enabled() {
            return;
        }
        if let Ok(mut g) = state().lock() {
            g.pending_input = Some((label.to_owned(), Instant::now()));
        }
    }

    pub fn record_running_light_frame() {
        if is_enabled() {
            RUNNING_LIGHT_FRAMES.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_render(render_started: Instant, shell_state: ShellProbeState) {
        if !is_enabled() {
            return;
        }
        let now = Instant::now();
        let render_us = now.saturating_duration_since(render_started).as_micros() as u64;
        if let Ok(mut g) = state().lock() {
            if g.frame_render_us.len() >= MAX_FRAME_SAMPLES {
                g.frame_render_us.pop_front();
            }
            g.frame_render_us.push_back(render_us);
            g.shell = shell_state;
        }
    }

    pub fn record_frame_painted(frame_started: Instant) {
        if !is_enabled() {
            return;
        }
        let now = Instant::now();
        let paint_us = now.saturating_duration_since(frame_started).as_micros() as u64;
        if let Ok(mut g) = state().lock() {
            if g.startup.first_frame_ms.is_none() {
                g.startup.first_frame_ms = Some(elapsed_ms());
            }
            if let Some(prev) = g.last_frame_at {
                let interval = now.saturating_duration_since(prev).as_micros() as u64;
                if g.frame_interval_us.len() >= MAX_FRAME_SAMPLES {
                    g.frame_interval_us.pop_front();
                }
                g.frame_interval_us.push_back(interval);
            }
            g.last_frame_at = Some(now);
            g.total_frames += 1;
            g.window_frames += 1;
            if g.frame_paint_us.len() >= MAX_FRAME_SAMPLES {
                g.frame_paint_us.pop_front();
            }
            g.frame_paint_us.push_back(paint_us);
            if paint_us >= 16_667 {
                g.frames_over_16ms += 1;
            }
            if paint_us >= 33_333 {
                g.frames_over_33ms += 1;
            }
            if let Some((label, started)) = g.pending_input.take() {
                let latency_ms = now.saturating_duration_since(started).as_secs_f64() * 1000.0;
                if g.input_samples.len() >= MAX_INPUT_SAMPLES {
                    g.input_samples.pop_front();
                }
                g.input_samples.push_back(InputLatencySample {
                    label,
                    latency_ms,
                    at_ms: elapsed_ms(),
                });
            }
        }
    }

    /// Zero-visual-impact overlay canvas that records container `Bounds` during prepaint and
    /// optionally finalizes the full render+layout+prepaint+paint frame timer on paint.
    pub fn bounds_probe_layer(
        name: &'static str,
        frame_started: Option<Instant>,
    ) -> Option<gpui_kit::AnyElement> {
        if !is_enabled() {
            return None;
        }
        Some(
            gpui_kit::canvas(
                move |bounds, _, _| {
                    record_bounds(
                        name,
                        f32::from(bounds.origin.x),
                        f32::from(bounds.origin.y),
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                    );
                },
                move |_, (), _, _| {
                    if let Some(started) = frame_started {
                        record_frame_painted(started);
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .into_any_element(),
        )
    }

    pub fn record_scroll(
        offset_y: f32,
        max_offset_y: f32,
        auto_follow_bottom: bool,
        show_scroll_bottom_btn: bool,
    ) {
        if !is_enabled() {
            return;
        }
        if let Ok(mut g) = state().lock() {
            g.scroll = ScrollProbeState {
                offset_y,
                max_offset_y,
                auto_follow_bottom,
                show_scroll_bottom_btn,
            };
        }
    }

    pub fn record_bounds(name: &str, x: f32, y: f32, w: f32, h: f32) {
        if !is_enabled() {
            return;
        }
        if let Ok(mut g) = state().lock() {
            g.bounds.insert(name.to_owned(), [x, y, w, h]);
        }
    }

    pub fn record_composer_style(focused: bool, border_rgba: [f32; 4], shadow_count: usize) {
        if !is_enabled() {
            return;
        }
        if let Ok(mut g) = state().lock() {
            g.styles = ComputedStyleState {
                composer_focused: focused,
                composer_border_rgba: border_rgba,
                composer_shadow_count: shadow_count,
            };
        }
    }

    pub fn reset_window_counters() {
        let diag = surface_diagnostics();
        let rl = RUNNING_LIGHT_FRAMES.load(Ordering::Relaxed);
        if let Ok(mut g) = state().lock() {
            g.window_frames = 0;
            g.frame_render_us.clear();
            g.frame_paint_us.clear();
            g.frame_interval_us.clear();
            g.frames_over_16ms = 0;
            g.frames_over_33ms = 0;
            g.input_samples.clear();
            g.pending_input = None;
            g.baseline_sky_draws = diag.sky_draw_count;
            g.baseline_frosted_draws = diag.frosted_draw_count;
            g.baseline_running_light = rl;
        }
    }

    fn pct_u64(vals: &VecDeque<u64>, p: f64) -> u64 {
        if vals.is_empty() {
            return 0;
        }
        let mut sorted: Vec<u64> = vals.iter().copied().collect();
        sorted.sort_unstable();
        let idx = ((p / 100.0) * (sorted.len().saturating_sub(1) as f64)).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn pct_f64(vals: &[f64], p: f64) -> f64 {
        if vals.is_empty() {
            return 0.0;
        }
        let mut sorted = vals.to_vec();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let idx = ((p / 100.0) * (sorted.len().saturating_sub(1) as f64)).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn avg_u64(vals: &VecDeque<u64>) -> f64 {
        if vals.is_empty() {
            0.0
        } else {
            vals.iter().copied().sum::<u64>() as f64 / vals.len() as f64
        }
    }

    fn snapshot() -> ProbeSnapshot {
        let diag: SurfaceRuntimeDiagnostics = surface_diagnostics();
        let rl = RUNNING_LIGHT_FRAMES.load(Ordering::Relaxed);
        let g = state().lock().unwrap_or_else(|e| e.into_inner());
        let last_ago = g.last_frame_at.map(|t| t.elapsed().as_secs_f64() * 1000.0);
        let input_vals: Vec<f64> = g.input_samples.iter().map(|s| s.latency_ms).collect();
        let recent_inputs: Vec<InputLatencySample> =
            g.input_samples.iter().rev().take(20).cloned().collect();

        ProbeSnapshot {
            pid: std::process::id(),
            uptime_ms: elapsed_ms(),
            startup: g.startup.clone(),
            frame_pacing: FramePacingReport {
                total_frames: g.total_frames,
                window_frames: g.window_frames,
                last_frame_ms_ago: last_ago,
                render_us_avg: avg_u64(&g.frame_render_us),
                render_us_p50: pct_u64(&g.frame_render_us, 50.0),
                render_us_p95: pct_u64(&g.frame_render_us, 95.0),
                render_us_p99: pct_u64(&g.frame_render_us, 99.0),
                render_us_max: g.frame_render_us.iter().copied().max().unwrap_or(0),
                paint_us_avg: avg_u64(&g.frame_paint_us),
                paint_us_p50: pct_u64(&g.frame_paint_us, 50.0),
                paint_us_p95: pct_u64(&g.frame_paint_us, 95.0),
                paint_us_p99: pct_u64(&g.frame_paint_us, 99.0),
                paint_us_max: g.frame_paint_us.iter().copied().max().unwrap_or(0),
                interval_us_p50: pct_u64(&g.frame_interval_us, 50.0),
                interval_us_p95: pct_u64(&g.frame_interval_us, 95.0),
                frames_over_16ms: g.frames_over_16ms,
                frames_over_33ms: g.frames_over_33ms,
            },
            input_latency: InputLatencyReport {
                count: input_vals.len(),
                p50_ms: pct_f64(&input_vals, 50.0),
                p95_ms: pct_f64(&input_vals, 95.0),
                max_ms: input_vals.iter().copied().fold(0.0, f64::max),
                recent: recent_inputs,
            },
            surface: SurfaceDeltaReport {
                supports_sm50: diag.supports_sm50,
                is_software_adapter: diag.is_software_adapter,
                sky_draw_count: diag.sky_draw_count,
                frosted_draw_count: diag.frosted_draw_count,
                device_build_count: diag.device_build_count,
                sky_draw_delta: diag.sky_draw_count.saturating_sub(g.baseline_sky_draws),
                frosted_draw_delta: diag
                    .frosted_draw_count
                    .saturating_sub(g.baseline_frosted_draws),
                running_light_frames: rl,
                running_light_delta: rl.saturating_sub(g.baseline_running_light),
            },
            shell: g.shell.clone(),
            scroll: g.scroll.clone(),
            bounds: g.bounds.clone(),
            styles: g.styles.clone(),
        }
    }
}

#[cfg(feature = "test-support")]
pub use imp::{
    ShellProbeState, bounds_probe_layer, forced_warp_enabled, init_if_requested, is_enabled,
    mark_bootstrap_done, mark_fonts_loaded, mark_input, mark_window_opened, probe_port,
    record_bounds, record_composer_style, record_frame_painted, record_render,
    record_running_light_frame, record_scroll, reset_window_counters, set_forced_warp_for_test,
    start_probe_server,
};

#[cfg(not(feature = "test-support"))]
#[derive(Debug, Clone, Default)]
pub struct ShellProbeState {
    pub route: String,
    pub active_session_id: Option<String>,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub scale_factor: f32,
    pub sidebar_collapsed: bool,
    pub window_active: bool,
    pub reduce_motion: bool,
    pub transparency_enabled: bool,
    pub web_active: bool,
    pub forced_warp: bool,
}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn is_enabled() -> bool {
    false
}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn forced_warp_enabled() -> bool {
    false
}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn init_if_requested(_kit_root: Option<&std::path::Path>) {}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn mark_bootstrap_done() {}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn mark_fonts_loaded() {}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn mark_window_opened() {}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn mark_input(_label: &str) {}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn record_running_light_frame() {}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn record_render(_render_started: std::time::Instant, _shell_state: ShellProbeState) {}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn bounds_probe_layer(
    _name: &'static str,
    _frame_started: Option<std::time::Instant>,
) -> Option<gpui_kit::AnyElement> {
    None
}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn record_scroll(
    _offset_y: f32,
    _max_offset_y: f32,
    _auto_follow_bottom: bool,
    _show_scroll_bottom_btn: bool,
) {
}

#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub fn record_composer_style(_focused: bool, _border_rgba: [f32; 4], _shadow_count: usize) {}
