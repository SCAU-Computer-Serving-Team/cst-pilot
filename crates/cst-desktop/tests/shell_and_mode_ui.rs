#![cfg(windows)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cst_client::bootstrap::{
    BootstrapOptions, DesktopInstanceOutcome, VerifiedServerEndpoint,
    acquire_or_activate_desktop_instance,
};
use cst_client::http::AppHttpClient;
use cst_client::runtime::CstClient;
use cst_client::session_store::SessionViewSnapshot;
use cst_client::state::{ChannelModeDto, ClientSnapshot, DesktopSettingsDto};
use cst_desktop::background::{
    AnimationGateInputs, D3D_FEATURE_LEVEL_11_0_RAW, D3d11FrostedGlassSurface, D3d11SkySurface,
    FrostedGlassSpec, ShaderFrameScheduler, SkyPreset, d3d_sm50_supported,
    has_active_hardware_diagnostic, reset_d3d11_pipeline_cache, surface_diagnostics,
};
use cst_desktop::shell::register_shell_keys;
use cst_desktop::shell::window_bounds::{
    DisplayWorkArea, NormalBoundsTracker, compute_display_hint, resolve_startup_window_geometry,
};
use cst_desktop::theme::DesktopPalette;
use cst_desktop::{
    open_desktop_shell_window_with_handle, parse_cli_args, run_desktop_health_check,
};
use cst_os::{ProcessIdentity, RestartUi};
use gpui::{
    AppContext as _, Bounds, ContentMask, Point, ScaledPixels, Size, WindowsSurfaceDrawContext,
    WindowsSurfaceRenderer, px,
};
use gpui_kit::test::{TestAppContextExt, TestQueryExt, TestWindowExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VIEWPORT, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::core::Interface;

struct KitTempFixture {
    root: PathBuf,
}

impl KitTempFixture {
    fn create() -> Self {
        let workspace_fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
        let dir = std::env::temp_dir().join(format!("cst-desktop-step3-{}", Uuid::new_v4()));
        fs::create_dir_all(dir.join("app/prompts")).unwrap();
        fs::create_dir_all(dir.join("app/fonts")).unwrap();
        fs::create_dir_all(dir.join("data/config")).unwrap();
        fs::write(dir.join("VERSION"), "cst-pilot v0.1.0\n").unwrap();
        fs::write(
            dir.join("app/prompts/APPEND_SYSTEM.md"),
            "桌面端测试规则。\n",
        )
        .unwrap();
        for skill in [
            "disk",
            "ls",
            "sys",
            "startup",
            "eventlog",
            "driver",
            "runbook",
            "bitlocker",
        ] {
            let p = dir.join(format!("app/skills/{skill}/SKILL.md"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, format!("---\nname: {skill}\n---\n技能说明。\n")).unwrap();
        }
        for font_file in [
            "SourceHanSansCN-VF.otf",
            "LICENSE.txt",
            "JetBrainsMono-Regular.ttf",
            "JetBrainsMono-Medium.ttf",
            "JetBrainsMono-OFL.txt",
        ] {
            fs::copy(
                workspace_fonts.join(font_file),
                dir.join("app/fonts").join(font_file),
            )
            .unwrap();
        }
        Self {
            root: dir.canonicalize().unwrap(),
        }
    }
}

impl Drop for KitTempFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct OffscreenD3d11Harness {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    rt_tex: ID3D11Texture2D,
    rt_view: ID3D11RenderTargetView,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl OffscreenD3d11Harness {
    fn create(width: u32, height: u32) -> Self {
        unsafe {
            let levels = [D3D_FEATURE_LEVEL_11_0];
            let mut device = None;
            let mut context = None;
            let mut actual_level = D3D_FEATURE_LEVEL_11_0;
            let hw = D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut actual_level),
                Some(&mut context),
            );
            if hw.is_err() {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    windows::Win32::Foundation::HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    Some(&mut actual_level),
                    Some(&mut context),
                )
                .expect("WARP D3D11 device creation must succeed");
            }
            let device = device.unwrap();
            let context = context.unwrap();

            let rt_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut rt_tex = None;
            device
                .CreateTexture2D(&rt_desc, None, Some(&mut rt_tex))
                .unwrap();
            let rt_tex = rt_tex.unwrap();

            let mut rt_view = None;
            device
                .CreateRenderTargetView(&rt_tex, None, Some(&mut rt_view))
                .unwrap();
            let rt_view = rt_view.unwrap();

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .unwrap();
            let staging = staging.unwrap();

            let rtv_opt = Some(rt_view.clone());
            context.OMSetRenderTargets(Some(std::slice::from_ref(&rtv_opt)), None);
            let vp = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            context.RSSetViewports(Some(std::slice::from_ref(&vp)));

            Self {
                device,
                context,
                rt_tex,
                rt_view,
                staging,
                width,
                height,
            }
        }
    }

    fn clear_split_black_white(&self) {
        let mut bgra = vec![0u8; (self.width * self.height * 4) as usize];
        for y in 0..self.height {
            for x in 0..self.width {
                let idx = ((y * self.width + x) * 4) as usize;
                let v = if x < self.width / 2 { 0u8 } else { 255u8 };
                bgra[idx] = v;
                bgra[idx + 1] = v;
                bgra[idx + 2] = v;
                bgra[idx + 3] = 255;
            }
        }
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: self.width,
            bottom: self.height,
            back: 1,
        };
        unsafe {
            self.context.UpdateSubresource(
                &self.rt_tex,
                0,
                Some(&region),
                bgra.as_ptr().cast(),
                self.width * 4,
                0,
            );
        }
    }

    fn make_draw_ctx(
        &self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        feature_level: u32,
    ) -> WindowsSurfaceDrawContext {
        let full = Bounds {
            origin: Point {
                x: ScaledPixels(0.0),
                y: ScaledPixels(0.0),
            },
            size: Size {
                width: ScaledPixels(self.width as f32),
                height: ScaledPixels(self.height as f32),
            },
        };
        WindowsSurfaceDrawContext {
            device_raw: self.device.as_raw(),
            device_context_raw: self.context.as_raw(),
            render_target_raw: self.rt_tex.as_raw(),
            render_target_view_raw: self.rt_view.as_raw(),
            bounds: Bounds {
                origin: Point {
                    x: ScaledPixels(x),
                    y: ScaledPixels(y),
                },
                size: Size {
                    width: ScaledPixels(w),
                    height: ScaledPixels(h),
                },
            },
            content_mask: ContentMask { bounds: full },
            viewport_size: [self.width as f32, self.height as f32],
            feature_level,
            is_software_emulated: false,
        }
    }

    fn readback_bgra(&self) -> Vec<[u8; 4]> {
        unsafe {
            self.context.CopyResource(&self.staging, &self.rt_tex);
            let mut mapped = std::mem::zeroed();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .unwrap();
            let mut pixels = Vec::with_capacity((self.width * self.height) as usize);
            let base = mapped.pData.cast::<u8>();
            for y in 0..self.height {
                let row = std::slice::from_raw_parts(
                    base.add((y * mapped.RowPitch) as usize),
                    (self.width * 4) as usize,
                );
                for x in 0..self.width as usize {
                    pixels.push([row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]]);
                }
            }
            self.context.Unmap(&self.staging, 0);
            pixels
        }
    }
}

