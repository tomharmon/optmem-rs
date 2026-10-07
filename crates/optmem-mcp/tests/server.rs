use std::{sync::Arc, time::Duration};

use optmem::{OptMem, storage::MemoryStorage};
use optmem_agent::{CallToolRequestParams, MemoryTools, ToolPolicy};
use optmem_mcp::OptMemServer;
use rmcp::{RoleClient, ServiceExt, model::CallToolResult, service::RunningService};
use serde_json::{Value, json};
use tokio::{task::JoinHandle, time::timeout};

fn request(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone())
}
fn data(result: &CallToolResult) -> &Value {
    result.structured_content.as_ref().unwrap()
}

async fn connect(
    tools: MemoryTools<MemoryStorage>,
) -> (RunningService<RoleClient, ()>, JoinHandle<()>) {
    let (server_stream, client_stream) = tokio::io::duplex(65536);
    let server = OptMemServer::new(tools);
    let handle = tokio::spawn(async move {
        server
            .serve(server_stream)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let client = timeout(Duration::from_secs(5), ().serve(client_stream))
        .await
        .unwrap()
        .unwrap();
    (client, handle)
}
async fn close(client: RunningService<RoleClient, ()>, handle: JoinHandle<()>) {
    client.cancel().await.unwrap();
    timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn initialize_lists_tools_and_completes_a_memory_lifecycle() {
    let memory = Arc::new(OptMem::new(MemoryStorage::new()));
    let (client, handle) = connect(MemoryTools::new(memory.clone())).await;
    let info = client.peer_info().unwrap();
    assert_eq!(info.server_info.as_ref().unwrap().name, "optmem-mcp");
    assert!(info.capabilities.tools.is_some());
    assert!(info.capabilities.prompts.is_none());
    assert!(
        info.instructions
            .as_ref()
            .unwrap()
            .contains("full current agent context")
    );
    let definitions = client.list_tools(None).await.unwrap();
    assert_eq!(definitions.tools.len(), 5);
    assert!(definitions.next_cursor.is_none());
    let empty = client
        .call_tool(request("memory_wake", json!({})))
        .await
        .unwrap();
    assert_eq!(data(&empty)["result"]["awake"], true);
    for text in ["first original", "second original"] {
        let note = client
            .call_tool(request("memory_note", json!({"text": text})))
            .await
            .unwrap();
        assert_eq!(note.is_error, Some(false));
    }
    let next = client
        .call_tool(request("memory_nap", json!({})))
        .await
        .unwrap();
    assert_eq!(data(&next)["result"]["next_compression"]["block"], "0-1");
    let saved = client
        .call_tool(request(
            "memory_nap",
            json!({"block": "0-1", "text": "both originals"}),
        ))
        .await
        .unwrap();
    assert_eq!(data(&saved)["result"]["submission"], "saved");
    let zoom = client
        .call_tool(request("memory_zoom", json!({"block": "0-3"})))
        .await
        .unwrap();
    assert_eq!(
        data(&zoom)["result"]["children"][0]["text"],
        "both originals"
    );
    let recall = client
        .call_tool(request("memory_recall", json!({"pattern": "FIRST"})))
        .await
        .unwrap();
    assert_eq!(
        data(&recall)["result"]["memories"][0]["text"],
        "first original"
    );
    assert_eq!(memory.len().unwrap(), 2);
    close(client, handle).await;
}

#[tokio::test]
async fn protocol_errors_and_execution_errors_stay_distinct() {
    let (client, handle) = connect(MemoryTools::new(Arc::new(
        OptMem::new(MemoryStorage::new()),
    )))
    .await;
    assert!(
        client
            .call_tool(request("not_a_tool", json!({})))
            .await
            .is_err()
    );
    assert!(
        client
            .call_tool(request("memory_note", json!({"text": 42})))
            .await
            .is_err()
    );
    assert!(
        client
            .call_tool(request("memory_nap", json!({"block": "0-1"})))
            .await
            .is_err()
    );
    let invalid = client
        .call_tool(request("memory_note", json!({"text": ""})))
        .await
        .unwrap();
    assert_eq!(invalid.is_error, Some(true));
    assert_eq!(data(&invalid)["error"]["code"], "invalid_memory_operation");
    let bad_regex = client
        .call_tool(request("memory_recall", json!({"pattern": "["})))
        .await
        .unwrap();
    assert_eq!(bad_regex.is_error, Some(true));
    close(client, handle).await;
}

#[tokio::test]
async fn read_only_connections_reject_hidden_and_lifecycle_writes() {
    let memory = Arc::new(OptMem::new(MemoryStorage::new()));
    let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
    memory.note(date, "one").unwrap();
    memory.note(date, "two").unwrap();
    let (client, handle) =
        connect(MemoryTools::new(memory.clone()).with_policy(ToolPolicy::read_only())).await;
    let definitions = client.list_tools(None).await.unwrap();
    assert!(
        !definitions
            .tools
            .iter()
            .any(|t| t.name == "memory_note" || t.name == "memory_forget")
    );
    let denied = client
        .call_tool(request("memory_note", json!({"text": "unauthorized"})))
        .await
        .unwrap();
    assert_eq!(denied.is_error, Some(true));
    let denied = client
        .call_tool(request(
            "memory_nap",
            json!({"block": "0-1", "text": "unauthorized"}),
        ))
        .await
        .unwrap();
    assert_eq!(data(&denied)["error"]["code"], "permission_denied");
    assert_eq!(memory.len().unwrap(), 2);
    assert_eq!(memory.pending_count(None).unwrap(), 1);
    close(client, handle).await;
}

#[tokio::test]
async fn explicitly_enabled_forget_is_available_over_mcp() {
    let memory = Arc::new(OptMem::new(MemoryStorage::new()));
    let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
    memory.note(date, "one").unwrap();
    memory.note(date, "two").unwrap();
    memory
        .submit_summary("0-1".parse().unwrap(), "both")
        .unwrap();
    let tools = MemoryTools::new(memory.clone()).with_policy(ToolPolicy {
        allow_notes: true,
        allow_forget: true,
    });
    let (client, handle) = connect(tools).await;
    assert_eq!(client.list_tools(None).await.unwrap().tools.len(), 6);
    let result = client
        .call_tool(request("memory_forget", json!({"block": "0-1"})))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        data(&result)["result"]["invalidated_blocks"],
        json!(["0-1"])
    );
    assert_eq!(memory.len().unwrap(), 2);
    close(client, handle).await;
}
