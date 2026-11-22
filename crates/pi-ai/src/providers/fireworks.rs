use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/fireworks.json");

pub fn fireworks_provider() -> Provider {
    build_standard_provider("fireworks", "Fireworks", DATA, false, false)
}
