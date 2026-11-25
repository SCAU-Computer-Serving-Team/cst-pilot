use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/groq.json");

pub fn groq_provider() -> Provider {
    build_standard_provider("groq", "Groq", DATA, false, false)
}
