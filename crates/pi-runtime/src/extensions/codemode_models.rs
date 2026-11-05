use std::sync::{Arc, Mutex};

use pi_ai::{AnyModel, ClassifierContext, ClassifierOptions, JsonValue, ModelType, Models, Usage};
use serde_json::json;
use tokio::sync::Semaphore;

use crate::codemode::CodemodeTool;

fn model_info(model: AnyModel) -> Result<JsonValue, String> {
    let mut info = serde_json::to_value(model).map_err(|error| error.to_string())?;
    if let Some(object) = info.as_object_mut() {
        object.remove("headers");
    }
    Ok(info)
}

/// Each execution gets its own limiter and usage ledger. Script model objects are
/// references only: endpoints, headers and credentials come from the session catalog.
pub(super) fn model_globals(models: Arc<Models>, usage: Arc<Mutex<Usage>>) -> Vec<CodemodeTool> {
    let limit = Arc::new(Semaphore::new(4));
    [
        "getModelsOfType",
        "getAvailableOfType",
        "getModelOfType",
        "classify",
    ]
    .into_iter()
    .map(|operation| {
        let models = models.clone();
        let usage = usage.clone();
        let limit = limit.clone();
        let mut tool = CodemodeTool::new(format!("models.{operation}"), move |args, cancel| {
            let models = models.clone();
            let usage = usage.clone();
            let limit = limit.clone();
            async move {
                let args = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
                if operation == "classify" {
                    let given = args.first().ok_or("models.classify requires a model")?;
                    let provider = given
                        .get("provider")
                        .and_then(JsonValue::as_str)
                        .ok_or("models.classify model requires a provider")?;
                    let id = given
                        .get("id")
                        .and_then(JsonValue::as_str)
                        .ok_or("models.classify model requires an id")?;
                    let model = models
                        .get_classifier_model(provider, id)
                        .ok_or_else(|| format!("Unknown classifier model {provider}/{id}"))?;
                    let context: ClassifierContext = serde_json::from_value(
                        args.get(1)
                            .cloned()
                            .ok_or("models.classify requires a context")?,
                    )
                    .map_err(|e| format!("Invalid classifier context: {e}"))?;
                    if context.questions.is_empty() {
                        return Err("Classifier context requires at least one question".to_owned());
                    }
                    let _permit = tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Err("Model call aborted".to_owned()),
                        permit = limit.acquire() => permit.map_err(|e| e.to_string())?,
                    };
                    let result = models
                        .classify(
                            &model,
                            &context,
                            Some(&ClassifierOptions {
                                cancel_token: Some(cancel),
                                ..Default::default()
                            }),
                        )
                        .await;
                    if let Some(reported) = &result.usage {
                        let mut total = usage.lock().expect("model usage lock");
                        *total = pi_session::combine_usage(&total, reported);
                    }
                    return serde_json::to_value(result)
                        .map(Some)
                        .map_err(|e| e.to_string());
                }

                let model_type: ModelType =
                    serde_json::from_value(args.first().cloned().unwrap_or(JsonValue::Null))
                        .map_err(|_| "Expected model type 'chat' or 'classifier'".to_owned())?;
                let provider = match args.get(1) {
                    None | Some(JsonValue::Null) => None,
                    Some(JsonValue::String(provider)) => Some(provider.as_str()),
                    _ => return Err("Model provider must be a string".to_owned()),
                };
                let mut found = models.get_models_of_type(model_type, provider);
                if operation == "getModelOfType" {
                    let provider = provider.ok_or("getModelOfType requires a provider")?;
                    let id = args
                        .get(2)
                        .and_then(JsonValue::as_str)
                        .ok_or("getModelOfType requires a model id")?;
                    return found
                        .into_iter()
                        .find(|m| m.provider() == provider && m.id() == id)
                        .map(model_info)
                        .transpose();
                }
                if operation == "getAvailableOfType" {
                    let query = async {
                        if model_type == ModelType::Chat {
                            let available = models
                                .get_available(provider)
                                .await
                                .map_err(|e| e.to_string())?;
                            found.retain(|m| {
                                available
                                    .iter()
                                    .any(|a| a.provider == m.provider() && a.id == m.id())
                            });
                        } else {
                            let mut authenticated = std::collections::BTreeSet::new();
                            for model in &found {
                                if !authenticated.contains(model.provider())
                                    && models
                                        .check_auth(model.provider())
                                        .await
                                        .map_err(|e| e.to_string())?
                                        .is_some()
                                {
                                    authenticated.insert(model.provider().to_owned());
                                }
                            }
                            found.retain(|m| authenticated.contains(m.provider()));
                        }
                        Ok::<(), String>(())
                    };
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Err("Model lookup aborted".to_owned()),
                        result = query => result?,
                    }
                }
                Ok(Some(json!(
                    found
                        .into_iter()
                        .map(model_info)
                        .collect::<Result<Vec<_>, _>>()?
                )))
            }
        });
        tool.spread = true;
        tool
    })
    .collect()
}
