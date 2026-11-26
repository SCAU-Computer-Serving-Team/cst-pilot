mod accessibility;
pub mod auth;
pub mod background;
pub mod palette;
pub mod sessions;
pub mod settings;
pub mod shell;
pub mod theme;
pub mod tools;
#[cfg(feature = "test-support")]
pub mod ux_probe;
pub mod workbench;

pub mod shaders {
    include!(concat!(env!("OUT_DIR"), "/compiled_shaders.rs"));
}

use std::path::{Path, PathBuf};

use cst_client::{BootstrapOptions, ClientBootstrapResult, ClientHandle, CstClient};
use cst_os::{REQUIRED_SKILL_NAMES, RestartUi, Storage, parse_local_json};
use gpui_kit::{App, AppContext as _, Entity, Global};

use crate::shell::window_bounds::{collect_app_displays, resolve_startup_window_geometry};
use crate::shell::{AppShell, register_shell_keys};
use crate::theme::{FontLoadReport, load_bundled_fonts, motion};

const REQUIRED_FONT_NAMES: &[&str] = &[
    "SourceHanSansCN-VF.otf",
    "LICENSE.txt",
    "JetBrainsMono-Regular.ttf",
    "JetBrainsMono-Medium.ttf",
    "JetBrainsMono-OFL.txt",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopCliArgs {
    pub health_check: bool,
    pub ui: RestartUi,
    pub kit_root: Option<PathBuf>,
}

pub fn parse_cli_args<I>(args: I) -> Result<DesktopCliArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut health_check = false;
    let mut ui = RestartUi::Desktop;
    let mut kit_root = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--health-check" => health_check = true,
            "--ui" => {
                let raw = iter
                    .next()
                    .ok_or("`--ui` 缺少界面模式参数（desktop / tui / web）。")?;
                ui = match raw.as_str() {
                    "desktop" => RestartUi::Desktop,
                    "tui" => RestartUi::Tui,
                    "web" => RestartUi::Web,
                    other => return Err(format!("不支持的 `--ui` 模式：{other}")),
                };
            }
            "--kit-root" => {
                let raw = iter.next().ok_or("`--kit-root` 缺少路径参数。")?;
                kit_root = Some(PathBuf::from(raw));
            }
            other => return Err(format!("未知的命令行参数：{other}")),
        }
    }

    Ok(DesktopCliArgs {
        health_check,
        ui,
        kit_root,
    })
}

fn normalize_version_tag(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed.strip_prefix('v').unwrap_or(trimmed)
}

fn read_kit_version_file(
    root: &Path,
    compiled_build_version: Option<&str>,
) -> Result<String, String> {
    let storage = Storage::new(root)?;
    let Some(bytes) = storage.read_limited(&storage.root.join("VERSION"), 4096)? else {
        if compiled_build_version.is_none() && storage.root.join("Cargo.toml").is_file() {
            return Ok(env!("CARGO_PKG_VERSION").to_owned());
        }
        return Err("缺少工具包 VERSION 文件。".to_owned());
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| "VERSION 文件编码无效。".to_owned())?;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or_else(|| "VERSION 文件内容为空。".to_owned())?;
    let ver = line
        .strip_prefix("cst-pilot ")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "VERSION 文件格式无效。".to_owned())?;
    Ok(ver.to_owned())
}

fn resolve_sibling_kit_binaries(kit_root: &Path) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let layout = cst_os::KitLayout::new(kit_root)?;
    let canonical_root = layout.display_root();
    let target_dir = canonical_root.join("target");

    let dev_target_bin_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.canonicalize().ok())
        .map(|exe| cst_os::strip_windows_verbatim_prefix(&exe))
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .filter(|dir| {
            dir.parent()
                .is_some_and(|parent| parent == target_dir.as_path())
        });

    if let Some(bin_dir) = dev_target_bin_dir {
        Ok((
            bin_dir.join("cst-pilot.exe"),
            bin_dir.join("cst-server.exe"),
            bin_dir.join("cst-updater.exe"),
        ))
    } else {
        Ok((
            canonical_root.join("cst-pilot.exe"),
            canonical_root.join("app").join("cst-server.exe"),
            canonical_root.join("app").join("cst-updater.exe"),
        ))
    }
}

