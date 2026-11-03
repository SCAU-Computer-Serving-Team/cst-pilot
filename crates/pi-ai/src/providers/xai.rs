use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/xai.json");

pub fn xai_provider() -> Provider {
    build_standard_provider("xai", "xAI", DATA, true, true)
}
