use cst_pilot::update::{applier::Applier, handoff, manifest::ReleaseKey};
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let usage = "用法：cst-updater --root <工具包目录> --request <交接标识>，或 --root <工具包目录> --recover";
    if args.len() == 1 && args[0] == "--help" {
        println!("{usage}");
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