/// Fast local read-only `--health-check` executed BEFORE initializing GUI, DirectWrite, single-instance locks, or network.
pub fn run_desktop_health_check(
    kit_root: &Path,
    compiled_build_version: Option<&str>,
) -> Result<String, String> {
    // 1. Embedded DXBC shader container integrity
    for (name, blob) in [
        ("VS_SURFACE_DXBC", shaders::VS_SURFACE_DXBC),
        ("PS_BLUE_HOUR_DXBC", shaders::PS_BLUE_HOUR_DXBC),
        ("PS_BLUE_SKY_DXBC", shaders::PS_BLUE_SKY_DXBC),
        ("PS_NIGHT_SKY_DXBC", shaders::PS_NIGHT_SKY_DXBC),
        ("PS_FROSTED_GLASS_DXBC", shaders::PS_FROSTED_GLASS_DXBC),
    ] {
        if blob.len() < 64 || &blob[0..4] != b"DXBC" {
            return Err(format!("内置着色器字节码损坏：{name}"));
        }
    }

    // 2. Kit VERSION & optional binary build version match
    let layout = cst_os::KitLayout::new(kit_root)?;
    let disk_version = read_kit_version_file(layout.root(), compiled_build_version)?;
    if let Some(build_ver) = compiled_build_version
        .map(str::trim)
        .filter(|v| !v.is_empty())
        && normalize_version_tag(&disk_version) != normalize_version_tag(build_ver)
    {
        return Err("健康检查发现桌面程序内置版本与工具包 VERSION 不一致。".into());
    }

    // 3. Kit prompt & skill assets under `app/`
    let storage = Storage::new(layout.root())?;
    let _app = storage.pin_directory(Path::new("app"), false)?;
    if storage
        .read_limited(
            &storage.root.join("app/prompts/APPEND_SYSTEM.md"),
            4 * 1024 * 1024,
        )?
        .is_none()
    {
        return Err("健康检查缺少 app/prompts/APPEND_SYSTEM.md。".into());
    }
    for skill in REQUIRED_SKILL_NAMES {
        let rel = format!("app/skills/{skill}/SKILL.md");
        if storage
            .read_limited(&storage.root.join(rel), 4 * 1024 * 1024)?
            .is_none()
        {
            return Err(format!("健康检查缺少内置技能文件：{skill}"));
        }
    }

    // 4. Required bundled desktop & web fonts + SFNT table directory validation
    let fonts_rel = layout.fonts_relative_dir();
    let _fonts = storage.pin_directory(fonts_rel, false)?;
    for name in REQUIRED_FONT_NAMES {
        if storage
            .read_limited(&storage.root.join(fonts_rel).join(name), 32 * 1024 * 1024)?
            .is_none()
        {
            return Err("健康检查缺少必需随包字体或许可文件。".into());
        }
    }
    for (name, magic) in [
        ("SourceHanSansCN-VF.otf", b"OTTO"),
        ("JetBrainsMono-Regular.ttf", &[0, 1, 0, 0]),
        ("JetBrainsMono-Medium.ttf", &[0, 1, 0, 0]),
    ] {
        let bytes = storage
            .read_limited(&storage.root.join(fonts_rel).join(name), 32 * 1024 * 1024)?
            .ok_or_else(|| "健康检查缺少随包字体。".to_owned())?;
        if !theme::has_valid_sfnt_magic(&bytes, magic) {
            return Err(format!("健康检查发现随包字体头损坏：{name}"));
        }
    }

    // 5. Local user JSON configuration non-destructive parse check
    if layout.config_dir().exists() {
        let (data_storage, cfg_rel) = if let Ok(rel) = layout.data_dir().strip_prefix(layout.root())
        {
            (Storage::new(layout.root())?, rel.join("config"))
        } else {
            (Storage::new(layout.data_dir())?, PathBuf::from("config"))
        };
        let _cfg = data_storage.pin_directory(&cfg_rel, false)?;
        for name in [
            "auth.json",
            "models.json",
            "settings.json",
            "device.json",
            "desktop.json",
            "web.json",
        ] {
            if let Some(bytes) = data_storage.read_limited(
                &data_storage.root.join(&cfg_rel).join(name),
                8 * 1024 * 1024,
            )? && !parse_local_json(&format!("data/config/{name}"), &bytes)?.is_object()
            {
                return Err("健康检查发现配置格式无效，原文件已保留。".into());
            }
        }
    }

    Ok(disk_version)
}

