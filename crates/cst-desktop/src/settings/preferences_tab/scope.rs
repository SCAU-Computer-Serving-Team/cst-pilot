use std::collections::HashSet;

use cst_client::{ModelScopeMatchDto, ModelScopeRuleDto, ModelsSnapshot};

#[derive(Clone, Copy, PartialEq, Eq)]
struct RulePosition {
    rule: usize,
    member: usize,
}

#[derive(Clone, PartialEq, Eq)]
struct ScopeRow {
    id: Option<String>,
    pattern: String,
    origin: Option<RulePosition>,
}

#[derive(Clone)]
struct OriginalRule {
    pattern: String,
    row_count: usize,
}

#[derive(Clone, Default)]
pub(super) struct ModelScope {
    original: Vec<OriginalRule>,
    rows: Vec<ScopeRow>,
}

impl ModelScope {
    pub(super) fn from_snapshot(snapshot: &ModelsSnapshot) -> Self {
        if let Some(rules) = &snapshot.scope_rules {
            return Self::from_rules(rules);
        }
        let mut scope = Self::default();
        if let Some(enabled) = &snapshot.enabled {
            for id in enabled {
                scope.push(id.clone());
            }
            // Older servers cannot distinguish unmatched expressions from model IDs.
            scope
                .rows
                .extend(snapshot.unavailable_enabled.iter().map(|pattern| ScopeRow {
                    id: None,
                    pattern: pattern.clone(),
                    origin: None,
                }));
        } else {
            for model in snapshot
                .models
                .iter()
                .filter(|m| m.credential_ready && m.catalog_present)
            {
                scope.push(format!("{}/{}", model.provider, model.id));
            }
        }
        scope
    }

    fn from_rules(rules: &[ModelScopeRuleDto]) -> Self {
        let mut scope = Self::default();
        for (rule, entry) in rules.iter().enumerate() {
            scope.original.push(OriginalRule {
                pattern: entry.pattern.clone(),
                row_count: entry.matches.len().max(1),
            });
            if entry.matches.is_empty() {
                scope.rows.push(ScopeRow {
                    id: None,
                    pattern: entry.pattern.clone(),
                    origin: Some(RulePosition { rule, member: 0 }),
                });
            } else {
                scope.rows.extend(entry.matches.iter().enumerate().map(
                    |(member, ModelScopeMatchDto { id, pattern })| ScopeRow {
                        id: Some(id.clone()),
                        pattern: pattern.clone(),
                        origin: Some(RulePosition { rule, member }),
                    },
                ));
            }
        }
        scope
    }