#[test]
fn d3d11_single_window_sky_frosted_glass_and_device_recovery() {
    reset_d3d11_pipeline_cache();
    let before = surface_diagnostics();

    let h1 = OffscreenD3d11Harness::create(256, 160);

    // 1. Paint all 4 sky presets and verify non-trivial vertical gradient & rounded SDF corner clipping
    for preset in [
        SkyPreset::HomeLight,
        SkyPreset::HomeDark,
        SkyPreset::LoginLight,
        SkyPreset::LoginDark,
    ] {
        h1.clear_split_black_white();
        let sky0 = D3d11SkySurface {
            preset,
            time_sec: 0.0,
            opacity: 1.0,
            corner_radii_phys: [12.0, 12.0, 12.0, 12.0],
        };
        sky0.paint(&h1.make_draw_ctx(0.0, 0.0, 256.0, 160.0, D3D_FEATURE_LEVEL_11_0_RAW))
            .unwrap();
        let px0 = h1.readback_bgra();
        // Top-left (0, 0) lies outside the 12px corner radius, so it stays untouched black (0, 0, 0)!
        assert_eq!(
            px0[0][0..3],
            [0, 0, 0],
            "rounded sky SDF must clip (0, 0) for {preset:?}"
        );
        let top = px0[16 * 256 + 128];
        let bot = px0[144 * 256 + 128];
        assert_ne!(
            top, bot,
            "sky preset {preset:?} must produce distinct vertical sky colors"
        );
    }

    // 2. Sharp black/white split + frosted glass overlay -> Gaussian blur across x=128 inside overlay, untouched outside!
    h1.clear_split_black_white();
    let palette = DesktopPalette::light();
    let mut spec = FrostedGlassSpec::popover_h2p2z(&palette);
    spec.tint_opacity = 0.20;
    spec.blur_px = 10.0;
    let frosted = D3d11FrostedGlassSurface {
        spec,
        scale_factor: 1.0,
    };
    frosted
        .paint(&h1.make_draw_ctx(64.0, 32.0, 128.0, 96.0, D3D_FEATURE_LEVEL_11_0_RAW))
        .unwrap();

    let blurred_px = h1.readback_bgra();
    // Outside overlay (x=20, y=80) remains exact black (0, 0, 0)
    assert_eq!(blurred_px[80 * 256 + 20][0..3], [0, 0, 0]);
    // Outside overlay right side (x=236, y=80) remains exact white (255, 255, 255)
    assert_eq!(blurred_px[80 * 256 + 236][0..3], [255, 255, 255]);
    // Inside overlay across the x=128 edge (x=122 vs x=128 vs x=134): smoothly blurred intermediate values!
    let left_inner = blurred_px[80 * 256 + 80][0];
    let near_edge_left = blurred_px[80 * 256 + 124][0];
    let near_edge_right = blurred_px[80 * 256 + 132][0];
    let right_inner = blurred_px[80 * 256 + 176][0];
    assert!(
        left_inner < near_edge_left
            && near_edge_left < near_edge_right
            && near_edge_right < right_inner,
        "frosted glass must Gaussian-blur the live render target edge: {left_inner} < {near_edge_left} < {near_edge_right} < {right_inner}"
    );

    let after_h1 = surface_diagnostics();
    assert!(after_h1.device_build_count > before.device_build_count);

    // 3. Second independent ID3D11Device simulates device-removed / adapter recreate -> automatic pipeline rebuild!
    let h2 = OffscreenD3d11Harness::create(192, 128);
    D3d11SkySurface {
        preset: SkyPreset::HomeLight,
        time_sec: 2.5,
        opacity: 1.0,
        corner_radii_phys: [8.0, 8.0, 8.0, 8.0],
    }
    .paint(&h2.make_draw_ctx(0.0, 0.0, 192.0, 128.0, D3D_FEATURE_LEVEL_11_0_RAW))
    .unwrap();
    let after_h2 = surface_diagnostics();
    assert!(
        after_h2.device_build_count > after_h1.device_build_count,
        "switching ID3D11Device must automatically rebuild cached shaders & textures"
    );

    // 4. Below Feature Level 11_0 (`0xa000`) -> graceful fallback without crash, then restore 11_0
    D3d11SkySurface {
        preset: SkyPreset::HomeLight,
        time_sec: 1.0,
        opacity: 1.0,
        corner_radii_phys: [0.0; 4],
    }
    .paint(&h2.make_draw_ctx(0.0, 0.0, 192.0, 128.0, 0xa000))
    .unwrap();
    assert!(!d3d_sm50_supported());

    D3d11SkySurface {
        preset: SkyPreset::HomeLight,
        time_sec: 1.0,
        opacity: 1.0,
        corner_radii_phys: [0.0; 4],
    }
    .paint(&h2.make_draw_ctx(0.0, 0.0, 192.0, 128.0, D3D_FEATURE_LEVEL_11_0_RAW))
    .unwrap();
    assert!(d3d_sm50_supported());
}

#[test]
fn shader_frame_scheduler_zero_fps_freeze_and_static_start_frame() {
    let mut sched = ShaderFrameScheduler::new();
    let t0 = Instant::now();
    let active_gate = AnimationGateInputs {
        route_has_sky_shader: true,
        web_active_locked: false,
        window_focused: true,
        window_minimized: false,
        hardware_diagnostic_active: false,
        reduce_motion: false,
        remote_session: false,
        software_adapter: false,
        sm50_supported: true,
    };

    let (u0, req0) = sched.step(active_gate, t0);
    assert_eq!(u0, 0.0);
    assert!(req0);

    let (u1, req1) = sched.step(active_gate, t0 + Duration::from_millis(50));
    assert!((u1 - 0.05).abs() < 1e-4);
    assert!(req1);

    // Freeze at 0 FPS when off-page, unfocused, minimized, WebActive, or sys(gpu/sensor) active
    for frozen in [
        AnimationGateInputs {
            route_has_sky_shader: false,
            ..active_gate
        },
        AnimationGateInputs {
            window_focused: false,
            ..active_gate
        },
        AnimationGateInputs {
            window_minimized: true,
            ..active_gate
        },
        AnimationGateInputs {
            web_active_locked: true,
            ..active_gate
        },
        AnimationGateInputs {
            hardware_diagnostic_active: true,
            ..active_gate
        },
    ] {
        let (uf, reqf) = sched.step(frozen, t0 + Duration::from_secs(5));
        assert!(!reqf, "frozen gate must not request next animation frame");
        assert!(
            (uf - u1).abs() < 1e-5,
            "frozen gate must hold exact accumulated time {u1}, got {uf}"
        );
    }

    // Static START_T (`0.0`) on reduce_motion, remote_session (RDP), or software adapter
    for static_gate in [
        AnimationGateInputs {
            reduce_motion: true,
            ..active_gate
        },
        AnimationGateInputs {
            remote_session: true,
            ..active_gate
        },
        AnimationGateInputs {
            software_adapter: true,
            ..active_gate
        },
    ] {
        let (us, reqs) = sched.step(static_gate, t0 + Duration::from_secs(10));
        assert_eq!(us, 0.0);
        assert!(!reqs);
    }

    // Verify `sys(gpu)` & `sys(sensor)` detection on `ClientSnapshot`
    let mut snap = ClientSnapshot::default();
    assert!(!has_active_hardware_diagnostic(&snap));
    let mut active_tools = BTreeMap::new();
    active_tools.insert(
        "call-1".to_owned(),
        json!({ "toolName": "sys", "args": { "scope": "sensor" } }),
    );
    Arc::make_mut(&mut snap.loaded_sessions).insert(
        "s1".to_owned(),
        Arc::new(SessionViewSnapshot {
            id: "s1".to_owned(),
            revision: 1,
            event_cursor: None,
            leaf_id: None,
            running: true,
            compacting: false,
            active_turn_started_ms: None,
            messages: Arc::new(Vec::new()),
            entries: Arc::new(Vec::new()),
            active_tools: Arc::new(active_tools),
            streaming_assistant: None,
            bitlocker: None,
            context_usage: None,
            queue: None,
            ui: None,
            estimated_bytes: 0,
        }),
    );
    assert!(has_active_hardware_diagnostic(&snap));
}

