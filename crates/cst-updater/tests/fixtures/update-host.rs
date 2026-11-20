// Synthetic executable compiled only into a test-owned directory. It does not read host state.
use std::{fs, io::Write, time::Duration};
fn main() {
    let exe = std::env::current_exe().unwrap();
    let exe_name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_owned();
    let root = exe.parent().unwrap().to_owned();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--health-check") {
        let mode = fs::read_to_string(root.join(".state/health-mode")).unwrap_or_default();
        match mode.trim() {
            "fail" => std::process::exit(7),
            "hang" => std::thread::sleep(Duration::from_secs(60)),
            "mismatch" => {
                println!("cst-pilot v9.9.9");
                return;
            }
            "slow_then_hang" => {
                if exe_name.eq_ignore_ascii_case("cst-server.exe") {
                    std::thread::sleep(Duration::from_secs(6));
                } else {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
            _ => (),
        }
        let version_text =
            fs::read_to_string(root.join("VERSION")).unwrap_or_else(|_| "cst-pilot v0.0.0\n".into());
        print!("{version_text}");
        let _ = std::io::stdout().flush();
        fs::create_dir_all(root.join(".state")).unwrap();
        fs::write(root.join(".state/health-checked"), b"checked").unwrap();
        let mut order = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join(".state/health-order"))
            .unwrap();
        writeln!(order, "{exe_name}").unwrap();
    } else if args.first().map(String::as_str) == Some("--ui") {
        fs::create_dir_all(root.join(".state")).unwrap();
        fs::write(root.join(".state/restarted-ui"), args.get(1).unwrap()).unwrap();
    }
}
