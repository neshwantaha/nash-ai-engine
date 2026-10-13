//! Runs the real `nash` binary against a mock provider to verify the CLI wires
//! config, the engine and streaming together end to end.

use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SSE_BODY: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\" CLI\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":null}]}\n\n\
data: [DONE]\n\n";

fn write_config(dir: &std::path::Path, base_url: &str) -> std::path::PathBuf {
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
    let path = dir.join("config.json");
    std::fs::write(&path, json).unwrap();
    path
}

fn nash(dir: &std::path::Path, config: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_nash"))
        .args(args)
        .arg("--config")
        .arg(config)
        .env("NASH_CONFIG_DIR", dir)
        .env("NASH_DB", dir.join("nash.sqlite"))
        .output()
        .expect("failed to run nash")
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_lists_config() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), &server.uri());
    let dir = dir.path().to_path_buf();

    let out = tokio::task::spawn_blocking(move || nash(&dir, &config, &["providers"]))
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("mock"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_streams_from_mock_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE_BODY),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), &server.uri());
    let dir = dir.path().to_path_buf();

    let out = tokio::task::spawn_blocking(move || {
        nash(&dir, &config, &["chat", "hi", "--model", "test-model"])
    })
    .await
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("Hello CLI"), "stdout: {stdout}");
}
