// Synthetic executable compiled only into a test-owned directory. It does not read host state.
use std::{fs, time::Duration};
fn main() {
    let root = std::env::current_exe().unwrap().parent().unwrap().to_owned();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--health-check") {
        let mode = fs::read_to_string(root.join(".state/health-mode")).unwrap_or_default();
        match mode.as_str() {
            "fail" => std::process::exit(7),
            "hang" => std::thread::sleep(Duration::from_secs(60)),
            _ => (),
        }
        fs::write(root.join(".state/health-checked"), b"checked").unwrap();
    } else if args.first().map(String::as_str) == Some("--ui") {
        fs::write(root.join(".state/restarted-ui"), args.get(1).unwrap()).unwrap();
    }
}