#[test]
fn window_geometry_multi_dpi_clamping_drag_intersection_and_normal_bounds() {
    let primary = DisplayWorkArea {
        display_id: None,
        hint: compute_display_hint("disp-primary"),
        work_x: 0.0,
        work_y: 0.0,
        work_width: 1366.0,
        work_height: 768.0,
    };
    let secondary = DisplayWorkArea {
        display_id: None,
        hint: compute_display_hint("disp-secondary-2k"),
        work_x: 1366.0,
        work_y: -200.0,
        work_width: 2560.0,
        work_height: 1400.0,
    };
    let displays = [primary.clone(), secondary.clone()];

    // 1. Oversized saved rect on 1366x768 clamps to 90% (1229x691) and centers when offscreen
    let small_settings = DesktopSettingsDto {
        display_hint: Some(primary.hint.clone()),
        window_width: Some(2400),
        window_height: Some(1600),
        window_x: Some(-2000),
        window_y: Some(-900),
        ..DesktopSettingsDto::default()
    };
    let res_small = resolve_startup_window_geometry(&small_settings, &displays);
    assert_eq!(res_small.width, 1229.0);
    assert_eq!(res_small.height, 691.0);
    assert_eq!(res_small.x, 69.0);
    assert_eq!(res_small.y, 39.0);

    // 2. Secondary display hint matches secondary work area & preserves valid drag bar position
    let sec_settings = DesktopSettingsDto {
        display_hint: Some(secondary.hint.clone()),
        window_width: Some(1400),
        window_height: Some(900),
        window_x: Some(1500),
        window_y: Some(-150),
        ..DesktopSettingsDto::default()
    };
    let res_sec = resolve_startup_window_geometry(&sec_settings, &displays);
    assert_eq!(
        res_sec.display_hint.as_deref(),
        Some(secondary.hint.as_str())
    );
    assert_eq!(res_sec.x, 1500.0);
    assert_eq!(res_sec.y, -150.0);

    // 3. NormalBoundsTracker preserves normal restore bounds across maximize & minimize
    let mut tracker = NormalBoundsTracker::from_resolved(&res_sec);
    let initial_bounds = Bounds {
        origin: Point {
            x: px(1500.0),
            y: px(-150.0),
        },
        size: Size {
            width: px(1400.0),
            height: px(900.0),
        },
    };
    assert!(
        !tracker.observe(initial_bounds, false, false, Some(secondary.hint.clone())),
        "initial window creation frame must not emit a settings PATCH"
    );

    // User moves normal window to (1600, -120)
    let moved_bounds = Bounds {
        origin: Point {
            x: px(1600.0),
            y: px(-120.0),
        },
        size: Size {
            width: px(1440.0),
            height: px(920.0),
        },
    };
    assert!(tracker.observe(moved_bounds, false, false, Some(secondary.hint.clone())));

    // User maximizes to 2560x1400 -> `window_maximized` flips to true, normal rect stays 1440x920 @ (1600, -120)!
    let max_bounds = Bounds {
        origin: Point {
            x: px(1366.0),
            y: px(-200.0),
        },
        size: Size {
            width: px(2560.0),
            height: px(1400.0),
        },
    };
    assert!(tracker.observe(max_bounds, true, false, Some(secondary.hint.clone())));
    let patch = tracker.take_pending_patch().expect("maximize patch");
    assert_eq!(patch.window_maximized, Some(true));
    assert_eq!(patch.window_width, Some(Some(1440)));
    assert_eq!(patch.window_height, Some(Some(920)));
    assert_eq!(patch.window_x, Some(Some(1600)));
    assert_eq!(patch.window_y, Some(Some(-120)));

    // Minimized 0x0 notification is ignored
    let min_bounds = Bounds {
        origin: Point {
            x: px(-32000.0),
            y: px(-32000.0),
        },
        size: Size {
            width: px(0.0),
            height: px(0.0),
        },
    };
    assert!(!tracker.observe(min_bounds, false, true, None));
}

#[test]
fn desktop_cli_health_check_lock_free_and_corrupt_asset_rejection() {
    let parsed = parse_cli_args([
        "--health-check".to_owned(),
        "--ui".to_owned(),
        "tui".to_owned(),
    ])
    .unwrap();
    assert!(parsed.health_check);
    assert_eq!(parsed.ui, RestartUi::Tui);

    let fixture = KitTempFixture::create();
    let ver = run_desktop_health_check(&fixture.root, Some("0.1.0")).unwrap();
    assert_eq!(ver, "v0.1.0");

    // Hold single-instance desktop lock while spawning real `cst-pilot.exe --health-check`!
    let boot_opts = BootstrapOptions {
        require_elevation: false,
        ..BootstrapOptions::default()
    };
    let guard = match acquire_or_activate_desktop_instance(&fixture.root, &boot_opts).unwrap() {
        DesktopInstanceOutcome::Acquired(g) => g,
        DesktopInstanceOutcome::ActivatedExisting { .. } => panic!("unexpected existing instance"),
    };

    let exe = Path::new(env!("CARGO_BIN_EXE_cst-pilot"));
    let output = Command::new(exe)
        .arg("--health-check")
        .arg("--kit-root")
        .arg(&fixture.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cst-pilot.exe --health-check failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "cst-pilot v0.1.0");
    drop(guard);

    // Corrupt SFNT font header -> must fail cleanly without modifying files
    let mono_path = fixture.root.join("app/fonts/JetBrainsMono-Regular.ttf");
    fs::write(&mono_path, b"BAD_FONT_HEADER_BYTES").unwrap();
    let bad_out = Command::new(exe)
        .arg("--health-check")
        .arg("--kit-root")
        .arg(&fixture.root)
        .output()
        .unwrap();
    assert!(!bad_out.status.success());
    assert_eq!(fs::read(&mono_path).unwrap(), b"BAD_FONT_HEADER_BYTES");
}

#[derive(Default)]
struct MockServerState {
    channel_mode: String,
    sidebar_collapsed: bool,
    reject_sse: bool,
    running_sessions: bool,
    web_closed_calls: usize,
    exit_calls: usize,
    patched_bodies: Vec<Value>,
}

struct MockDesktopBackend {
    rt: tokio::runtime::Runtime,
    port: u16,
    token: String,
    state: Arc<Mutex<MockServerState>>,
    stop: Arc<AtomicBool>,
}

impl MockDesktopBackend {
    fn start() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let state = Arc::new(Mutex::new(MockServerState {
            channel_mode: "app_active".to_owned(),
            sidebar_collapsed: false,
            reject_sse: false,
            running_sessions: true,
            web_closed_calls: 0,
            exit_calls: 0,
            patched_bodies: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let token = "test-app-bearer-token-step3".to_owned();

        let state_bg = Arc::clone(&state);
        let stop_bg = Arc::clone(&stop);
        let port = rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                while !stop_bg.load(Ordering::Relaxed) {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    let st = Arc::clone(&state_bg);
                    let st_stop = Arc::clone(&stop_bg);
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 16384];
                        let Ok(n) = stream.read(&mut buf).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let first_line = req.lines().next().unwrap_or("");
                        let mut parts = first_line.split_whitespace();
                        let method = parts.next().unwrap_or("GET");
                        let path = parts.next().unwrap_or("/");
                        let body_str = req.split("\r\n\r\n").nth(1).unwrap_or("");

                        if path.starts_with("/api/app/events") || path.starts_with("/api/events/stream") {
                            if st.lock().unwrap().reject_sse {
                                return;
                            }
                            let hdr = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n: connected\n\n";
                            let _ = stream.write_all(hdr.as_bytes()).await;
                            while !st_stop.load(Ordering::Relaxed) {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                if stream.write_all(b": ping\n\n").await.is_err() {
                                    break;
                                }
                            }
                            return;
                        }

                        let resp_json = {
                            let mut guard = st.lock().unwrap();
                            match (method, path) {
                                ("GET", "/api/health") => json!({
                                    "connected": true,
                                    "stopping": false,
                                    "version": "v0.1.0"
                                }),
                                ("GET", "/api/state") => json!({
                                    "version": "v0.1.0",
                                    "channelMode": guard.channel_mode,
                                    "multiplexStream": true
                                }),
                                ("GET", "/api/sessions") => json!({
                                    "sessions": [
                                        { "id": "sess-1", "title": "蓝屏 0x116 显卡驱动排查", "running": guard.running_sessions }
                                    ]
                                }),
                                ("GET", p) if p.starts_with("/api/models") => json!({
                                    "models": [],
                                    "catalog": [],
                                    "unavailableEnabled": []
                                }),
                                ("GET", "/api/auth") => json!({ "providers": [] }),
                                ("GET", p) if p.starts_with("/api/account") => json!({
                                    "signedIn": true,
                                    "profile": {
                                        "supported": true,
                                        "studentId": "20260001",
                                        "name": "测试队员"
                                    }
                                }),
                                ("GET", "/api/app/settings") => json!({
                                    "theme": "light",
                                    "defaultView": "gui",
                                    "sidebarCollapsed": guard.sidebar_collapsed,
                                    "windowMaximized": false
                                }),
                                ("PATCH", "/api/app/settings") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        if let Some(c) = v.get("sidebarCollapsed").and_then(Value::as_bool) {
                                            guard.sidebar_collapsed = c;
                                        }
                                        guard.patched_bodies.push(v);
                                    }
                                    json!({
                                        "theme": "light",
                                        "defaultView": "gui",
                                        "sidebarCollapsed": guard.sidebar_collapsed,
                                        "windowMaximized": false
                                    })
                                }
                                ("GET", "/api/ui-hints") => json!({}),
                                ("GET", "/api/app/update/status") => json!({
                                    "status": { "phase": "idle" }
                                }),
                                ("POST", "/api/app/web/open") => {
                                    guard.channel_mode = "web_active".to_owned();
                                    json!({ "url": "http://127.0.0.1:43110/?ticket=mock" })
                                }
                                ("POST", "/api/app/web/close") => {
                                    guard.channel_mode = "app_active".to_owned();
                                    guard.web_closed_calls += 1;
                                    json!({ "channelMode": "app_active" })
                                }
                                ("GET", "/api/lifecycle") => json!({
                                    "stopping": false,
                                    "version": "v0.1.0",
                                    "activeExports": 0,
                                    "sessions": [
                                        {
                                            "id": "sess-1",
                                            "title": "蓝屏排查",
                                            "running": guard.running_sessions,
                                            "pendingInboxCount": 1
                                        }
                                    ]
                                }),
                                ("POST", "/api/app/exit") => {
                                    guard.exit_calls += 1;
                                    json!({ "stopping": true })
                                }
                                _ => json!({}),
                            }
                        };

                        let body_bytes = serde_json::to_vec(&resp_json).unwrap();
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body_bytes.len()
                        );
                        let _ = stream.write_all(header.as_bytes()).await;
                        let _ = stream.write_all(&body_bytes).await;
                    });
                }
            });
            port
        });

        Self {
            rt,
            port,
            token,
            state,
            stop,
        }
    }
}

