use pi_ai::Model;
use pi_runtime::*;

fn model(provider: &str) -> Model {
    let mut model = pi_ai::faux::create_faux_provider().model;
    model.provider = provider.into();
    model
}

fn query(text: &str) -> ModelReferenceRequest {
    ModelReferenceRequest {
        text: text.into(),
        provider: None,
        thinking_suffix: ThinkingSuffixMode::None,
        fuzzy: true,
    }
}

#[test]
fn unmatched_unicode_model_queries_return_not_found() {
    for text in ["不存在的模型", "😀😀😀", "nonexistent-model", "ééé未知"] {
        assert!(matches!(
            resolve_model_reference(&query(text), &[model("deepseek")], |_| true),
            ModelReferenceResult::NotFound { target: None }
        ));
    }
}

#[test]
fn provider_prefix_uses_original_unicode_boundaries_and_longest_match() {
    for (text, providers, expected_provider, expected_id) in [
        ("İ/模型", vec!["i\u{307}"], "i\u{307}", "模型"),
        (
            "服务商/新模型",
            vec!["deepseek", "服务商"],
            "服务商",
            "新模型",
        ),
        (
            "https://host/api/新模型",
            vec!["https:", "https://host", "https://host/api"],
            "https://host/api",
            "新模型",
        ),
        (
            "DEEPSEEK / 未知模型",
            vec!["deepseek"],
            "deepseek",
            "未知模型",
        ),
    ] {
        let models = providers.into_iter().map(model).collect::<Vec<_>>();
        let ModelReferenceResult::NotFound {
            target: Some(target),
        } = resolve_model_reference(&query(text), &models, |_| true)
        else {
            panic!("missing provider target for {text}");
        };
        assert_eq!(target.provider, expected_provider);
        assert_eq!(target.model_id, expected_id);
    }
}
