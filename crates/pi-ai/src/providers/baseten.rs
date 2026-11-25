use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/baseten.json");

pub fn baseten_provider() -> Provider {
    build_standard_provider("baseten", "Baseten", DATA, false, false)
}
