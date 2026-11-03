use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::api::{
    build_header_map, calculate_cost, read_response_json_cancellable, require_client_api_key,
    send_with_retry,
};
use crate::types::{
    ClassifierAnswer, ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierQuestion,
    ClassifierResult, ClassifierStopReason, ImageContent, ProviderHeaders, Usage, UsageCost,
    known_classifier_api, now_ms,
};

fn detect_system_one_compat(model: &ClassifierModel) -> (String, bool) {
    let is_openrouter = model.provider == "openrouter" || model.base_url.contains("openrouter.ai");
    let default_image_input = if is_openrouter {
        "state-image-urls"
    } else {
        "assets"
    };
    let default_opt_in = !is_openrouter;

    let image_input = model
        .compat
        .as_ref()
        .and_then(|c| c.image_input.clone())
        .unwrap_or_else(|| default_image_input.to_owned());
    let send_opt_in = model
        .compat
        .as_ref()
        .and_then(|c| c.send_image_opt_in_header)
        .unwrap_or(default_opt_in);

    (image_input, send_opt_in)
}

fn add_images_to_request(
    model: &ClassifierModel,
    image_input: &str,
    images: &[ImageContent],
    request: &mut Map<String, Value>,
) -> Result<(), String> {
    if image_input == "images" {
        if images.len() > 4 {
            return Err(format!(
                "System One API accepts at most 4 images for {}, got {}",
                model.id,
                images.len()
            ));
        }
        let mut urls = Vec::with_capacity(images.len());
        for img in images {
            if !matches!(
                img.mime_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            ) {
                return Err(format!(
                    "System One API accepts PNG, JPEG, and WebP images for {}, got {}",
                    model.id, img.mime_type
                ));
            }
            urls.push(Value::String(format!(
                "data:{};base64,{}",
                img.mime_type, img.data
            )));
        }
        request.insert("images".to_owned(), Value::Array(urls));
        return Ok(());
    }

    if image_input == "state-image-urls" {
        let mut parts: Vec<Value> = images
            .iter()
            .map(|img| {
                json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:{};base64,{}", img.mime_type, img.data)
                    }
                })
            })
            .collect();
        let orig_state = request.remove("state").unwrap_or(Value::Null);
        parts.push(orig_state);
        request.insert("state".to_owned(), Value::Array(parts));
        return Ok(());
    }

    let mut assets = Map::new();
    let mut state_list = Vec::with_capacity(images.len() + 1);
    for (idx, img) in images.iter().enumerate() {
        let format = match img.mime_type.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpeg",
            other => {
                return Err(format!(
                    "System One API accepts PNG and JPEG images for {}, got {other}",
                    model.id
                ));
            }
        };
        let key = format!("<image-{}>", idx + 1);
        state_list.push(Value::String(key.clone()));
        assets.insert(
            key,
            json!({
                "type": "image",
                "format": format,
                "data_base64": img.data
            }),
        );
    }
    let orig_state = request.remove("state").unwrap_or(Value::Null);
    state_list.push(orig_state);
    request.insert("assets".to_owned(), Value::Object(assets));
    request.insert("state".to_owned(), Value::Array(state_list));
    Ok(())
}

fn build_wire_payload(
    model: &ClassifierModel,
    context: &ClassifierContext,
) -> Result<(Value, bool), String> {
    let mut questions = Map::new();
    for (id, q) in &context.questions {
        let q_val = match q {
            ClassifierQuestion::Choice {
                instructions,
                criteria,
            } => json!({
                "type": "choice",
                "instructions": instructions,
                "criteria": criteria,
            }),
            ClassifierQuestion::Score {
                instructions,
                criteria,
            } => json!({
                "type": "score",
                "instructions": instructions,
                "criteria": criteria,
            }),
            ClassifierQuestion::Bool {
                instructions,
                criteria,
            } => json!({
                "type": "noul",
                "instructions": instructions,
                "criteria": criteria,
            }),
        };
        questions.insert(id.clone(), q_val);
    }

    let mut req = Map::new();
    req.insert("model".to_owned(), Value::String(model.id.clone()));
    req.insert("state".to_owned(), Value::Object(context.state.clone()));
    req.insert("questions".to_owned(), Value::Object(questions));

    let mut need_opt_in_header = false;
    if let Some(images) = &context.images
        && !images.is_empty()
    {
        if !model.supports_image_input() {
            return Err(format!(
                "System One API does not support image input for {}",
                model.id
            ));
        }
        let (image_input, send_opt_in) = detect_system_one_compat(model);
        add_images_to_request(model, &image_input, images, &mut req)?;
        need_opt_in_header = send_opt_in;
    }

    Ok((Value::Object(req), need_opt_in_header))
}

fn parse_usage(val: Option<&Value>, model: &ClassifierModel) -> Option<Usage> {
    let obj = val?.as_object()?;
    if !obj.contains_key("input_tokens") && !obj.contains_key("output_tokens") {
        return None;
    }
    let input = obj
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output = obj
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut usage = Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output,
        cost: UsageCost::default(),
    };
    calculate_cost(&model.cost, &mut usage);
    Some(usage)
}

