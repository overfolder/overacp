use overloop::llm::ToolContent;
use overloop::tools::{
    tool_exec, tool_glob, tool_grep, tool_read, tool_read_media, tool_write, ToolOutput,
};
use serde_json::json;
use std::fs;
use tempfile::TempDir;

/// Extract the inner text from a ToolOutput::Text, panicking on Blocks.
fn unwrap_text(output: ToolOutput) -> String {
    match output {
        ToolOutput::Text(s) => s,
        ToolOutput::Blocks(_) => panic!("expected Text, got Blocks"),
    }
}

#[tokio::test]
async fn test_read_basic() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("hello.txt");
    fs::write(&path, "line one\nline two\nline three\n").unwrap();

    let result = tool_read(json!({ "path": path.to_str().unwrap() })).await;
    let output = unwrap_text(result.unwrap());
    assert!(output.contains("1\tline one"));
    assert!(output.contains("2\tline two"));
    assert!(output.contains("3\tline three"));
}

#[tokio::test]
async fn test_read_offset_limit() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("lines.txt");
    let content: String = (1..=10).map(|i| format!("line {}\n", i)).collect();
    fs::write(&path, &content).unwrap();

    let result = tool_read(json!({
        "path": path.to_str().unwrap(),
        "offset": 3,
        "limit": 2
    }))
    .await;
    let output = unwrap_text(result.unwrap());
    // offset=3 means start at index 3 (4th line), limit=2 means 2 lines
    assert!(output.contains("4\tline 4"));
    assert!(output.contains("5\tline 5"));
    assert!(!output.contains("line 3"));
    assert!(!output.contains("line 6"));
}

#[tokio::test]
async fn test_read_missing_file() {
    let result = tool_read(json!({ "path": "/tmp/nonexistent_overloop_test_file_xyz.txt" })).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_write_basic() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("output.txt");

    let result = tool_write(json!({
        "path": path.to_str().unwrap(),
        "content": "hello world"
    }))
    .await;
    assert!(result.is_ok());
    let written = fs::read_to_string(&path).unwrap();
    assert_eq!(written, "hello world");
}

#[tokio::test]
async fn test_write_creates_parents() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a").join("b").join("c").join("deep.txt");

    let result = tool_write(json!({
        "path": path.to_str().unwrap(),
        "content": "nested content"
    }))
    .await;
    assert!(result.is_ok());
    let written = fs::read_to_string(&path).unwrap();
    assert_eq!(written, "nested content");
}

#[tokio::test]
async fn test_exec_basic() {
    let result = tool_exec(json!({ "command": "echo hello" })).await;
    let output = unwrap_text(result.unwrap());
    assert!(output.contains("hello"));
    assert!(output.contains("[exit code: 0]"));
}

#[tokio::test]
async fn test_exec_timeout() {
    let result = tool_exec(json!({
        "command": "sleep 10",
        "timeout": 1
    }))
    .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("timed out"));
}

#[tokio::test]
async fn test_exec_exit_code() {
    let result = tool_exec(json!({ "command": "exit 42" })).await;
    let output = unwrap_text(result.unwrap());
    assert!(output.contains("[exit code: 42]"));
}

#[tokio::test]
async fn test_glob_match() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("foo.rs"), "").unwrap();
    fs::write(dir.path().join("bar.rs"), "").unwrap();
    fs::write(dir.path().join("baz.txt"), "").unwrap();

    let result = tool_glob(json!({
        "pattern": "*.rs",
        "path": dir.path().to_str().unwrap()
    }))
    .await;
    let output = unwrap_text(result.unwrap());
    assert!(output.contains("foo.rs"));
    assert!(output.contains("bar.rs"));
    assert!(!output.contains("baz.txt"));
}

#[tokio::test]
async fn test_glob_no_match() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("foo.rs"), "").unwrap();

    let result = tool_glob(json!({
        "pattern": "*.xyz",
        "path": dir.path().to_str().unwrap()
    }))
    .await;
    let output = unwrap_text(result.unwrap());
    assert!(output.contains("No matches"));
}

