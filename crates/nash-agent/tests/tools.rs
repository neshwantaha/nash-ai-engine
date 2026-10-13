//! Integration tests for the built-in tools, run against a real tempdir.

use nash_agent::tools::{ToolContext, ToolRegistry};
use tokio_util::sync::CancellationToken;

fn registry() -> ToolRegistry {
    ToolRegistry::builtin()
}

async fn ctx(dir: &std::path::Path) -> ToolContext {
    ToolContext::new(dir.to_path_buf(), CancellationToken::new())
}

#[tokio::test]
async fn write_then_read_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let c = ctx(dir.path()).await;

    let out = reg
        .call(
            "write_file",
            serde_json::json!({ "path": "sub/hello.txt", "content": "line1\nline2\nline3" }),
            &c,
        )
        .await
        .unwrap();
    assert!(!out.is_error);

    let read = reg
        .call("read_file", serde_json::json!({ "path": "sub/hello.txt" }), &c)
        .await
        .unwrap();
    assert_eq!(read.content, "line1\nline2\nline3");

    let windowed = reg
        .call(
            "read_file",
            serde_json::json!({ "path": "sub/hello.txt", "start_line": 2, "max_lines": 1 }),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(windowed.content, "line2");
}

#[tokio::test]
async fn edit_file_replaces_unique_match() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let c = ctx(dir.path()).await;
    reg.call(
        "write_file",
        serde_json::json!({ "path": "a.txt", "content": "alpha beta gamma" }),
        &c,
    )
    .await
    .unwrap();

    let out = reg
        .call(
            "edit_file",
            serde_json::json!({ "path": "a.txt", "old_string": "beta", "new_string": "BETA" }),
            &c,
        )
        .await
        .unwrap();
    assert!(!out.is_error);

    let read = reg
        .call("read_file", serde_json::json!({ "path": "a.txt" }), &c)
        .await
        .unwrap();
    assert_eq!(read.content, "alpha BETA gamma");
}

#[tokio::test]
async fn edit_file_rejects_non_unique_match() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let c = ctx(dir.path()).await;
    reg.call(
        "write_file",
        serde_json::json!({ "path": "a.txt", "content": "x x" }),
        &c,
    )
    .await
    .unwrap();

    let out = reg
        .call(
            "edit_file",
            serde_json::json!({ "path": "a.txt", "old_string": "x", "new_string": "y" }),
            &c,
        )
        .await
        .unwrap();
    assert!(out.is_error);
}

#[tokio::test]
async fn list_dir_marks_directories() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let c = ctx(dir.path()).await;
    reg.call(
        "write_file",
        serde_json::json!({ "path": "nested/file.txt", "content": "hi" }),
        &c,
    )
    .await
    .unwrap();

    let out = reg
        .call("list_dir", serde_json::json!({ "path": "." }), &c)
        .await
        .unwrap();
    assert!(out.content.contains("nested/"));
}

#[tokio::test]
async fn search_text_finds_matches_with_line_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let c = ctx(dir.path()).await;
    reg.call(
        "write_file",
        serde_json::json!({ "path": "code.rs", "content": "fn main() {}\nlet todo = 1;\n" }),
        &c,
    )
    .await
    .unwrap();

    let out = reg
        .call("search_text", serde_json::json!({ "pattern": "todo" }), &c)
        .await
        .unwrap();
    assert!(out.content.contains("code.rs:2:"));
}

#[tokio::test]
async fn glob_files_matches_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let c = ctx(dir.path()).await;
    reg.call(
        "write_file",
        serde_json::json!({ "path": "src/lib.rs", "content": "" }),
        &c,
    )
    .await
    .unwrap();
    reg.call(
        "write_file",
        serde_json::json!({ "path": "src/main.rs", "content": "" }),
        &c,
    )
    .await
    .unwrap();

    let out = reg
        .call("glob_files", serde_json::json!({ "pattern": "src/*.rs" }), &c)
        .await
        .unwrap();
    assert!(out.content.contains("lib.rs"));
    assert!(out.content.contains("main.rs"));
}

#[tokio::test]
async fn path_escape_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let c = ctx(dir.path()).await;
    let err = reg
        .call(
            "read_file",
            serde_json::json!({ "path": "../../etc/passwd" }),
            &c,
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "security");
}

#[tokio::test]
async fn run_command_captures_output() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let c = ctx(dir.path()).await;
    let out = reg
        .call(
            "run_command",
            serde_json::json!({ "command": "echo nash-agent" }),
            &c,
        )
        .await
        .unwrap();
    assert!(out.content.contains("nash-agent"));
    assert!(!out.is_error);
}
