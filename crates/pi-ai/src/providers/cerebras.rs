use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/cerebras.json");

pub fn cerebras_provider() -> Provider {
    build_standard_provider("cerebras", "Cerebras", DATA, false, false)
}
