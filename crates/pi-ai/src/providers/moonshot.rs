use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA_CN: &str = include_str!("data/moonshotai-cn.json");
const DATA_GLOBAL: &str = include_str!("data/moonshotai.json");
const DATA_KIMI_CODING: &str = include_str!("data/kimi-coding.json");

pub fn moonshotai_cn_provider() -> Provider {
    build_standard_provider(
        "moonshotai-cn",
        "Moonshot AI (China)",
        DATA_CN,
        false,
        false,
    )
}

pub fn moonshotai_provider() -> Provider {
    build_standard_provider("moonshotai", "Moonshot AI", DATA_GLOBAL, false, false)
}

pub fn kimi_coding_provider() -> Provider {
    build_standard_provider(
        "kimi-coding",
        "Kimi For Coding",
        DATA_KIMI_CODING,
        true,
        true,
    )
}
