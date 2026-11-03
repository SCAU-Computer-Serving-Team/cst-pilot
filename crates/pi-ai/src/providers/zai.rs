use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA_CN: &str = include_str!("data/zai-coding-cn.json");
const DATA_GLOBAL: &str = include_str!("data/zai.json");

pub fn zai_coding_cn_provider() -> Provider {
    build_standard_provider(
        "zai-coding-cn",
        "Z.AI Coding Plan (China)",
        DATA_CN,
        false,
        false,
    )
}

pub fn zai_provider() -> Provider {
    build_standard_provider("zai", "Z.AI", DATA_GLOBAL, false, false)
}
