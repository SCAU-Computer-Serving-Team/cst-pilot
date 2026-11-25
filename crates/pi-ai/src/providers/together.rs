use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/together.json");

pub fn together_provider() -> Provider {
    build_standard_provider("together", "Together", DATA, false, false)
}