    pub(super) fn patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();
        let mut offset = 0;
        while offset < self.rows.len() {
            let row = &self.rows[offset];
            // An intact group can still be evaluated against future catalogs. Only
            // edited or split groups need to be saved as individual model IDs.
            if let Some(origin) = row.origin
                && origin.member == 0
            {
                let rule = &self.original[origin.rule];
                let end = offset + rule.row_count;
                if let Some(group) = self.rows.get(offset..end)
                    && group.iter().enumerate().all(|(member, row)| {
                        row.origin
                            == Some(RulePosition {
                                rule: origin.rule,
                                member,
                            })
                    })
                {
                    patterns.push(rule.pattern.clone());
                    offset = end;
                    continue;
                }
            }
            patterns.push(row.pattern.clone());
            offset += 1;
        }
        patterns
    }

    pub(super) fn model_rows(&self) -> impl Iterator<Item = (usize, &str)> {
        let mut seen = HashSet::new();
        self.rows
            .iter()
            .enumerate()
            .filter_map(move |(index, row)| {
                let id = row.id.as_deref()?;
                seen.insert(id).then_some((index, id))
            })
    }

    pub(super) fn model_id_at(&self, index: usize) -> Option<&str> {
        self.rows.get(index)?.id.as_deref()
    }

    fn push(&mut self, id: String) {
        self.rows.push(ScopeRow {
            pattern: id.clone(),
            id: Some(id),
            origin: None,
        });
    }

    pub(super) fn toggle(&mut self, id: String) {
        if !self.clear([id.as_str()]) {
            self.push(id);
        }
    }

    pub(super) fn select(&mut self, ids: Vec<String>) -> bool {
        let mut enabled: HashSet<String> = self.model_rows().map(|(_, id)| id.to_owned()).collect();
        let previous_len = self.rows.len();
        for id in ids {
            if enabled.insert(id.clone()) {
                self.push(id);
            }
        }
        self.rows.len() != previous_len
    }

    pub(super) fn clear<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>) -> bool {
        let ids: HashSet<_> = ids.into_iter().collect();
        let previous_len = self.rows.len();
        self.rows
            .retain(|row| row.id.as_deref().is_none_or(|id| !ids.contains(id)));
        self.rows.len() != previous_len
    }

    pub(super) fn swap(&mut self, a: usize, b: usize) -> bool {
        let (first, last) = (a.min(b), a.max(b));
        let Some(moving_later) = self.model_id_at(first).map(str::to_owned) else {
            return false;
        };
        if first == last || self.model_id_at(last).is_none() {
            return false;
        }
        self.rows.swap(first, last);
        // Overlapping rules can contain the earlier model again between the two
        // slots. Remove those matches so they cannot take precedence after the move.
        let mut index = 0;
        self.rows.retain(|row| {
            let crossed_shadow =
                first < index && index < last && row.id.as_ref() == Some(&moving_later);
            index += 1;
            !crossed_shadow
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, matches: &[(&str, &str)]) -> ModelScopeRuleDto {
        ModelScopeRuleDto {
            pattern: pattern.to_owned(),
            matches: matches
                .iter()
                .map(|&(id, pattern)| ModelScopeMatchDto {
                    id: id.to_owned(),
                    pattern: pattern.to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn editing_an_unrelated_model_preserves_complete_and_unmatched_rules() {
        let mut scope = ModelScope::from_rules(&[
            rule(
                "DeepSeek/*:high",
                &[
                    ("deepseek/a", "deepseek/a:high"),
                    ("deepseek/b", "deepseek/b:high"),
                ],
            ),
            rule("missing/*", &[]),
            rule("other/x", &[("other/x", "other/x")]),
        ]);
        scope.toggle("other/x".into());
        assert_eq!(scope.patterns(), ["DeepSeek/*:high", "missing/*"]);
        assert!(scope.select(vec!["deepseek/a".into(), "other/y".into()]));
        assert_eq!(
            scope.patterns(),
            ["DeepSeek/*:high", "missing/*", "other/y"]
        );
        assert!(scope.clear(["other/y"]));
        assert_eq!(scope.patterns(), ["DeepSeek/*:high", "missing/*"]);
    }

    #[test]
    fn removing_a_model_updates_all_matching_rules_and_keeps_other_members_thinking() {
        let mut scope = ModelScope::from_rules(&[
            rule(
                "deepseek/*:high",
                &[
                    ("deepseek/a", "deepseek/a:high"),
                    ("deepseek/b", "deepseek/b:high"),
                ],
            ),
            rule(
                "*/a:low",
                &[("deepseek/a", "deepseek/a:low"), ("other/a", "other/a:low")],
            ),
        ]);
        scope.toggle("deepseek/a".into());
        assert_eq!(scope.patterns(), ["deepseek/b:high", "other/a:low"]);
    }

    #[test]
    fn swapping_rows_keeps_unrelated_rules_and_respects_overlaps() {
        let mut scope = ModelScope::from_rules(&[
            rule("deepseek/a:high", &[("deepseek/a", "deepseek/a:high")]),
            rule("other/*", &[("other/a", "other/a"), ("other/b", "other/b")]),
            rule(
                "*/a:low",
                &[("deepseek/a", "deepseek/a:low"), ("other/a", "other/a:low")],
            ),
            rule("last/x", &[("last/x", "last/x")]),
        ]);
        assert!(scope.swap(0, 5));
        assert_eq!(
            scope.patterns(),
            ["last/x", "other/*", "other/a:low", "deepseek/a:high"]
        );
        let ids: Vec<_> = scope.model_rows().map(|(_, id)| id).collect();
        assert_eq!(ids, ["last/x", "other/a", "other/b", "deepseek/a"]);
    }
}