#[tokio::test]
async fn test_grep_match() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("data.txt"), "hello world\ngoodbye\n").unwrap();

    let result = tool_grep(json!({
        "pattern": "hello",
        "path": dir.path().to_str().unwrap()
    }))
    .await;
    let output = unwrap_text(result.unwrap());
    assert!(output.contains("hello world"));
}

#[tokio::test]
async fn test_grep_no_match() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("data.txt"), "hello world\n").unwrap();

    let result = tool_grep(json!({
        "pattern": "nonexistent",
        "path": dir.path().to_str().unwrap()
    }))
    .await;
    let output = unwrap_text(result.unwrap());
    assert!(output.contains("No matches"));
}

// ---- read_media tests ----

#[tokio::test]
async fn test_read_media_png() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.png");
    // Write a tiny valid-ish PNG (doesn't need to be fully valid — we
    // just need binary bytes and the .png extension).
    fs::write(&path, b"\x89PNG\r\n\x1a\nfake").unwrap();

    let result = tool_read_media(json!({ "path": path.to_str().unwrap() })).await;
    let output = result.unwrap();

    match output {
        ToolOutput::Blocks(blocks) => {
            assert_eq!(blocks.len(), 1);
            match &blocks[0] {
                ToolContent::ImageBase64 { data, mime_type } => {
                    assert_eq!(mime_type, "image/png");
                    assert!(!data.is_empty());
                }
                other => panic!("expected ImageBase64, got {:?}", other),
            }
        }
        ToolOutput::Text(_) => panic!("expected Blocks, got Text"),
    }
}

#[tokio::test]
async fn test_read_media_missing_file() {
    let result =
        tool_read_media(json!({ "path": "/tmp/nonexistent_overloop_media_xyz.png" })).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_read_media_unknown_extension() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("data.xyz");
    fs::write(&path, b"some bytes").unwrap();

    // Without media_type override, should error
    let result = tool_read_media(json!({ "path": path.to_str().unwrap() })).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("cannot determine media type"));
}

#[tokio::test]
async fn test_read_media_mime_override() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("data.bin");
    fs::write(&path, b"fake image data").unwrap();

    let result = tool_read_media(json!({
        "path": path.to_str().unwrap(),
        "media_type": "image/png"
    }))
    .await;
    let output = result.unwrap();
    match output {
        ToolOutput::Blocks(blocks) => {
            assert_eq!(blocks.len(), 1);
            assert!(matches!(
                &blocks[0],
                ToolContent::ImageBase64 { mime_type, .. } if mime_type == "image/png"
            ));
        }
        ToolOutput::Text(_) => panic!("expected Blocks"),
    }
}

#[tokio::test]
async fn test_read_media_mime_override_takes_priority() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("photo.png");
    fs::write(&path, b"fake").unwrap();

    // Override to jpeg even though extension is png
    let result = tool_read_media(json!({
        "path": path.to_str().unwrap(),
        "media_type": "image/jpeg"
    }))
    .await;
    let output = result.unwrap();
    match output {
        ToolOutput::Blocks(blocks) => {
            assert!(matches!(
                &blocks[0],
                ToolContent::ImageBase64 { mime_type, .. } if mime_type == "image/jpeg"
            ));
        }
        ToolOutput::Text(_) => panic!("expected Blocks"),
    }
}

#[tokio::test]
async fn test_read_media_jpeg_extension() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("photo.jpeg");
    fs::write(&path, b"not real jpeg").unwrap();

    let result = tool_read_media(json!({ "path": path.to_str().unwrap() })).await;
    let output = result.unwrap();
    match output {
        ToolOutput::Blocks(blocks) => {
            assert!(matches!(
                &blocks[0],
                ToolContent::ImageBase64 { mime_type, .. } if mime_type == "image/jpeg"
            ));
        }
        ToolOutput::Text(_) => panic!("expected Blocks"),
    }
}

#[tokio::test]
async fn test_read_media_jpg_extension() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("photo.jpg");
    fs::write(&path, b"not real jpg").unwrap();

    let result = tool_read_media(json!({ "path": path.to_str().unwrap() })).await;
    assert!(result.is_ok());
}
