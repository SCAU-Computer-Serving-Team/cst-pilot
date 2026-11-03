use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/huggingface.json");

pub fn huggingface_provider() -> Provider {
    build_standard_provider("huggingface", "Hugging Face", DATA, false, false)
}