impl Drop for MockDesktopBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[gpui_kit::test]
async fn narrow_sidebar_manual_override_and_drawer_follow_viewport_transitions(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = KitTempFixture::create();
    let backend = MockDesktopBackend::start();
    backend.state.lock().unwrap().reject_sse = true;
    let self_id = ProcessIdentity::current().unwrap();
    let endpoint = VerifiedServerEndpoint {
        kit_root: fixture.root.clone(),
        pid: self_id.pid,
        process_created: self_id.created,
        port: backend.port,
        version: "v0.1.0".into(),
        http: AppHttpClient::new(backend.port, &backend.token).unwrap(),
        spawned_by_client: false,
    };
    let handle = CstClient::from_verified_endpoint(backend.rt.handle(), endpoint, None).unwrap();
    let (win, _) = cx.update(|cx| {
        gpui_kit::init(cx);
        register_shell_keys(cx);
        open_desktop_shell_window_with_handle(handle.clone(), None, cx).unwrap()
    });
    let resize = |cx: &gpui_kit::TestAppContext, w, h| {
        cx.simulate_window_resize(win, Size::new(px(w), px(h)));
    };

    resize(cx, 1119.0, 700.0);
    cx.update_window(win, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("topbar-mklbe").visible());
        window.click("topbar-recent-drawer-btn", cx);
        assert!(window.find("recent-sessions-drawer").visible());
    })
    .unwrap();
    // The first resize persists its normal bounds even without a startup geometry event.
    cx.wait_for(win, Duration::from_secs(2), |_, _| {
        backend
            .state
            .lock()
            .unwrap()
            .patched_bodies
            .iter()
            .any(|patch| patch["windowWidth"] == 1119 && patch["windowHeight"] == 700)
    })
    .await;
    assert!(
        backend
            .state
            .lock()
            .unwrap()
            .patched_bodies
            .iter()
            .all(|patch| { patch.get("sidebarCollapsed").is_none() })
    );

    resize(cx, 1120.0, 700.0);
    // HTTP revisions apply even with no SSE connection. A persisted collapse
    // must stay collapsed across the breakpoint, unlike an automatic collapse.
    backend.state.lock().unwrap().sidebar_collapsed = true;
    backend
        .rt
        .block_on(handle.query_desktop_settings())
        .unwrap();
    assert!(!handle.snapshot().connected);
    cx.wait_for(win, Duration::from_secs(2), |window, _| {
        window.try_find("sidebar-l081m1").is_none()
    })
    .await;
    resize(cx, 1119.0, 700.0);
    resize(cx, 1120.0, 700.0);
    cx.update_window(win, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("sidebar-l081m1").is_none());
    })
    .unwrap();
    backend.state.lock().unwrap().sidebar_collapsed = false;
    backend
        .rt
        .block_on(handle.query_desktop_settings())
        .unwrap();
    cx.wait_for(win, Duration::from_secs(2), |window, _| {
        window.try_find("sidebar-l081m1").is_some()
    })
    .await;

    resize(cx, 1119.0, 700.0);
    cx.update_window(win, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("recent-sessions-drawer").is_none());
        window.press("ctrl-b", cx);
    })
    .unwrap();
    cx.executor()
        .advance_clock(cst_desktop::theme::motion::NORMAL);
    cx.update_window(win, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("sidebar-l081m1").visible());
    })
    .unwrap();

    resize(cx, 1.0, 1.0);
    resize(cx, 1100.0, 700.0);
    cx.update_window(win, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("sidebar-l081m1").visible(),
            "manual expansion is retained within the narrow viewport"
        );
    })
    .unwrap();
    resize(cx, 1120.0, 700.0);
    resize(cx, 1119.0, 700.0);
    resize(cx, 1120.0, 1.0);
    cx.update_window(win, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("sidebar-l081m1").is_none());
    })
    .unwrap();
    resize(cx, 1120.0, 700.0);
    cx.update_window(win, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("sidebar-l081m1").visible());
    })
    .unwrap();
    cx.wait_for(win, Duration::from_secs(2), |_, _| {
        backend
            .state
            .lock()
            .unwrap()
            .patched_bodies
            .iter()
            .any(|patch| patch["sidebarCollapsed"] == false)
    })
    .await;
}

#[gpui_kit::test]
async fn headless_gpui_shell_sidebar_drawer_escape_web_lock_and_exit_confirm(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = KitTempFixture::create();
    let backend = MockDesktopBackend::start();
    let self_id = ProcessIdentity::current().unwrap();
    let endpoint = VerifiedServerEndpoint {
        kit_root: fixture.root.clone(),
        pid: self_id.pid,
        process_created: self_id.created,
        port: backend.port,
        version: "v0.1.0".into(),
        http: AppHttpClient::new(backend.port, &backend.token).unwrap(),
        spawned_by_client: false,
    };
    let handle = CstClient::from_verified_endpoint(backend.rt.handle(), endpoint, None).unwrap();
    backend
        .rt
        .block_on(handle.refresh_globals())
        .expect("initial globals refresh");

    let (win_handle, shell) = cx.update(|cx| {
        let font_report = cst_desktop::theme::load_bundled_fonts(&fixture.root, cx);
        assert!(font_report.loaded_cjk, "{font_report:?}");
        assert!(font_report.loaded_mono, "{font_report:?}");
        assert_eq!(font_report.ui_family(), "Source Han Sans CN VF");
        assert_eq!(font_report.mono_family(), "JetBrains Mono");
        assert!(font_report.warning.is_none(), "{font_report:?}");
        gpui_kit::init(cx);
        register_shell_keys(cx);
        open_desktop_shell_window_with_handle(handle.clone(), Some(font_report), cx).unwrap()
    });

    // 1. Initial expanded state: `sidebar-l081m1` + `topbar-gels8` + `workspace-home-view` visible
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("sidebar-l081m1").visible());
        assert!(window.find("topbar-gels8").visible());
        assert!(window.find("workspace-home-view").visible());
        assert!(window.try_find("topbar-mklbe").is_none());
    })
    .unwrap();

    // 2. Collapse sidebar via `ctrl-b` -> switches to `topbar-mklbe` and focuses expand button
    cx.update_window(win_handle, |_, window, cx| {
        window.press("ctrl-b", cx);
        assert!(window.try_find("sidebar-l081m1").is_none());
        assert!(window.find("topbar-mklbe").visible());
        assert_eq!(
            window.find("topbar-expand-sidebar-btn").focused(),
            Some(true)
        );
    })
    .unwrap();

    // 3. Reverse the drawer mid-flight, without remounting it at its closed position.
    cx.update_window(win_handle, |_, window, cx| {
        window.click("topbar-recent-drawer-btn", cx);
        assert!(window.find("recent-sessions-drawer").visible());
    })
    .unwrap();
    cx.executor().advance_clock(Duration::from_millis(75));
    let opening_x = cx
        .update_window(win_handle, |_, window, cx| {
            window.render_frame(cx);
            let x = window.find("recent-sessions-drawer").bounds().origin.x;
            window.press("escape", cx);
            assert!(window.try_find("recent-sessions-drawer").is_none());
            x
        })
        .unwrap();
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.update_window(win_handle, |_, window, cx| {
        window.click("topbar-recent-drawer-btn", cx);
        let reopening_x = window.find("recent-sessions-drawer").bounds().origin.x;
        assert!(reopening_x > gpui_kit::px(-116.0) && reopening_x < opening_x);

        // Escape still removes the interactive surface immediately and restores focus.
        window.press("escape", cx);
        assert!(window.try_find("recent-sessions-drawer").is_none());
        assert_eq!(
            window.find("topbar-recent-drawer-btn").focused(),
            Some(true)
        );
    })
    .unwrap();

    // Account menu exits without retaining its interactive surface or losing trigger focus.
    cx.update_window(win_handle, |_, window, cx| {
        window.click("topbar-account-btn", cx);
        assert!(window.try_find("account-popover-menu").is_some());
    })
    .unwrap();
    cx.executor()
        .advance_clock(cst_desktop::theme::motion::POPOVER_OPEN);
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
        assert!(window.try_find("account-popover-menu").is_none());
        assert_eq!(window.find("topbar-account-btn").focused(), Some(true));
        // Reopen during the exit so the lock has a live menu to cancel.
        window.click("topbar-account-btn", cx);
        assert!(window.try_find("account-popover-menu").is_some());
    })
    .unwrap();

    // 4. `WebActive` lock overlay (`WO6NQ`) & `"切回桌面端"` recovery
    {
        backend.state.lock().unwrap().channel_mode = "web_active".to_owned();
    }
    backend.rt.block_on(handle.refresh_globals()).unwrap();
    assert_eq!(handle.snapshot().channel_mode, ChannelModeDto::WebActive);

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("web-active-card-wo6nq").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        assert!(window.try_find("account-popover-menu").is_none());
        window.click("web-active-switch-back-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("web-active-card-wo6nq").is_none()
    })
    .await;
    assert_eq!(backend.state.lock().unwrap().web_closed_calls, 1);
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("account-popover-menu").is_none());
    })
    .unwrap();

    // 5. Close button (`window-btn-close`) with active session -> opens `pFooK` 384px exit dialog,
    //    initial focus on `"取消"`, `escape` dismisses safely without calling `/api/app/exit`!
    cx.update_window(win_handle, |_, window, cx| {
        window.click("window-btn-close", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("exit-dialog-card-pfook").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        let card = window.find("exit-dialog-card-pfook");
        assert_eq!(f32::from(card.bounds().size.width), 384.0);
        assert_eq!(window.find("exit-dialog-cancel-btn").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find("exit-dialog-confirm-btn").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find("exit-dialog-cancel-btn").focused(), Some(true));

        window.press("escape", cx);
        assert!(window.try_find("exit-dialog-card-pfook").is_none());
    })
    .unwrap();

    cx.update(|cx| {
        assert!(!shell.read(cx).has_exit_dialog());
    });
    assert_eq!(backend.state.lock().unwrap().exit_calls, 0);
}

