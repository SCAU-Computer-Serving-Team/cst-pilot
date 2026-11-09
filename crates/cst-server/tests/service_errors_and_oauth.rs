use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_server::error::{ServiceError, ServiceErrorKind};
use cst_server::oauth::OAuthFlowManager;
use cst_server::routes::ApiError;
use pi_ai::{AuthPrompt, AuthType};

#[test]
fn service_errors_are_translated_to_http_only_at_the_route_layer() {
    for (kind, status) in [
        (ServiceErrorKind::InvalidInput, 400),
        (ServiceErrorKind::Conflict, 409),
        (ServiceErrorKind::NotFound, 404),
        (ServiceErrorKind::AccessDenied, 403),
        (ServiceErrorKind::TooLarge, 413),
        (ServiceErrorKind::Unavailable, 503),
        (ServiceErrorKind::Internal, 500),
    ] {
        let api = ApiError::from(ServiceError::new(kind, "service_code", "错误详情"));
        assert_eq!(api.status.as_u16(), status);
        assert_eq!(api.code, "service_code");
        assert_eq!(api.message, "错误详情");
    }
}

#[tokio::test]
async fn oauth_flow_validates_answers_preserves_pending_prompts_and_reuses_active_login() {
    let changes = Arc::new(Mutex::new(Vec::new()));
    let observed = changes.clone();
    let manager = OAuthFlowManager::new(move |_, view| {
        observed.lock().unwrap().push(view.clone());
    });
    let flow = manager.start_or_reuse("test-provider", AuthType::OAuth, None, |interaction| {
        Box::pin(async move {
            let answer = (interaction.prompt)(AuthPrompt::Secret {
                message: "输入凭据".to_owned(),
                placeholder: None,
            })
            .await?;
            assert_eq!(answer, "secret-answer");
            Ok(())
        })
    });
    let ready = manager
        .wait_until_ready("test-provider", &flow)
        .await
        .unwrap();
    let reused = manager.start_or_reuse("test-provider", AuthType::OAuth, None, |_| {
        panic!("an active login must be reused")
    });
    assert!(Arc::ptr_eq(&flow, &reused));
    let flow_id = ready["flowId"].as_str().unwrap();
    let prompt_id = ready["prompt"]["id"].as_str().unwrap();
    let stale = manager
        .respond("test-provider", "stale-flow", prompt_id, "secret-answer")
        .unwrap_err();
    assert_eq!(stale.kind, ServiceErrorKind::Conflict);
    assert_eq!(ApiError::from(stale).status, 409);
    let invalid = manager
        .respond("test-provider", flow_id, prompt_id, " ")
        .unwrap_err();
    assert_eq!(invalid.kind, ServiceErrorKind::InvalidInput);
    assert_eq!(
        manager.status("test-provider", None)["prompt"]["id"],
        prompt_id
    );
    manager
        .respond("test-provider", flow_id, prompt_id, "secret-answer")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while manager.status("test-provider", None)["state"] == "pending" {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let done = manager.status("test-provider", None);
    assert_eq!(done["state"], "succeeded");
    assert!(done.get("prompt").is_none());
    assert!(
        !serde_json::to_string(&*changes.lock().unwrap())
            .unwrap()
            .contains("secret-answer")
    );
    manager.close_all().await;
}
