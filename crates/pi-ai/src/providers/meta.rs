use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/meta.json");

pub fn meta_provider() -> Provider {
    build_standard_provider("meta", "Meta", DATA, false, false)
}