#[test]
fn motion_primitives_timings_easings_banner_hover_pause_and_welcome_stagger_unit() {
    use cst_desktop::shell::overlays::BannerCountdownState;
    use cst_desktop::theme::motion;

    assert_eq!(motion::INSTANT, Duration::ZERO);
    assert_eq!(motion::PRESS, Duration::from_millis(120));
    assert_eq!(motion::FAST, Duration::from_millis(150));
    assert_eq!(motion::POPOVER_OPEN, Duration::from_millis(220));
    assert_eq!(motion::WELCOME_ITEM, Duration::from_millis(220));
    assert_eq!(motion::MODAL_OPEN, Duration::from_millis(240));
    assert_eq!(motion::NORMAL, Duration::from_millis(240));
    assert_eq!(motion::SURFACE_CLOSE, Duration::from_millis(150));
    assert_eq!(motion::FLIP_ENTER, Duration::from_millis(180));
    assert_eq!(motion::FLIP_MOVE, Duration::from_millis(250));
    assert_eq!(motion::RESIZE, Duration::from_millis(300));
    assert_eq!(motion::WELCOME_MAX_TOTAL, Duration::from_millis(300));
    assert_eq!(motion::WELCOME_STAGGER_STEP, Duration::from_millis(40));
    assert_eq!(motion::SLOW, Duration::from_millis(340));
    assert_eq!(motion::ICON_SWAP_RESTORE, Duration::from_millis(1600));
    assert_eq!(motion::RUNNING_LIGHT_CYCLE, Duration::from_millis(2000));
    assert_eq!(motion::BANNER_AUTO_DISMISS, Duration::from_millis(2600));
    assert_eq!(motion::TOOLTIP_INITIAL_DELAY, Duration::from_millis(400));
    assert_eq!(motion::TOOLTIP_COOLDOWN, Duration::from_millis(300));

    let eo = motion::ease_out();
    assert!((eo(0.0) - 0.0).abs() < 1e-4);
    assert!((eo(1.0) - 1.0).abs() < 1e-4);
    assert!(eo(0.2) > 0.45, "ease_out should accelerate early");

    let eio = motion::ease_in_out();
    assert!((eio(0.0) - 0.0).abs() < 1e-4);
    assert!((eio(0.5) - 0.5).abs() < 0.02);
    assert!((eio(1.0) - 1.0).abs() < 1e-4);

    let sp = motion::ease_spring();
    assert!((sp(0.0) - 0.0).abs() < 1e-4);
    assert!((sp(1.0) - 1.0).abs() < 1e-4);
    assert!(
        (1..20).any(|i| sp(i as f32 * 0.05) > 1.0),
        "ease_spring should produce subtle overshoot"
    );

    assert_eq!(
        motion::welcome_stagger_progress(2, Duration::ZERO, true),
        (1.0, 0.0)
    );
    let (op0, y0) = motion::welcome_stagger_progress(0, Duration::from_millis(110), false);
    let (op2, y2) = motion::welcome_stagger_progress(2, Duration::from_millis(110), false);
    assert!(op0 > op2 && y0 < y2, "item 2 must lag item 0 by 80ms");
    assert_eq!(
        motion::welcome_stagger_progress(2, Duration::from_millis(300), false),
        (1.0, 0.0)
    );

    let t0 = Instant::now();
    let mut countdown = BannerCountdownState::new();
    let time = |ms| t0 + Duration::from_millis(ms);
    let first = countdown.start(t0);
    assert_eq!(first.remaining_at(t0), Duration::from_millis(2600));
    assert!(!countdown.expire(first, time(1000)));
    assert!(countdown.set_hovered(true, time(1000)).is_none());
    assert!(!countdown.expire(first, time(2600)));
    assert_eq!(countdown.remaining(), Duration::from_millis(1600));
    let resumed = countdown.set_hovered(false, time(4000)).unwrap();
    assert_eq!(
        resumed.remaining_at(time(4000)),
        Duration::from_millis(1600)
    );
    assert!(!countdown.expire(first, time(4000)));
    assert!(countdown.set_hovered(true, time(4100)).is_none());
    let resumed_again = countdown.set_hovered(false, time(4500)).unwrap();
    assert_eq!(
        resumed_again.remaining_at(time(4500)),
        Duration::from_millis(1500)
    );
    let replacement = countdown.start(time(5000));
    assert!(!countdown.expire(resumed, time(5600)));
    assert!(!countdown.expire(resumed_again, time(6000)));
    assert!(countdown.expire(replacement, time(7600)));
    assert!(!countdown.expire(replacement, time(7601)));
    assert!(countdown.set_hovered(true, time(7601)).is_none());
    assert!(!countdown.is_hovered());

    // Pause/resume at the same clock tick can produce the same absolute
    // deadline; it still must not revalidate the cancelled timer.
    let before_pause = countdown.start(time(9000));
    countdown.set_hovered(true, time(9000));
    let after_pause = countdown.set_hovered(false, time(9000)).unwrap();
    assert!(!countdown.expire(before_pause, time(11600)));
    assert!(countdown.expire(after_pause, time(11600)));
}

