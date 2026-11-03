use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/google.json");

pub fn google_provider() -> Provider {
    build_standard_provider("google", "Google", DATA, false, false)
}