/// Forwards `--ui web` to `<app>/cst-server.exe --standalone-web --open-browser` under a sanitized environment.
pub fn forward_standalone_web(kit_root: &Path) -> Result<(), String> {
    let canonical_root = cst_os::strip_windows_verbatim_prefix(
        &kit_root
            .canonicalize()
            .map_err(|e| format!("无法解析工具包根目录：{e}"))?,
    );
    let (_desktop_exe, server_exe, _updater_exe) = resolve_sibling_kit_binaries(&canonical_root)?;
    if !server_exe.is_file() {
        return Err("未找到配套的 cst-server.exe。".into());
    }
    let meta = std::fs::symlink_metadata(&server_exe)
        .map_err(|e| format!("无法校验 cst-server.exe：{e}"))?;
    if cst_os::is_reparse_or_symlink(&meta) {
        return Err("cst-server.exe 不能是符号链接或重解析点。".into());
    }
    let canonical_exe = cst_os::strip_windows_verbatim_prefix(
        &server_exe
            .canonicalize()
            .map_err(|e| format!("无法校验 cst-server.exe 路径：{e}"))?,
    );

    let tmp_dir = canonical_root.join(".state/tmp");
    let _ = std::fs::create_dir_all(&tmp_dir);

    let mut cmd = std::process::Command::new(&canonical_exe);
    cmd.args(["--standalone-web", "--open-browser", "--kit-root"])
        .arg(&canonical_root)
        .current_dir(&canonical_root)
        .env_clear()
        .env("CST_PILOT_ROOT", &canonical_root)
        .env("CST_OFFLINE", "1")
        .env("TEMP", &tmp_dir)
        .env("TMP", &tmp_dir);

    #[cfg(windows)]
    {
        if let Ok(win_dir) = cst_os::windows_directory() {
            cmd.env("SystemRoot", &win_dir).env("WINDIR", &win_dir);
        }
        if let Ok(sys_dir) = cst_os::system_directory() {
            cmd.env("PATH", sys_dir);
        }
    }

    let status = cmd
        .status()
        .map_err(|e| format!("无法启动独立网页服务：{e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("独立网页服务退出异常：{status}"))
    }
}

struct OwnedClientGlobal(#[allow(dead_code)] CstClient);
impl Global for OwnedClientGlobal {}

/// Mounts `AppShell` onto a headless or live GPUI `App` from an existing [`ClientHandle`],
/// returning both the window handle and the shell entity.
pub fn open_desktop_shell_window_with_handle(
    handle: ClientHandle,
    font_report: Option<FontLoadReport>,
    cx: &mut App,
) -> anyhow::Result<(gpui_kit::AnyWindowHandle, Entity<AppShell>)> {
    gpui_kit::base::TooltipDefaults::new()
        .with_show_delay(motion::TOOLTIP_INITIAL_DELAY)
        .with_grace_period(motion::TOOLTIP_COOLDOWN)
        .install(cx);
    let settings = handle.snapshot().desktop_settings.clone();
    let displays = collect_app_displays(cx);
    let resolved = resolve_startup_window_geometry(&settings, &displays);
    let win_options = resolved.to_window_options();

    gpui_kit::open_window(win_options, cx, |window, cx| {
        cx.new(|cx| AppShell::new(handle, &resolved, font_report.as_ref(), window, cx))
    })
}

/// Mounts `AppShell` onto a headless or live GPUI `App` from an existing [`ClientHandle`].
pub fn open_desktop_shell_window(
    handle: ClientHandle,
    font_report: Option<FontLoadReport>,
    cx: &mut App,
) -> anyhow::Result<Entity<AppShell>> {
    let (_win_handle, view) = open_desktop_shell_window_with_handle(handle, font_report, cx)?;
    Ok(view)
}

/// Read the Windows accessibility preference without changing system settings.
pub(crate) fn sync_system_reduce_motion(cx: &mut App) {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
        };

        let mut enabled = 0_i32;
        // SPI_GETCLIENTAREAANIMATION writes one BOOL to pvParam.
        if unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some((&raw mut enabled).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        }
        .is_ok()
        {
            cx.set_reduce_motion(enabled == 0);
        }
    }
    #[cfg(not(windows))]
    let _ = cx;
}

/// Runs the full desktop application lifecycle (`CstClient::bootstrap` + `gpui_kit::application`).
pub fn run_desktop_app(kit_root: PathBuf, initial_ui: RestartUi) -> Result<(), String> {
    #[cfg(feature = "test-support")]
    ux_probe::init_if_requested(Some(&kit_root));
    let (desktop_exe, server_exe, updater_exe) = resolve_sibling_kit_binaries(&kit_root)?;
    let mut boot_opts = BootstrapOptions {
        allow_uac_server_spawn: cfg!(windows),
        expected_desktop_exe: Some(desktop_exe),
        expected_server_exe: Some(server_exe),
        expected_updater_exe: Some(updater_exe),
        ..BootstrapOptions::default()
    };
    if std::env::var("CST_ALLOW_UNELEVATED").as_deref() == Ok("1") {
        boot_opts.require_elevation = false;
    }

    let client = match CstClient::bootstrap(&kit_root, boot_opts).map_err(|e| e.to_string())? {
        ClientBootstrapResult::ActivatedExisting { .. } => return Ok(()),
        ClientBootstrapResult::Started(c) => c,
    };
    #[cfg(feature = "test-support")]
    ux_probe::mark_bootstrap_done();

    let handle = client.handle().clone();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            let font_report = load_bundled_fonts(&kit_root, cx);
            #[cfg(feature = "test-support")]
            ux_probe::mark_fonts_loaded();
            gpui_kit::init(cx);
            register_shell_keys(cx);
            cx.set_global(OwnedClientGlobal(client));
            sync_system_reduce_motion(cx);

            let shell = open_desktop_shell_window(handle, Some(font_report), cx)
                .expect("failed to open CST Pilot desktop window");
            #[cfg(feature = "test-support")]
            ux_probe::mark_window_opened();
            if initial_ui == RestartUi::Tui {
                shell.update(cx, |this, cx| {
                    this.show_top_notice("内置终端视图将在阶段 10 开放，已进入桌面图形界面。", cx);
                });
            }
        });

    Ok(())
}
