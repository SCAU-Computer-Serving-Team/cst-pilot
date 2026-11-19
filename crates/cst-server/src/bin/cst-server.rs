use std::path::PathBuf;
use std::process::ExitCode;

use cst_kit::auth::{AuthRuntime, Channel, oa::OaClient};
use cst_kit::telemetry::TelemetryOptions;
use cst_kit::update::service::UpdateOptions;
use cst_kit::{Kit, KitHost};
use cst_server::{RunningServer, ServerConfig};

#[derive(Debug, PartialEq, Eq)]
struct CliArgs {
    health_check: bool,
    standalone_web: bool,
    open_browser: bool,
    port: Option<u16>,
    kit_root: Option<PathBuf>,
    cwd: Option<PathBuf>,
}

fn parse_args() -> Result<CliArgs, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from<I>(args: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut health_check = false;
    let mut standalone_web = false;
    let mut open_browser = false;
    let mut port = None;
    let mut kit_root = None;
    let mut cwd = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--health-check" => health_check = true,
            "--standalone-web" => standalone_web = true,
            "--open-browser" => open_browser = true,
            "--port" => {
                let val = iter.next().ok_or("`--port` 缺少端口号参数")?;
                port = Some(val.parse::<u16>().map_err(|_| "`--port` 端口号格式无效")?);
            }
            "--kit-root" => {
                let val = iter.next().ok_or("`--kit-root` 缺少路径参数")?;
                kit_root = Some(PathBuf::from(val));
            }
            "--cwd" => {
                let val = iter.next().ok_or("`--cwd` 缺少路径参数")?;
                cwd = Some(PathBuf::from(val));
            }
            other => return Err(format!("未知的命令行参数：{other}")),
        }
    }

    if open_browser && !standalone_web {
        return Err("`--open-browser` 需配合 `--standalone-web` 使用".to_owned());
    }

    Ok(CliArgs {
        health_check,
        standalone_web,
        open_browser,
        port,
        kit_root,
        cwd,
    })
}

fn open_default_browser(url: &str) {
    #[cfg(windows)]
    {
        if let Ok(win_dir) = cst_os::windows_directory() {
            let explorer = win_dir.join("explorer.exe");
            let _ = std::process::Command::new(explorer).arg(url).spawn();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = url;
    }
}

fn resolve_default_kit_root() -> Result<PathBuf, String> {
    cst_os::resolve_kit_root()
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };

    let kit_root = match args
        .kit_root
        .map(Ok)
        .unwrap_or_else(resolve_default_kit_root)
    {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };

    if args.health_check {
        return match cst_kit::update::handoff::run_cli_health_check_with_version(
            &kit_root,
            option_env!("CST_BUILD_VERSION"),
        ) {
            Ok(version) => {
                println!("cst-pilot {version}");
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
        };
    }

    let kit = match Kit::new(&kit_root) {
        Ok(k) => k,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };

    let oa_client = match OaClient::official() {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };

    let auth = match AuthRuntime::create(kit.root(), Channel::Gui, oa_client).await {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };

    let host = match KitHost::new(
        kit,
        auth,
        TelemetryOptions::default(),
        UpdateOptions::Compiled,
    )
    .await
    {
        Ok(h) => h,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };

    let mut config = ServerConfig::new(&kit_root);
    config.mode_config.standalone_web = args.standalone_web;
    config.port = args.port;
    config.cwd = args.cwd.or_else(|| std::env::current_dir().ok());

    let server = match RunningServer::start(host, config).await {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };

    println!("cst-server listening on {}", server.base_url());
    if args.open_browser {
        open_default_browser(&format!("{}/", server.base_url()));
    }

    let stop_token = server.state.server_stop.clone();
    tokio::select! {
        _ = stop_token.cancelled() => {},
        _ = tokio::signal::ctrl_c() => {
            let _ = server.state.execute_shutdown().await;
        }
    }

    server.shutdown().await;
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::parse_args_from;

    #[test]
    fn parse_standalone_web_and_open_browser() {
        let parsed = parse_args_from(
            ["--standalone-web", "--open-browser", "--port", "52831"]
                .into_iter()
                .map(str::to_owned),
        )
        .expect("valid cli args");
        assert!(parsed.standalone_web);
        assert!(parsed.open_browser);
        assert_eq!(parsed.port, Some(52831));
    }

    #[test]
    fn open_browser_requires_standalone_web() {
        let err = parse_args_from(["--open-browser"].into_iter().map(str::to_owned))
            .expect_err("open-browser without standalone-web should fail");
        assert!(err.contains("--standalone-web"));
    }
}
