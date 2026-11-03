use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/openrouter.json");

pub fn openrouter_provider() -> Provider {
    build_standard_provider("openrouter", "OpenRouter", DATA, true, false)
}
