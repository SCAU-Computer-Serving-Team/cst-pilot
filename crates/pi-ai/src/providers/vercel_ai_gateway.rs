use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA: &str = include_str!("data/vercel-ai-gateway.json");

pub fn vercel_ai_gateway_provider() -> Provider {
    build_standard_provider("vercel-ai-gateway", "Vercel AI Gateway", DATA, false, false)
}