#[gpui_kit::test]
async fn headless_gpui_motion_primitives_interruptible_reversal_reduce_motion_one_shot_and_banner_hover_pause(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::base::motion::{Transition, transition};
    use gpui_kit::{ParentElement as _, Styled as _, div};
    use std::cell::Cell;
    use std::rc::Rc;

    cx.executor().allow_parking();

    // 1. Interruptible GPUI Transition mid-flight reversal + reduce_motion 0ms snap
    struct ProbeView {
        target: Rc<Cell<f32>>,
        last_val: Rc<Cell<f32>>,
    }
    impl gpui_kit::Render for ProbeView {
        fn render(
            &mut self,
            window: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            let v = transition(
                "probe-reversal-id",
                self.target.get(),
                Transition::new(cst_desktop::theme::motion::NORMAL)
                    .ease(cst_desktop::theme::motion::ease_out()),
                window,
                cx,
            );
            self.last_val.set(v);
            div().size_full().child(format!("{v:.3}"))
        }
    }

    let target = Rc::new(Cell::new(0.0_f32));
    let sampled = Rc::new(Cell::new(0.0_f32));
    let probe_handle = cx.add_window(|_, _| ProbeView {
        target: Rc::clone(&target),
        last_val: Rc::clone(&sampled),
    });
    cx.update_window(*probe_handle, |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();
    assert!((sampled.get() - 0.0).abs() < 1e-4);

    // Retarget 0.0 -> 1.0 and advance half duration (120ms / 240ms)
    target.set(1.0);
    cx.update_window(*probe_handle, |_, window, cx| {
        window.refresh();
        window.render_frame(cx);
    })
    .unwrap();
    cx.executor().advance_clock(Duration::from_millis(120));
    cx.update_window(*probe_handle, |_, window, cx| {
        window.refresh();
        window.render_frame(cx);
    })
    .unwrap();
    let mid = sampled.get();
    assert!(
        mid > 0.25 && mid < 0.99,
        "expected mid-flight value in (0.25, 0.99), got {mid}"
    );

    // Reverse target back to 0.0 mid-flight — must reverse smoothly from `mid` without snapping to 0 or 1
    target.set(0.0);
    cx.update_window(*probe_handle, |_, window, cx| {
        window.refresh();
        window.render_frame(cx);
    })
    .unwrap();
    let at_reverse = sampled.get();
    assert!(
        at_reverse > 0.2 && (at_reverse - mid).abs() < 0.1,
        "reversal must start from current mid-flight value {mid}, got {at_reverse}"
    );
    cx.executor().advance_clock(Duration::from_millis(80));
    cx.update_window(*probe_handle, |_, window, cx| {
        window.refresh();
        window.render_frame(cx);
    })
    .unwrap();
    let reversing = sampled.get();
    assert!(
        reversing > 0.0 && reversing < at_reverse,
        "expected smooth downward progress between 0 and {at_reverse}, got {reversing}"
    );

    // Enabling reduce_motion must snap immediately (0ms) on target change
    cx.update(|cx| cx.set_reduce_motion(true));
    target.set(1.0);
    cx.update_window(*probe_handle, |_, window, cx| {
        window.refresh();
        window.render_frame(cx);
    })
    .unwrap();
    assert!(
        (sampled.get() - 1.0).abs() < 1e-4,
        "reduce_motion must snap transition to 1.0 at 0ms, got {}",
        sampled.get()
    );
    cx.update(|cx| cx.set_reduce_motion(false));

    // 2. Real AppShell: welcomeStagger oneShotPerSession, 0ms Ctrl+K, & 2600ms hover-pause banner
    let fixture = KitTempFixture::create();
    let backend = MockDesktopBackend::start();
    let self_id = ProcessIdentity::current().unwrap();
    let endpoint = VerifiedServerEndpoint {
        kit_root: fixture.root.clone(),
        pid: self_id.pid,
        process_created: self_id.created,
        port: backend.port,
        version: "v0.1.0".into(),
        http: AppHttpClient::new(backend.port, &backend.token).unwrap(),
        spawned_by_client: false,
    };
    let handle = CstClient::from_verified_endpoint(backend.rt.handle(), endpoint, None).unwrap();
    backend.rt.block_on(handle.refresh_globals()).unwrap();

    let (win_handle, shell) = cx.update(|cx| {
        let font_report = cst_desktop::theme::load_bundled_fonts(&fixture.root, cx);
        gpui_kit::init(cx);
        register_shell_keys(cx);
        open_desktop_shell_window_with_handle(handle.clone(), Some(font_report), cx).unwrap()
    });

    // Ctrl+K instant (0ms) open and Escape close
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("ctrl-k", cx);
        assert!(window.find("command-palette-modal-i70mz").visible());
        window.press("escape", cx);
        assert!(window.try_find("command-palette-modal-i70mz").is_none());
    })
    .unwrap();

    // Bottom-right gear button opens the desktop settings/account popover menu (matching Web),
    // and clicking "设置" navigates away from Home (marking welcomeStagger as done).
    cx.update_window(win_handle, |_, window, cx| {
        window.click("sidebar-settings-btn", cx);
        assert!(window.find("account-popover-menu").visible());
        assert!(window.find("account-menu-feedback-btn").visible());
        window.click("account-menu-settings-btn", cx);
        assert!(window.find("settings-view-vpwaf").visible());
        window.click("sidebar-new-session-btn", cx);
        assert!(window.find("workspace-home-view").visible());
    })
    .unwrap();
    cx.update(|cx| {
        assert!(shell.read(cx).workbench_welcome_stagger_done(cx));
    });

    // Banner hover pauses 2600ms auto-dismiss and unhover completes countdown
    shell.update(cx, |this, cx| {
        this.show_top_notice("已自动保存配置", cx);
    });
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("top-notice-banner").visible());
        window.hover("top-notice-banner", cx);
    })
    .unwrap();

    cx.executor().advance_clock(Duration::from_millis(3200));
    cx.executor().run_until_parked();
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("top-notice-banner").is_some(),
            "hovered banner must pause auto-dismiss"
        );
        window.hover("topbar-gels8", cx);
    })
    .unwrap();

    cx.executor().advance_clock(Duration::from_millis(2800));
    cx.executor().run_until_parked();
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("top-notice-banner").is_none(),
            "unhovered banner must auto-dismiss after remaining duration"
        );
    })
    .unwrap();

    // A notice unmounted under the pointer does not receive a hover-leave.
    // It must not get stuck paused when a higher-priority error replaces it.
    shell.update(cx, |this, cx| this.show_top_notice("后台操作已完成", cx));
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        window.hover("top-notice-banner", cx);
    })
    .unwrap();
    shell.update(cx, |this, cx| this.show_workspace_error("未能保存设置", cx));
    cx.update_window(win_handle, |_, window, cx| {
        window.hover("topbar-gels8", cx);
    })
    .unwrap();
    cx.executor().advance_clock(Duration::from_millis(2700));
    cx.executor().run_until_parked();
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("workspace-top-error-bar").is_none());
        assert!(window.try_find("top-notice-banner").is_none());
    })
    .unwrap();

    // CopyFeedbackTracker 1600ms iconSwap restore check
    struct CopyProbe {
        tracker: cst_desktop::workbench::markdown_view::CopyFeedbackTracker,
    }
    impl gpui_kit::Render for CopyProbe {
        fn render(
            &mut self,
            _window: &mut gpui_kit::Window,
            _cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            div().size_full()
        }
    }
    let copy_entity = cx.new(|_| CopyProbe {
        tracker: Default::default(),
    });
    copy_entity.update(cx, |p, cx| {
        p.tracker
            .mark_copied("block-1".into(), |v| &mut v.tracker, cx);
        assert!(p.tracker.is_copied("block-1"));
    });
    cx.executor().advance_clock(Duration::from_millis(1500));
    cx.executor().run_until_parked();
    cx.read(|cx| {
        assert!(
            copy_entity.read(cx).tracker.is_copied("block-1"),
            "copy icon must stay swapped before 1600ms"
        );
    });
    copy_entity.update(cx, |p, cx| {
        p.tracker
            .mark_copied("block-1".into(), |v| &mut v.tracker, cx);
    });
    cx.executor().run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.executor().run_until_parked();
    cx.read(|cx| assert!(copy_entity.read(cx).tracker.is_copied("block-1")));
    cx.executor().advance_clock(Duration::from_millis(1450));
    cx.executor().run_until_parked();
    cx.read(|cx| assert!(!copy_entity.read(cx).tracker.is_copied("block-1")));
}

