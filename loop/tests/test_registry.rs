use overloop::tools::ToolRegistry;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_has_5_builtins() {
    let registry = ToolRegistry::new();
    assert_eq!(registry.definitions().len(), 5);
}

#[test]
fn test_builtin_names() {
    let registry = ToolRegistry::new();
    let defs = registry.definitions();
    let mut names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
    names.sort();

    assert_eq!(names, vec!["exec", "glob", "grep", "read", "write"]);
}

#[tokio::test]
async fn test_execute_unknown() {
    let mut registry = ToolRegistry::new();
    let result = registry.execute("nonexistent", json!({})).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("unknown tool"),
        "Expected 'unknown tool' in: {}",
        err
    );
}

#[tokio::test]
async fn test_execute_read() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.txt");
    fs::write(&path, "hello from registry\n").unwrap();

    let mut registry = ToolRegistry::new();
    let result = registry
        .execute("read", json!({ "path": path.to_str().unwrap() }))
        .await;

    let output = result.unwrap();
    assert!(output.contains("hello from registry"));
    assert!(output.contains("1\t")); // line number
}
