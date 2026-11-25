use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA_CN: &str = include_str!("data/xiaomi-token-plan-cn.json");
const DATA_AMS: &str = include_str!("data/xiaomi-token-plan-ams.json");
const DATA_SGP: &str = include_str!("data/xiaomi-token-plan-sgp.json");
const DATA_GLOBAL: &str = include_str!("data/xiaomi.json");

pub fn xiaomi_token_plan_cn_provider() -> Provider {
    build_standard_provider(
        "xiaomi-token-plan-cn",
        "Xiaomi Token Plan (China)",
        DATA_CN,
        false,
        false,
    )
}

pub fn xiaomi_token_plan_ams_provider() -> Provider {
    build_standard_provider(
        "xiaomi-token-plan-ams",
        "Xiaomi Token Plan AMS",
        DATA_AMS,
        false,
        false,
    )
}

pub fn xiaomi_token_plan_sgp_provider() -> Provider {
    build_standard_provider(
        "xiaomi-token-plan-sgp",
        "Xiaomi Token Plan SGP",
        DATA_SGP,
        false,
        false,
    )
}

pub fn xiaomi_provider() -> Provider {
    build_standard_provider("xiaomi", "Xiaomi MiMo", DATA_GLOBAL, false, false)
}
