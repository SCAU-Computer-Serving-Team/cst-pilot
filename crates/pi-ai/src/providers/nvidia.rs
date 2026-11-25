use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/nvidia.json");

pub fn nvidia_provider() -> Provider {
    build_standard_provider("nvidia", "NVIDIA", DATA, false, false)
}
