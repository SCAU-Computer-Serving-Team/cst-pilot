use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/typesafe.json");

pub fn typesafe_provider() -> Provider {
    build_standard_provider("typesafe", "TypeSafe", DATA, false, false)
}
