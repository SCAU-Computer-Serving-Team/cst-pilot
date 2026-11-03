use crate::models::Provider;
use crate::providers::build_standard_provider;

const DATA_CN: &str = include_str!("data/minimax-cn.json");
const DATA_GLOBAL: &str = include_str!("data/minimax.json");

pub fn minimax_cn_provider() -> Provider {
    build_standard_provider("minimax-cn", "MiniMax (China)", DATA_CN, false, false)
}

pub fn minimax_provider() -> Provider {
    build_standard_provider("minimax", "MiniMax", DATA_GLOBAL, false, false)
}