#[gpui::test]
async fn milestone_g_motion_primitives_track_live_layout_and_cancel_stale_state(
    cx: &mut gpui::TestAppContext,
) {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use cst_desktop::theme::motion;
    use gpui_kit::{
        InteractiveElement as _, IntoElement as _, ParentElement as _, Styled as _,
        TestSupportExt as _, div, px,
    };

    struct MilestoneGProbe {
        checked: Rc<Cell<bool>>,
        collapsed_open: Rc<Cell<bool>>,
        hover_active: Rc<Cell<bool>>,
        flip_scope: Rc<Cell<&'static str>>,
        body_height: Rc<Cell<f32>>,
        body_focus: gpui::FocusHandle,
        flip_y_pos: Rc<Cell<f32>>,
        last_check_some: Rc<Cell<bool>>,
        last_collapse_some: Rc<Cell<bool>>,
        last_hover_p: Rc<Cell<f32>>,
        last_flip_offset: Rc<Cell<f32>>,
    }

    impl gpui_kit::Render for MilestoneGProbe {
        fn render(
            &mut self,
            window: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            let chk = motion::animated_checkbox_mark(
                "probe-chk",
                self.checked.get(),
                px(12.0),
                gpui_kit::white(),
                window,
                cx,
            );
            self.last_check_some.set(chk.is_some());

            let col = motion::animated_collapse(
                "probe-col",
                self.collapsed_open.get(),
                window,
                cx,
                |_, _| {
                    div()
                        .id("probe-body")
                        .test_support()
                        .track_focus(&self.body_focus)
                        .h(px(self.body_height.get()))
                        .flex_shrink_0()
                        .child("detail")
                        .into_any_element()
                },
            );
            self.last_collapse_some.set(col.is_some());

            let hp =
                motion::animated_state_progress("probe-hover", self.hover_active.get(), window, cx);
            self.last_hover_p.set(hp);

            self.last_flip_offset
                .set(f32::from(motion::flip_row_offset_y(
                    "probe-list",
                    "row-a",
                    &self.flip_scope.get().into(),
                    px(self.flip_y_pos.get()),
                    window,
                    cx,
                )));

            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().absolute().children(chk))
                .children(col)
                .child(
                    div()
                        .id("probe-tail")
                        .test_support()
                        .h(px(10.0))
                        .flex_shrink_0(),
                )
        }
    }

    let checked = Rc::new(Cell::new(false));
    let collapsed_open = Rc::new(Cell::new(false));
    let hover_active = Rc::new(Cell::new(false));
    let flip_scope = Rc::new(Cell::new("initial"));
    let body_height = Rc::new(Cell::new(80.0));
    let flip_y_pos = Rc::new(Cell::new(100.0_f32));
    let last_check_some = Rc::new(Cell::new(false));
    let last_collapse_some = Rc::new(Cell::new(false));
    let last_hover_p = Rc::new(Cell::new(0.0_f32));
    let last_flip_offset = Rc::new(Cell::new(0.0_f32));

    let win = cx.add_window(|_, cx| MilestoneGProbe {
        checked: Rc::clone(&checked),
        collapsed_open: Rc::clone(&collapsed_open),
        hover_active: Rc::clone(&hover_active),
        flip_scope: Rc::clone(&flip_scope),
        body_height: Rc::clone(&body_height),
        body_focus: cx.focus_handle(),
        flip_y_pos: Rc::clone(&flip_y_pos),
        last_check_some: Rc::clone(&last_check_some),
        last_collapse_some: Rc::clone(&last_collapse_some),
        last_hover_p: Rc::clone(&last_hover_p),
        last_flip_offset: Rc::clone(&last_flip_offset),
    });

    // 1. Initial closed / unchecked / idle state
    cx.update_window(*win, |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();
    assert!(!last_check_some.get());
    assert!(!last_collapse_some.get());
    assert!((last_hover_p.get() - 0.0).abs() < 1e-4);
    assert!((last_flip_offset.get() - 0.0).abs() < 1e-4);

    let sample = |cx: &mut gpui::TestAppContext| {
        cx.update_window(*win, |_, window, cx| {
            window.refresh();
            window.render_frame(cx);
        })
        .unwrap();
        last_flip_offset.get()
    };
    // Reordering down starts at the previous visual position, even on the first frame.
    flip_y_pos.set(160.0);
    assert_eq!(sample(cx), -60.0);
    cx.executor().advance_clock(Duration::from_millis(60));
    let mid = sample(cx);
    assert!(mid > -60.0 && mid < 0.0);
    // Reversing mid-flight preserves visual position. A filter/session switch never animates.
    flip_y_pos.set(100.0);
    assert!((sample(cx) - (60.0 + mid)).abs() < 0.001);
    flip_scope.set("filtered");
    flip_y_pos.set(20.0);
    assert_eq!(sample(cx), 0.0);
    flip_y_pos.set(501.0);
    assert_eq!(sample(cx), 0.0);
    flip_y_pos.set(21.0);
    assert_eq!(sample(cx), 480.0);
    cx.executor().advance_clock(Duration::from_millis(250));
    assert_eq!(sample(cx), 0.0);

    // 4. checkStroke + animated_collapse + 150ms hover progress
    checked.set(true);
    collapsed_open.set(true);
    hover_active.set(true);
    cx.update_window(*win, |_, window, cx| {
        window.refresh();
        window.render_frame(cx);
    })
    .unwrap();
    cx.executor().advance_clock(Duration::from_millis(75));
    cx.update_window(*win, |_, window, cx| {
        window.refresh();
        window.render_frame(cx);
    })
    .unwrap();
    assert!(
        last_check_some.get(),
        "checkStroke canvas should be visible while checking"
    );
    assert!(
        last_collapse_some.get(),
        "animated_collapse should be visible while opening"
    );
    let hover_mid = last_hover_p.get();
    assert!(
        hover_mid > 0.2 && hover_mid < 0.99,
        "150ms hover progress should be mid-flight at 75ms, got {hover_mid}"
    );

    // Growth while still opening must increase the reveal. Once open the body
    // follows its natural height, and the close starts at that last height.
    body_height.set(200.0);
    sample(cx);
    cx.executor().advance_clock(Duration::from_millis(175));
    sample(cx);
    let tail_y = |cx: &mut gpui::TestAppContext| {
        cx.update_window(*win, |_, window, _| {
            window
                .elements()
                .into_iter()
                .find(|el| el.path().last() == Some(&"probe-tail".into()))
                .unwrap()
                .bounds()
                .origin
                .y
        })
        .unwrap()
    };
    assert_eq!(tail_y(cx), px(200.0));
    body_height.set(280.0);
    sample(cx);
    assert_eq!(tail_y(cx), px(280.0));
    cx.update_window(*win, |probe, window, cx| {
        probe
            .downcast::<MilestoneGProbe>()
            .unwrap()
            .read(cx)
            .body_focus
            .clone()
            .focus(window, cx)
    })
    .unwrap();

    // Close collapse & uncheck: the height is animated but the subtree is already inert.
    checked.set(false);
    collapsed_open.set(false);
    sample(cx);
    assert_eq!(tail_y(cx), px(280.0));
    cx.update_window(*win, |probe, window, cx| {
        assert!(
            !probe
                .downcast::<MilestoneGProbe>()
                .unwrap()
                .read(cx)
                .body_focus
                .is_focused(window)
        );
        assert!(
            !window
                .elements()
                .iter()
                .any(|el| el.path().last() == Some(&"probe-body".into()))
        );
    })
    .unwrap();
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.update_window(*win, |_, window, cx| {
        window.refresh();
        window.render_frame(cx);
    })
    .unwrap();
    assert!(
        !last_check_some.get(),
        "checkStroke should unmount after 150ms uncheck"
    );
    assert!(
        !last_collapse_some.get(),
        "animated_collapse should unmount after 150ms close"
    );

    // Reduced motion adopts the new layout without a transitional frame.
    cx.update(|cx| cx.set_reduce_motion(true));
    collapsed_open.set(true);
    sample(cx);
    assert_eq!(tail_y(cx), px(280.0));
    collapsed_open.set(false);
    sample(cx);
    assert_eq!(tail_y(cx), px(0.0));

    let mid_col = motion::lerp_hsla(gpui_kit::transparent_black(), gpui_kit::white(), 0.5);
    assert!((mid_col.a - 0.5).abs() < 0.05);
}

