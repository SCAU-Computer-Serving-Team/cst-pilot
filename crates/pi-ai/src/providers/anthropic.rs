use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/anthropic.json");

pub fn anthropic_provider() -> Provider {
    build_standard_provider("anthropic", "Anthropic", DATA, true, true)
}
