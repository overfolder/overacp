use overloop::llm::ToolContent;
use overloop::mcp::McpClient;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// A responder that returns different responses for initialize vs other methods.
struct McpResponder {
    responses: Mutex<VecDeque<ResponseTemplate>>,
}

impl Respond for McpResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut queue = self.responses.lock().unwrap();
        queue
            .pop_front()
            .unwrap_or_else(|| ResponseTemplate::new(500))
    }
}

fn init_response() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": 1,
        "result": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "serverInfo": {
                "name": "test-server",
                "version": "1.0.0"
            }
        }
    }))
}

fn init_response_with_session() -> ResponseTemplate {
    init_response().insert_header("Mcp-Session-Id", "session-abc-123")
}

#[tokio::test]
async fn test_list_tools() {
    let server = MockServer::start().await;

    let tools_list_response = ResponseTemplate::new(200).set_body_json(json!({
        "id": 2,
        "result": {
            "tools": [
                {
                    "name": "my_tool",
                    "description": "A test tool",
                    "inputSchema": { "type": "object", "properties": {} }
                },
                {
                    "name": "another_tool",
                    "description": "Another test tool",
                    "inputSchema": { "type": "object" }
                }
            ]
        }
    }));

    let responder = McpResponder {
        responses: Mutex::new(vec![init_response(), tools_list_response].into()),
    };

    Mock::given(method("POST"))
        .respond_with(responder)
        .expect(2)
        .mount(&server)
        .await;

    let mut client = McpClient::new(&server.uri());
    let tools = client.list_tools().await.unwrap();

    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].function.name, "my_tool");
    assert_eq!(tools[0].function.description, "A test tool");
    assert_eq!(tools[1].function.name, "another_tool");
}

#[tokio::test]
async fn test_call_tool_success() {
    let server = MockServer::start().await;

    let tool_result_response = ResponseTemplate::new(200).set_body_json(json!({
        "id": 2,
        "result": {
            "content": [
                { "type": "text", "text": "Tool output here" }
            ],
            "isError": false
        }
    }));

    let responder = McpResponder {
        responses: Mutex::new(vec![init_response(), tool_result_response].into()),
    };

    // Initialize first, then call tool
    Mock::given(method("POST"))
        .respond_with(responder)
        .expect(2)
        .mount(&server)
        .await;

    let mut client = McpClient::new(&server.uri());
    // Must initialize before calling a tool
    client.initialize().await.unwrap();
    let result = client
        .call_tool("my_tool", json!({"arg": "val"}))
        .await
        .unwrap();

    assert_eq!(result.len(), 1);
    assert!(matches!(&result[0], ToolContent::Text(t) if t == "Tool output here"));
}

#[tokio::test]
async fn test_call_tool_multimodal() {
    let server = MockServer::start().await;

    let tool_result_response = ResponseTemplate::new(200).set_body_json(json!({
        "id": 2,
        "result": {
            "content": [
                { "type": "text", "text": "Here is the screenshot:" },
                { "type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png" }
            ],
            "isError": false
        }
    }));

    let responder = McpResponder {
        responses: Mutex::new(vec![init_response(), tool_result_response].into()),
    };

    Mock::given(method("POST"))
        .respond_with(responder)
        .expect(2)
        .mount(&server)
        .await;

    let mut client = McpClient::new(&server.uri());
    client.initialize().await.unwrap();
    let result = client.call_tool("screenshot", json!({})).await.unwrap();

    assert_eq!(result.len(), 2);
    assert!(matches!(&result[0], ToolContent::Text(t) if t == "Here is the screenshot:"));
    assert!(
        matches!(&result[1], ToolContent::ImageBase64 { data, mime_type }
            if data == "iVBORw0KGgo=" && mime_type == "image/png")
    );
}

#[tokio::test]
async fn test_call_tool_resource_converted_to_text() {
    let server = MockServer::start().await;

    let tool_result_response = ResponseTemplate::new(200).set_body_json(json!({
        "id": 2,
        "result": {
            "content": [
                { "type": "resource", "resource": { "uri": "file:///etc/hosts", "text": "localhost" } }
            ],
            "isError": false
        }
    }));

    let responder = McpResponder {
        responses: Mutex::new(vec![init_response(), tool_result_response].into()),
    };

    Mock::given(method("POST"))
        .respond_with(responder)
        .expect(2)
        .mount(&server)
        .await;

    let mut client = McpClient::new(&server.uri());
    client.initialize().await.unwrap();
    let result = client.call_tool("read_resource", json!({})).await.unwrap();

    assert_eq!(result.len(), 1);
    assert!(matches!(&result[0], ToolContent::Text(t) if t.contains("localhost")));
}

#[tokio::test]
async fn test_call_tool_error() {
    let server = MockServer::start().await;

    let tool_error_response = ResponseTemplate::new(200).set_body_json(json!({
        "id": 2,
        "result": {
            "content": [
                { "type": "text", "text": "Something went wrong" }
            ],
            "isError": true
        }
    }));

    let responder = McpResponder {
        responses: Mutex::new(vec![init_response(), tool_error_response].into()),
    };

    Mock::given(method("POST"))
        .respond_with(responder)
        .expect(2)
        .mount(&server)
        .await;

    let mut client = McpClient::new(&server.uri());
    client.initialize().await.unwrap();
    let result = client.call_tool("bad_tool", json!({})).await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Something went wrong"),
        "Expected error message, got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_session_id() {
    let server = MockServer::start().await;

    // The second request (tools/list) should include the session ID from init
    let tools_response = ResponseTemplate::new(200).set_body_json(json!({
        "id": 2,
        "result": {
            "tools": []
        }
    }));

    let responder = McpResponder {
        responses: Mutex::new(vec![init_response_with_session(), tools_response].into()),
    };

    Mock::given(method("POST"))
        .respond_with(responder)
        .expect(2)
        .mount(&server)
        .await;

    let mut client = McpClient::new(&server.uri());
    let tools = client.list_tools().await.unwrap();

    // Verify tools were returned (session ID was handled internally)
    assert_eq!(tools.len(), 0);

    // Verify the server received both requests
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);

    // Second request should have the Mcp-Session-Id header
    let second_req = &requests[1];
    let session_header = second_req
        .headers
        .get("Mcp-Session-Id")
        .map(|v| v.to_str().unwrap().to_string());
    assert_eq!(session_header, Some("session-abc-123".to_string()));
}
