#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::Write as _;
use std::path::PathBuf;

use cst_updater::{applier::Applier, handoff, manifest::ReleaseKey};

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        cst_os::attach_parent_console_if_stdio_missing();
    }
    if let Err(error) = run(&args) {
        cst_os::attach_parent_console_if_stdio_missing();
        let _ = writeln!(std::io::stderr(), "{error}");
        let _ = std::io::stderr().flush();
        std::process::exit(1);
    }
    let _ = std::io::stdout().flush();
}

fn run(args: &[std::ffi::OsString]) -> Result<(), String> {
    let usage = "用法：cst-updater --root <工具包目录> --request <交接标识>，或 --root <工具包目录> --recover";
    if args.len() == 1 && args[0] == "--help" {
        let _ = writeln!(std::io::stdout(), "{usage}");
        let _ = std::io::stdout().flush();
        return Ok(());
    }
    if args.len() < 3 || args[0] != "--root" {
        return Err(usage.into());
    }
    let root = PathBuf::from(&args[1]);
    // Never accept a public key or arbitrary executable through command-line input.
    let key = ReleaseKey::compiled()?;
    if args.len() == 3 && args[2] == "--recover" {
        Applier::new(&root, key)?.recover()?;
    } else if args.len() == 4 && args[2] == "--request" {
        let id = args[3].to_str().ok_or("升级交接标识无效。")?;
        handoff::run_request(&root, id, key)?;
    } else {
        return Err(usage.into());
    }
    Ok(())
}
