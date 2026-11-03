use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA_OPENAI: &str = include_str!("data/openai.json");
const DATA_CODEX: &str = include_str!("data/openai-codex.json");

pub fn openai_provider() -> Provider {
    build_standard_provider("openai", "OpenAI", DATA_OPENAI, true, true)
}

pub fn openai_codex_provider() -> Provider {
    build_standard_provider("openai-codex", "OpenAI Codex", DATA_CODEX, true, true)
}
