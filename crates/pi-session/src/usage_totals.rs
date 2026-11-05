use std::collections::BTreeMap;

use pi_agent::AgentMessage;
use pi_ai::{Usage, UsageCost};
use serde::{Deserialize, Serialize};

use crate::session_manager::SessionEntry;

/// 累计 Token 与费用汇总（对照 `UsageTotals`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
}

pub fn create_usage_totals() -> UsageTotals {
    UsageTotals::default()
}

pub fn add_usage_to_totals(totals: &mut UsageTotals, usage: &Usage) {
    totals.input = totals.input.saturating_add(usage.input);
    totals.output = totals.output.saturating_add(usage.output);
    totals.cache_read = totals.cache_read.saturating_add(usage.cache_read);
    totals.cache_write = totals.cache_write.saturating_add(usage.cache_write);
    totals.cost += usage.cost.total;
}

/// 合并两次 `Usage`（保留 `cache_write_1h` 与 `reasoning` 可选分项，且仅当两侧均为供应商报告费用时 `total_from_provider` 为 `Some(true)`）。
pub fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    let cache_write_1h = if first.cache_write_1h.is_some() || second.cache_write_1h.is_some() {
        Some(
            first
                .cache_write_1h
                .unwrap_or(0)
                .saturating_add(second.cache_write_1h.unwrap_or(0)),
        )
    } else {
        None
    };

    let reasoning = if first.reasoning.is_some() || second.reasoning.is_some() {
        Some(
            first
                .reasoning
                .unwrap_or(0)
                .saturating_add(second.reasoning.unwrap_or(0)),
        )
    } else {
        None
    };

    let total_from_provider = if first.cost.total_from_provider == Some(true)
        && second.cost.total_from_provider == Some(true)
    {
        Some(true)
    } else {
        Some(false)
    };

    Usage {
        input: first.input.saturating_add(second.input),
        output: first.output.saturating_add(second.output),
        cache_read: first.cache_read.saturating_add(second.cache_read),
        cache_write: first.cache_write.saturating_add(second.cache_write),
        cache_write_1h,
        reasoning,
        total_tokens: first.total_tokens.saturating_add(second.total_tokens),
        cost: UsageCost {
            input: first.cost.input + second.cost.input,
            output: first.cost.output + second.cost.output,
            cache_read: first.cost.cache_read + second.cost.cache_read,
            cache_write: first.cost.cache_write + second.cost.cache_write,
            total: first.cost.total + second.cost.total,
            total_from_provider,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCostBreakdownEntry {
    pub key: String,
    pub cost: f64,
    pub tokens: u64,
}

/// 按模型归属分组统计会话条目用量，工具与总结用量归入 `"Tools/summaries"`（对照 `getUsageCostBreakdown`）。
pub fn get_usage_cost_breakdown(entries: &[SessionEntry]) -> Vec<UsageCostBreakdownEntry> {
    let mut totals_by_key: BTreeMap<String, UsageTotals> = BTreeMap::new();

    for entry in entries {
        let (key, usage): (Option<String>, Option<&Usage>) = match entry {
            SessionEntry::Message(m) => match &m.message {
                AgentMessage::Assistant(a) => {
                    let model_name = a.response_model.as_deref().unwrap_or(&a.model);
                    (Some(format!("{}/{model_name}", a.provider)), Some(&a.usage))
                }
                AgentMessage::ToolResult(tr) => {
                    if let Some(u) = &tr.usage {
                        (Some("Tools/summaries".to_owned()), Some(u))
                    } else {
                        (None, None)
                    }
                }
                _ => (None, None),
            },
            SessionEntry::Usage(u) => (Some(format!("{}/{}", u.provider, u.model)), Some(&u.usage)),
            SessionEntry::BranchSummary(b) => (
                b.usage.as_ref().map(|_| "Tools/summaries".to_owned()),
                b.usage.as_ref(),
            ),
            SessionEntry::Compaction(c) => (
                c.usage.as_ref().map(|_| "Tools/summaries".to_owned()),
                c.usage.as_ref(),
            ),
            _ => (None, None),
        };

        if let (Some(k), Some(u)) = (key, usage) {
            let totals = totals_by_key.entry(k).or_default();
            add_usage_to_totals(totals, u);
        }
    }

    let mut out: Vec<UsageCostBreakdownEntry> = totals_by_key
        .into_iter()
        .map(|(key, totals)| UsageCostBreakdownEntry {
            key,
            cost: totals.cost,
            tokens: totals
                .input
                .saturating_add(totals.output)
                .saturating_add(totals.cache_read)
                .saturating_add(totals.cache_write),
        })
        .filter(|e| e.cost > 0.0 || e.tokens > 0)
        .collect();

    out.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}