fn parse_answers(
    val: Option<&Value>,
    context: &ClassifierContext,
) -> Result<BTreeMap<String, ClassifierAnswer>, String> {
    let obj = val
        .and_then(|v| v.as_object())
        .ok_or_else(|| "System One API returned an unexpected response".to_owned())?;

    let mut answers = BTreeMap::new();
    for (id, question) in &context.questions {
        let ans = obj
            .get(id)
            .and_then(|v| v.as_object())
            .ok_or_else(|| format!("System One API did not return an answer for {id}"))?;
        let ans_type = ans.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match question {
            ClassifierQuestion::Choice { .. } => {
                let choice = ans
                    .get("choice")
                    .and_then(|v| v.as_str())
                    .filter(|_| ans_type == "choice")
                    .ok_or_else(|| {
                        format!("System One API did not return a choice answer for {id}")
                    })?;
                let probs_obj = ans
                    .get("probabilities")
                    .and_then(|v| v.as_object())
                    .ok_or_else(|| {
                        format!("System One API returned invalid probabilities for {id}")
                    })?;
                let mut probabilities = BTreeMap::new();
                for (k, p_val) in probs_obj {
                    let p = p_val.as_f64().ok_or_else(|| {
                        format!("System One API returned an invalid probability for {id}.{k}")
                    })?;
                    probabilities.insert(k.clone(), p);
                }
                let confidence =
                    ans.get("confidence")
                        .and_then(|v| v.as_f64())
                        .ok_or_else(|| {
                            format!("System One API returned an invalid confidence for {id}")
                        })?;
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Choice {
                        choice: choice.to_owned(),
                        probabilities,
                        confidence,
                    },
                );
            }
            ClassifierQuestion::Score { .. } => {
                if ans_type != "score" {
                    return Err(format!(
                        "System One API did not return a score answer for {id}"
                    ));
                }
                let score = ans
                    .get("score")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| format!("System One API returned an invalid score for {id}"))?;
                let confidence =
                    ans.get("confidence")
                        .and_then(|v| v.as_f64())
                        .ok_or_else(|| {
                            format!("System One API returned an invalid confidence for {id}")
                        })?;
                answers.insert(id.clone(), ClassifierAnswer::Score { score, confidence });
            }
            ClassifierQuestion::Bool { .. } => {
                if ans_type != "noul" {
                    return Err(format!(
                        "System One API did not return a bool answer for {id}"
                    ));
                }
                let probability = ans.get("noul").and_then(|v| v.as_f64()).ok_or_else(|| {
                    format!("System One API returned an invalid probability for {id}")
                })?;
                answers.insert(id.clone(), ClassifierAnswer::Bool { probability });
            }
        }
    }
    Ok(answers)
}

/// 执行一次 TypeSafe System One 结构化分类请求。
pub async fn classify(
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<&ClassifierOptions>,
) -> ClassifierResult {
    let mut output = ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: BTreeMap::new(),
        usage: None,
        stop_reason: ClassifierStopReason::Stop,
        error_message: None,
        timestamp: now_ms(),
    };

    let run = async {
        if model.api != known_classifier_api::TYPESAFE_SYSTEM_ONE {
            return Err(format!("Unsupported classifier API: {}", model.api));
        }
        let api_key = require_client_api_key(
            &model.provider,
            options.and_then(|o| o.api_key.as_deref()),
            options.and_then(|o| o.headers.as_ref()),
        )?;

        let (payload, need_opt_in_header) = build_wire_payload(model, context)?;
        let mut opt_headers: ProviderHeaders =
            options.and_then(|o| o.headers.clone()).unwrap_or_default();
        if need_opt_in_header && !opt_headers.contains_key("TypeSafe-Danger-Zone-Images") {
            opt_headers.insert(
                "TypeSafe-Danger-Zone-Images".to_owned(),
                Some("true".to_owned()),
            );
        }

        let auth_header = format!("Bearer {api_key}");
        let headers = build_header_map(
            options.and_then(|o| o.identity.as_ref()),
            &[
                ("authorization", &auth_header),
                ("content-type", "application/json"),
            ],
            model.headers.as_ref(),
            Some(&opt_headers),
        );

        let base = model.base_url.trim_end_matches('/');
        let url = format!("{base}/systemone");
        let client = reqwest::Client::new();

        let resp = send_with_retry(
            &client,
            &url,
            headers,
            &payload,
            options.and_then(|o| o.timeout_ms),
            options.and_then(|o| o.max_retries).unwrap_or(2),
            options.and_then(|o| o.max_retry_delay_ms),
            options.and_then(|o| o.cancel_token.as_ref()),
        )
        .await?;

        let body: Value =
            read_response_json_cancellable(resp, options.and_then(|o| o.cancel_token.as_ref()))
                .await
                .map_err(|e| {
                    if e == "Request was aborted" {
                        e
                    } else {
                        format!("System One API invalid JSON: {e}")
                    }
                })?;

        if let Some(usage) = parse_usage(body.get("usage"), model) {
            output.usage = Some(usage);
        }
        let answers = parse_answers(body.get("answers"), context)?;
        Ok(answers)
    };

    match run.await {
        Ok(answers) => {
            output.answers = answers;
        }
        Err(err) => {
            let aborted = options
                .and_then(|o| o.cancel_token.as_ref())
                .is_some_and(|t| t.is_cancelled());
            output.stop_reason = if aborted {
                ClassifierStopReason::Aborted
            } else {
                ClassifierStopReason::Error
            };
            output.error_message = Some(err);
        }
    }

    output
}
