use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/deepseek.json");

pub fn deepseek_provider() -> Provider {
    build_standard_provider("deepseek", "DeepSeek", DATA, false, false)
}
