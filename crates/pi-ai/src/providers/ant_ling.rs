use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/ant-ling.json");

pub fn ant_ling_provider() -> Provider {
    build_standard_provider("ant-ling", "Ant Ling", DATA, false, false)
}