#[gpui::test]
async fn desktop_tooltips_share_delay_but_not_window_or_anchor_lifetime(
    cx: &mut gpui::TestAppContext,
) {
    use cst_desktop::theme::motion::{self, DesktopTooltipExt as _, DesktopTooltips};
    use gpui_kit::{
        InputEvent as _, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
        MouseMoveEvent, ParentElement as _, Styled as _, TestSupportExt as _, div, point,
    };

    struct Probe {
        mounted: bool,
        layer: gpui::Entity<DesktopTooltips>,
    }
    impl gpui::Render for Probe {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let trigger = |id| {
                div()
                    .id(id)
                    .test_support()
                    .w(px(40.0))
                    .h(px(40.0))
                    .desktop_tooltip(id, id)
            };
            div()
                .size_full()
                .flex()
                .pl(px(12.0))
                .pt(px(12.0))
                .children(self.mounted.then(|| trigger("tip-a")))
                .child(trigger("tip-b"))
                .child(self.layer.clone())
        }
    }
    cx.update(|cx| {
        gpui_kit::base::TooltipDefaults::new()
            .with_show_delay(motion::TOOLTIP_INITIAL_DELAY)
            .with_grace_period(motion::TOOLTIP_COOLDOWN)
            .install(cx);
    });
    let open = |cx: &mut gpui::TestAppContext| {
        cx.add_window(|window, cx| Probe {
            mounted: true,
            layer: DesktopTooltips::install(window, cx),
        })
    };
    let first = open(cx);
    let second = open(cx);
    let has = |cx: &mut gpui::TestAppContext, win, id: &str| {
        cx.update_window(win, |_, window, cx| {
            window.refresh();
            window.render_frame(cx);
            window
                .elements()
                .iter()
                .any(|el| el.path().last() == Some(&gpui::SharedString::from(id.to_owned()).into()))
        })
        .unwrap()
    };
    let hover = |cx: &mut gpui::TestAppContext, win, id: &'static str| {
        cx.update_window(win, |_, window, cx| window.hover(id, cx))
            .unwrap();
    };
    let tick = |cx: &mut gpui::TestAppContext, ms| {
        cx.executor().run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(ms));
        cx.executor().run_until_parked();
    };

    cx.simulate_window_pointer_state(*first, true, true);
    hover(cx, *first, "tip-a");
    tick(cx, 399);
    assert!(!has(cx, *first, "tooltip-tip-a"));
    tick(cx, 1);
    assert!(has(cx, *first, "tooltip-tip-a"));
    assert!(!has(cx, *second, "tooltip-tip-a"));

    hover(cx, *first, "tip-b");
    assert!(has(cx, *first, "tooltip-tip-b"));
    assert!(!has(cx, *first, "tooltip-tip-a"));
    // A mouse-down outside any tooltip trigger cancels the visible grace period.
    cx.update_window(*first, |_, window, cx| {
        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Right,
                position: point(px(120.0), px(20.0)),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    assert!(!has(cx, *first, "tooltip-tip-b"));

    hover(cx, *first, "tip-a");
    tick(cx, 400);
    assert!(has(cx, *first, "tooltip-tip-a"));
    first
        .update(cx, |probe, _, cx| {
            probe.mounted = false;
            cx.notify();
        })
        .unwrap();
    // Move away and remove the hovered trigger before its normal leave delay expires.
    cx.update_window(*first, |_, window, cx| {
        window.dispatch_event(
            MouseMoveEvent {
                position: point(px(120.0), px(20.0)),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    has(cx, *first, "tooltip-tip-a");
    tick(cx, 0);
    assert!(!has(cx, *first, "tooltip-tip-a"));

    hover(cx, *first, "tip-b");
    tick(cx, 400);
    assert!(has(cx, *first, "tooltip-tip-b"));
    cx.simulate_window_pointer_state(*first, false, false);
    cx.simulate_window_pointer_state(*second, true, true);
    assert!(!has(cx, *first, "tooltip-tip-b"));
    hover(cx, *second, "tip-a");
    tick(cx, 399);
    assert!(!has(cx, *second, "tooltip-tip-a"));
    tick(cx, 1);
    assert!(has(cx, *second, "tooltip-tip-a"));
}

#[gpui::test]
async fn motion_tuning_collapse_exit_easings_and_frosted_opacity(cx: &mut gpui::TestAppContext) {
    use cst_desktop::background::FrostedGlassSpec;
    use cst_desktop::theme::{DesktopPalette, motion};
    use gpui_kit::{
        InteractiveElement as _, IntoElement as _, ParentElement as _, Styled as _,
        TestSupportExt as _, div,
    };
    use std::cell::Cell;
    use std::rc::Rc;

    // 1. FrostedGlassSpec::with_opacity scales & clamps cleanly for D3D11 fade sync.
    let pal = DesktopPalette::light();
    let scrim = FrostedGlassSpec::scrim(&pal);
    let half = scrim.with_opacity(0.5);
    assert!((half.opacity - scrim.opacity * 0.5).abs() < 1e-5);
    assert_eq!(scrim.with_opacity(-1.0).opacity, 0.0);
    assert!((scrim.with_opacity(2.0).opacity - scrim.opacity).abs() < 1e-5);

    // 2. checkStroke spring overshoot & flip ease_in_out symmetry.
    let spring = motion::ease_spring();
    let max_spring = (1..=40)
        .map(|i| spring(i as f32 / 40.0))
        .fold(0.0_f32, f32::max);
    assert!(
        max_spring > 1.001 && max_spring < 1.05,
        "checkStroke spring curve should have subtle overshoot in (1.001, 1.05), got {max_spring}"
    );
    let eio = motion::ease_in_out();
    assert!((eio(0.5) - 0.5).abs() < 0.02);
    assert!(eio(0.25) < 0.25 && eio(0.75) > 0.75);

    // 3. animated_collapse_with_exit preserves visual-only exit snapshot while blurring focus
    //    and unmounting interactive/test nodes on close frame 1, plus animated_press_progress.
    struct CollapseExitProbe {
        open: Rc<Cell<bool>>,
        pressed: Rc<Cell<bool>>,
        body_focus: gpui::FocusHandle,
        saw_inert_exit: Rc<Cell<bool>>,
        press_p: Rc<Cell<f32>>,
    }

    impl gpui::Render for CollapseExitProbe {
        fn render(
            &mut self,
            window: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            let saw_inert = Rc::clone(&self.saw_inert_exit);
            let focus = self.body_focus.clone();
            let col = motion::animated_collapse_with_exit(
                "exit-probe",
                self.open.get(),
                window,
                cx,
                |interactive, _, _| {
                    if interactive {
                        div()
                            .id("exit-probe-live-body")
                            .test_support()
                            .track_focus(&focus)
                            .h(px(120.0))
                            .flex_shrink_0()
                            .child("live")
                            .into_any_element()
                    } else {
                        saw_inert.set(true);
                        div()
                            .h(px(120.0))
                            .flex_shrink_0()
                            .child("inert-exit")
                            .into_any_element()
                    }
                },
            );
            self.press_p.set(motion::animated_press_progress(
                "press-probe-btn",
                self.pressed.get(),
                window,
                cx,
            ));
            div().size_full().flex().flex_col().children(col)
        }
    }

    let open = Rc::new(Cell::new(true));
    let pressed = Rc::new(Cell::new(false));
    let saw_inert_exit = Rc::new(Cell::new(false));
    let press_p = Rc::new(Cell::new(0.0_f32));
    let win = cx.add_window(|_, cx| CollapseExitProbe {
        open: Rc::clone(&open),
        pressed: Rc::clone(&pressed),
        body_focus: cx.focus_handle(),
        saw_inert_exit: Rc::clone(&saw_inert_exit),
        press_p: Rc::clone(&press_p),
    });

    let frame = |cx: &mut gpui::TestAppContext| {
        cx.update_window(*win, |_, window, cx| {
            window.refresh();
            window.render_frame(cx);
        })
        .unwrap();
    };
    frame(cx);
    cx.executor().advance_clock(Duration::from_millis(260));
    frame(cx);
    cx.update_window(*win, |probe, window, cx| {
        probe
            .downcast::<CollapseExitProbe>()
            .unwrap()
            .read(cx)
            .body_focus
            .clone()
            .focus(window, cx);
    })
    .unwrap();

    // Trigger close: frame 1 must blur focus, unmount interactive node, and render inert exit.
    open.set(false);
    pressed.set(true);
    frame(cx);
    cx.executor().advance_clock(Duration::from_millis(60));
    frame(cx);
    assert!(
        saw_inert_exit.get(),
        "animated_collapse_with_exit should render inert exit content during 150ms close"
    );
    assert!(
        press_p.get() > 0.2 && press_p.get() < 0.99,
        "120ms press progress should be mid-flight at 60ms, got {}",
        press_p.get()
    );
    cx.update_window(*win, |probe, window, cx| {
        assert!(
            !probe
                .downcast::<CollapseExitProbe>()
                .unwrap()
                .read(cx)
                .body_focus
                .is_focused(window)
        );
        assert!(
            !window
                .elements()
                .iter()
                .any(|el| el.path().last() == Some(&"exit-probe-live-body".into()))
        );
    })
    .unwrap();

    // After 150ms close completes, inert exit stops rendering.
    saw_inert_exit.set(false);
    cx.executor().advance_clock(Duration::from_millis(120));
    frame(cx);
    assert!(
        !saw_inert_exit.get(),
        "inert exit should unmount once 150ms close finishes"
    );
    assert!(
        (press_p.get() - 1.0).abs() < 0.01,
        "120ms press progress should settle at 1.0, got {}",
        press_p.get()
    );
}
