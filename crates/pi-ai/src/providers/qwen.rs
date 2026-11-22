use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA_CN: &str = include_str!("data/qwen-token-plan-cn.json");
const DATA_GLOBAL: &str = include_str!("data/qwen-token-plan.json");
const DATA_INDIVIDUAL: &str = include_str!("data/qwen-token-plan-individual.json");
const DATA_INDIVIDUAL_CN: &str = include_str!("data/qwen-token-plan-individual-cn.json");

pub fn qwen_token_plan_cn_provider() -> Provider {
    build_standard_provider(
        "qwen-token-plan-cn",
        "Qwen Token Plan (China)",
        DATA_CN,
        false,
        false,
    )
}

pub fn qwen_token_plan_provider() -> Provider {
    build_standard_provider(
        "qwen-token-plan",
        "Qwen Token Plan",
        DATA_GLOBAL,
        false,
        false,
    )
}

pub fn qwen_token_plan_individual_provider() -> Provider {
    build_standard_provider(
        "qwen-token-plan-individual",
        "Qwen Token Plan (Individual)",
        DATA_INDIVIDUAL,
        false,
        false,
    )
}

pub fn qwen_token_plan_individual_cn_provider() -> Provider {
    build_standard_provider(
        "qwen-token-plan-individual-cn",
        "Qwen Token Plan Individual CN",
        DATA_INDIVIDUAL_CN,
        false,
        false,
    )
}
