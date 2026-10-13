//! End-to-end engine tests against a real (mock) HTTP server.
//!
//! These exercise the full path: config → engine → adapter → HTTP client →
//! SSE parsing → normalised response, over an actual TCP socket. They are the
//! closest thing to "a working real endpoint" that runs in CI without keys.

use nash_core::config::AppConfig;
use nash_core::engine::Engine;
use nash_core::message::ChatMessage;
use nash_core::request::ChatRequest;
use nash_core::secrets::EnvSecretResolver;
use nash_core::stream::StreamEvent;
use futures::StreamExt;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SSE_BODY: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n\
data: [DONE]\n\n";

fn config_for(base_url: &str) -> AppConfig {
    let json = format!(
        r#"{{
            "schema_version": 1,
            "providers": [{{
                "id": "mock",
                "protocol": "openai_chat",
                "base_url": "{base_url}",
                "auth": {{"type": "none"}},
                "model": "test-model",
                "local": true
            }}],
            "default_provider": "mock",
            "routing": {{"allow_failover": false}}
        }}"#
    );
    AppConfig::from_json(&json).unwrap()
}

fn engine_for(server: &MockServer) -> Engine {
    Engine::new(config_for(&server.uri()), Box::new(EnvSecretResolver::new())).unwrap()
}

async fn mount_chat(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE_BODY),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn chat_stream_yields_normalised_events() {
    let server = MockServer::start().await;
    mount_chat(&server).await;
    let engine = engine_for(&server);

    let req = ChatRequest::new("test-model").message(ChatMessage::user("hi"));
    let mut stream = engine
        .chat_stream(req, CancellationToken::new())
        .await
        .unwrap()
        .stream;

    let mut content = String::new();
    let mut usage_total = None;
    let mut saw_done = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            StreamEvent::ContentDelta(t) => content.push_str(&t),
            StreamEvent::Usage(u) => usage_total = Some(u.total_tokens),
            StreamEvent::Done { .. } => saw_done = true,
            _ => {}
        }
    }

    assert_eq!(content, "Hello world");
    assert_eq!(usage_total, Some(5));
    assert!(saw_done);
}

#[tokio::test]
async fn chat_aggregates_stream_into_response() {
    let server = MockServer::start().await;
    mount_chat(&server).await;
    let engine = engine_for(&server);

    let req = ChatRequest::new("test-model").message(ChatMessage::user("hi"));
    let resp = engine.chat(req, CancellationToken::new()).await.unwrap();

    assert_eq!(resp.content, "Hello world");
    assert_eq!(resp.provider, "mock");
    assert_eq!(resp.model, "test-model");
    assert_eq!(resp.usage.unwrap().total_tokens, 5);
}

#[tokio::test]
async fn list_models_reads_openai_shape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"object":"list","data":[{"id":"test-model"},{"id":"other-model"}]}"#,
        ))
        .mount(&server)
        .await;
    let engine = engine_for(&server);

    let models = engine
        .list_models("mock", &CancellationToken::new())
        .await
        .unwrap();
    let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&"test-model"));
    assert!(ids.contains(&"other-model"));
}

#[tokio::test]
async fn unauthorized_is_an_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_string(r#"{"error":{"message":"missing or invalid api key"}}"#),
        )
        .mount(&server)
        .await;
    let engine = engine_for(&server);

    let req = ChatRequest::new("test-model").message(ChatMessage::user("hi"));
    let err = engine.chat(req, CancellationToken::new()).await.unwrap_err();
    assert_eq!(err.kind(), "auth");
}

#[tokio::test]
async fn cancelled_request_short_circuits() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE_BODY),
        )
        .mount(&server)
        .await;
    let engine = engine_for(&server);

    let cancel = CancellationToken::new();
    cancel.cancel();
    let req = ChatRequest::new("test-model").message(ChatMessage::user("hi"));
    let err = engine.chat(req, cancel).await.unwrap_err();
    assert_eq!(err.kind(), "cancelled");
}

#[tokio::test]
async fn validate_reports_success_for_healthy_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"object":"list","data":[{"id":"test-model"}]}"#,
        ))
        .mount(&server)
        .await;
    mount_chat(&server).await;
    let engine = engine_for(&server);

    let report = engine
        .validate("mock", &CancellationToken::new())
        .await
        .unwrap();
    assert!(report.passed(), "checks: {:?}", report.checks);
}
