use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA_ZEN: &str = include_str!("data/opencode.json");
const DATA_GO: &str = include_str!("data/opencode-go.json");

pub fn opencode_provider() -> Provider {
    build_standard_provider("opencode", "OpenCode Zen", DATA_ZEN, false, false)
}

pub fn opencode_go_provider() -> Provider {
    build_standard_provider("opencode-go", "OpenCode Go", DATA_GO, false, false)
}
