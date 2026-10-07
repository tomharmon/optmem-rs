use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use chrono::NaiveDate;
use optmem::{
    Error, OptMem,
    storage::{FileStorage, MemoryStorage, Storage, StorageTransaction},
};
use optmem_agent::{
    CallToolRequestParams, CallToolResult, MemoryTools, Tool, ToolPolicy,
    rmcp::model::ErrorCode,
    types::{NoteOutput, ToolResponse},
};
use serde_json::{Value, json};

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 7).unwrap()
}
fn request(name: &str, args: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_owned()).with_arguments(args.as_object().unwrap().clone())
}
fn tools() -> MemoryTools<MemoryStorage> {
    MemoryTools::new(Arc::new(OptMem::new(MemoryStorage::new())))
}
fn call<S: Storage>(tools: &MemoryTools<S>, name: &str, args: Value) -> CallToolResult {
    tools.call_tool(request(name, args), day()).unwrap()
}
fn data(result: &CallToolResult) -> &Value {
    result.structured_content.as_ref().unwrap()
}
fn validate_result(tool: &Tool, result: &CallToolResult) {
    let schema = Value::Object(tool.output_schema.as_ref().unwrap().as_ref().clone());
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(data(result))
        .unwrap();
    let wire = serde_json::to_value(result).unwrap();
    let text: Value = serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(&text, data(result));
    let roundtrip: CallToolResult = serde_json::from_value(wire).unwrap();
    assert_eq!(roundtrip, *result);
}

#[test]
fn catalog_uses_rmcp_schemas_annotations_and_wire_names() {
    let tools = tools();
    let definitions = tools.tool_definitions();
    assert_eq!(
        definitions
            .iter()
            .map(|t| t.name.as_ref())
            .collect::<Vec<_>>(),
        ["memory_note", "memory_recall", "memory_zoom"]
    );
    for tool in &definitions {
        let wire = serde_json::to_value(tool).unwrap();
        assert!(wire.get("inputSchema").is_some());
        assert!(wire.get("outputSchema").is_some());
        assert_eq!(wire["inputSchema"]["additionalProperties"], false);
        assert!(tool.description.as_ref().is_some_and(|s| !s.is_empty()));
        let annotations = tool.annotations.as_ref().unwrap();
        assert_eq!(annotations.open_world_hint, Some(false));
        let read_only = tool.name != "memory_note";
        assert_eq!(annotations.read_only_hint, Some(read_only));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(read_only));
        assert_eq!(serde_json::from_value::<Tool>(wire).unwrap(), *tool);
    }
}

#[test]
fn note_returns_a_typed_receipt_and_leaves_compression_for_the_host() {
    let tools = tools();
    let result = call(&tools, "memory_note", json!({"text": "  lasting fact  "}));
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        data(&result)["result"]["memory"],
        json!({"id": 0, "date": "2026-10-07", "text": "lasting fact"})
    );
    assert_eq!(data(&result)["result"]["compression_pending"], 0);
    assert_eq!(
        result
            .clone()
            .into_typed::<ToolResponse<NoteOutput>>()
            .unwrap(),
        ToolResponse::Ok {
            result: NoteOutput {
                memory: optmem_agent::types::MemoryRecord {
                    id: 0,
                    date: "2026-10-07".into(),
                    text: "lasting fact".into()
                },
                compression_pending: Some(0),
                maintenance_warning: None,
            }
        }
    );
    validate_result(&tools.tool_definitions()[0], &result);
    let second = call(&tools, "memory_note", json!({"text": "another fact"}));
    assert_eq!(data(&second)["result"]["compression_pending"], 1);
    assert!(tools.memory().next_compression(None).unwrap().is_some());
}

#[test]
fn recall_and_zoom_return_navigable_structured_data() {
    let tools = tools();
    for text in ["café matters", "second original", "third original"] {
        call(&tools, "memory_note", json!({"text": text}));
    }
    let recall = call(&tools, "memory_recall", json!({"pattern": "CAFÉ"}));
    assert_eq!(data(&recall)["result"]["total_matches"], 1);
    assert_eq!(data(&recall)["result"]["truncated"], false);
    assert_eq!(data(&recall)["result"]["memories"][0]["id"], 0);
    validate_result(&tools.tool_definitions()[1], &recall);
    let unbuilt = call(&tools, "memory_zoom", json!({"block": "0-3"}));
    assert_eq!(
        data(&unbuilt)["result"]["children"][0],
        json!({"kind": "summary", "block": "0-1", "text": null})
    );
    validate_result(&tools.tool_definitions()[2], &unbuilt);
    tools
        .memory()
        .submit_summary("0-1".parse().unwrap(), "the first pair")
        .unwrap();
    let settled = call(&tools, "memory_zoom", json!({"block": "0-3"}));
    assert_eq!(
        data(&settled)["result"]["children"][0]["text"],
        "the first pair"
    );
    let leaf = call(&tools, "memory_zoom", json!({"block": "0-1"}));
    assert_eq!(data(&leaf)["result"]["children"][0]["kind"], "raw");
    assert_eq!(
        data(&leaf)["result"]["children"][0]["memory"]["text"],
        "café matters"
    );
    validate_result(&tools.tool_definitions()[2], &leaf);
    let tail = call(&tools, "memory_zoom", json!({"block": "2-3"}));
    assert_eq!(
        data(&tail)["result"]["children"].as_array().unwrap().len(),
        1
    );
    assert_eq!(data(&tail)["result"]["children"][0]["memory"]["id"], 2);
}

#[test]
fn recall_reports_truncation_and_no_matches_without_an_error() {
    let tools = tools();
    for i in 0..100 {
        tools
            .memory()
            .note(day(), &format!("matching fact {i}"))
            .unwrap();
    }
    tools
        .memory()
        .configure(&[(optmem::ConfigKey::PartChars, Some(100))])
        .unwrap();
    let matches = call(&tools, "memory_recall", json!({"pattern": "matching"}));
    assert_eq!(data(&matches)["result"]["total_matches"], 100);
    assert_eq!(data(&matches)["result"]["truncated"], true);
    assert_eq!(
        data(&matches)["result"]["memories"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["id"],
        99
    );
    let empty = call(&tools, "memory_recall", json!({"pattern": "absent"}));
    assert_eq!(empty.is_error, Some(false));
    assert_eq!(data(&empty)["result"]["memories"], json!([]));
}

#[test]
fn malformed_calls_and_unknown_tools_are_protocol_errors() {
    let tools = tools();
    for (name, args) in [
        ("memory_note", json!({})),
        ("memory_note", json!({"text": 42})),
        ("memory_note", json!({"text": "ok", "date": "2025-01-01"})),
        (
            "memory_note",
            json!({"text": "ok", "path": "/someone/else"}),
        ),
        ("memory_recall", json!({"regex": "wrong key"})),
        ("memory_zoom", json!({"block": "5-6"})),
        ("memory_zoom", json!({"block": "0-18446744073709551615"})),
        ("memory_nap", json!({})),
        ("memory_wake", json!({})),
        ("unknown", json!({})),
    ] {
        assert_eq!(
            tools
                .call_tool(request(name, args), day())
                .unwrap_err()
                .code,
            ErrorCode::INVALID_PARAMS
        );
    }
    assert_eq!(
        tools
            .call_tool(CallToolRequestParams::new("memory_note"), day())
            .unwrap_err()
            .code,
        ErrorCode::INVALID_PARAMS
    );
    let continuation = request("memory_note", json!({"text": "not a continuation"}))
        .with_request_state("opaque state");
    assert_eq!(
        tools.call_tool(continuation, day()).unwrap_err().code,
        ErrorCode::INVALID_PARAMS
    );
    assert_eq!(tools.memory().len().unwrap(), 0);
}

#[test]
fn execution_errors_are_model_visible_and_match_the_output_schema() {
    let tools = tools();
    for text in ["".into(), "two\nlines".into(), "ã".repeat(150)] {
        let result = call(&tools, "memory_note", json!({"text": text}));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(data(&result)["status"], "error");
        assert_eq!(data(&result)["error"]["code"], "invalid_memory_operation");
        validate_result(&tools.tool_definitions()[0], &result);
    }
    let bad_regex = call(&tools, "memory_recall", json!({"pattern": "["}));
    assert_eq!(bad_regex.is_error, Some(true));
    validate_result(&tools.tool_definitions()[1], &bad_regex);
    let beyond = call(&tools, "memory_zoom", json!({"block": "0-1"}));
    assert_eq!(beyond.is_error, Some(true));
    validate_result(&tools.tool_definitions()[2], &beyond);
    assert_eq!(tools.memory().len().unwrap(), 0);
}

#[test]
fn policy_is_enforced_when_hidden_tools_are_called_directly() {
    let tools = tools().with_policy(ToolPolicy::read_only());
    assert_eq!(tools.tool_definitions().len(), 2);
    for (name, args) in [
        ("memory_note", json!({"text": "unauthorized"})),
        ("memory_forget", json!({"block": "0-1"})),
    ] {
        let result = call(&tools, name, args);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(data(&result)["error"]["code"], "permission_denied");
    }
    assert_eq!(tools.memory().len().unwrap(), 0);
    assert_eq!(
        call(&tools, "memory_recall", json!({"pattern": "."})).is_error,
        Some(false)
    );
}

#[test]
fn forget_is_opt_in_invalidates_summaries_and_preserves_originals() {
    let tools = tools();
    for text in ["one", "two", "three", "four"] {
        call(&tools, "memory_note", json!({"text": text}));
    }
    while let Some(request) = tools.memory().next_compression(None).unwrap() {
        tools
            .memory()
            .submit_summary(request.block, "settled")
            .unwrap();
    }
    let originals = tools.memory().recall(".").unwrap();
    assert_eq!(
        call(&tools, "memory_forget", json!({"block": "0-1"})).is_error,
        Some(true)
    );
    assert_eq!(tools.memory().pending_count(None).unwrap(), 0);
    let authorized = tools.clone().with_policy(ToolPolicy {
        allow_notes: true,
        allow_forget: true,
    });
    let definition = authorized.tool_definitions().pop().unwrap();
    assert_eq!(definition.name, "memory_forget");
    assert_eq!(
        definition.annotations.as_ref().unwrap().destructive_hint,
        Some(true)
    );
    let result = call(&authorized, "memory_forget", json!({"block": "0-1"}));
    assert_eq!(
        data(&result)["result"]["invalidated_blocks"],
        json!(["0-1", "2-3", "0-3"])
    );
    validate_result(&definition, &result);
    assert_eq!(authorized.memory().recall(".").unwrap(), originals);
    assert_eq!(authorized.memory().pending_count(None).unwrap(), 3);
    let missing = call(&authorized, "memory_forget", json!({"block": "0-1"}));
    assert_eq!(missing.is_error, Some(true));
    validate_result(&definition, &missing);
}

#[test]
fn inputs_reject_additional_fields_in_both_schema_and_dispatch() {
    let tools = tools();
    for tool in tools.tool_definitions() {
        let mut arguments = match tool.name.as_ref() {
            "memory_note" => json!({"text": "fact"}),
            "memory_recall" => json!({"pattern": "."}),
            "memory_zoom" => json!({"block": "0-1"}),
            _ => unreachable!(),
        };
        let schema = Value::Object(tool.input_schema.as_ref().clone());
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&arguments));
        arguments["identity"] = json!("another user's memory");
        assert!(!validator.is_valid(&arguments));
        assert!(
            tools
                .call_tool(request(&tool.name, arguments), day())
                .is_err()
        );
    }
}

struct FailSecondTransaction {
    inner: MemoryStorage,
    calls: AtomicUsize,
}
impl Storage for FailSecondTransaction {
    fn transaction(&self) -> optmem::Result<Box<dyn StorageTransaction + '_>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            return Err(Error::Storage("maintenance unavailable".into()));
        }
        self.inner.transaction()
    }
}

#[test]
fn note_acknowledges_a_saved_append_when_maintenance_check_fails() {
    let memory = Arc::new(OptMem::new(FailSecondTransaction {
        inner: MemoryStorage::new(),
        calls: AtomicUsize::new(0),
    }));
    let tools = MemoryTools::new(memory);
    let result = call(
        &tools,
        "memory_note",
        json!({"text": "saved despite a later error"}),
    );
    assert_eq!(result.is_error, Some(false));
    assert_eq!(data(&result)["result"]["memory"]["id"], 0);
    assert!(data(&result)["result"]["compression_pending"].is_null());
    assert!(
        data(&result)["result"]["maintenance_warning"]
            .as_str()
            .unwrap()
            .contains("Note saved")
    );
    validate_result(&tools.tool_definitions()[0], &result);
    assert_eq!(tools.memory().len().unwrap(), 1);
}

#[test]
fn tools_work_with_dynamic_storage_and_concurrent_bindings() {
    let backend: Box<dyn Storage> = Box::new(MemoryStorage::new());
    let tools = MemoryTools::new(Arc::new(OptMem::new(backend)));
    let handles: Vec<_> = (0..16)
        .map(|i| {
            let tools = tools.clone();
            std::thread::spawn(move || {
                call(
                    &tools,
                    "memory_note",
                    json!({"text": format!("concurrent {i}")}),
                )
            })
        })
        .collect();
    let mut ids: Vec<_> = handles
        .into_iter()
        .map(|h| {
            data(&h.join().unwrap())["result"]["memory"]["id"]
                .as_u64()
                .unwrap()
        })
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, (0..16).collect::<Vec<_>>());
}

#[test]
fn corrupt_summary_exposes_a_recoverable_id_not_a_protocol_error() {
    let temp = tempfile::tempdir().unwrap();
    let memory = Arc::new(OptMem::new(FileStorage::initialize(temp.path()).unwrap()));
    memory.note(day(), "first").unwrap();
    memory.note(day(), "second").unwrap();
    memory
        .submit_summary("0-1".parse().unwrap(), "summary")
        .unwrap();
    let mut corrupt = vec![b' '; 288];
    corrupt[287] = b'\n';
    std::fs::write(temp.path().join("TREE/2"), corrupt).unwrap();
    let tools = MemoryTools::new(memory);
    let result = call(&tools, "memory_zoom", json!({"block": "0-3"}));
    assert_eq!(result.is_error, Some(true));
    assert_eq!(data(&result)["error"]["code"], "corrupt_summary");
    assert_eq!(data(&result)["error"]["recovery_block"], "0-1");
    validate_result(&tools.tool_definitions()[2], &result);
}

#[test]
fn caller_managed_lifecycle_supports_wake_and_summary_submission() {
    let tools = tools().with_caller_managed_lifecycle();
    let definitions = tools.tool_definitions();
    let wake_tool = definitions
        .iter()
        .find(|t| t.name == "memory_wake")
        .unwrap();
    let nap_tool = definitions.iter().find(|t| t.name == "memory_nap").unwrap();
    let empty = call(&tools, "memory_wake", json!({}));
    assert_eq!(data(&empty)["result"]["state"], "ready");
    assert_eq!(data(&empty)["result"]["awake"], true);
    validate_result(wake_tool, &empty);
    for i in 0..4 {
        tools.memory().note(day(), &format!("fact {i}")).unwrap();
    }
    tools
        .memory()
        .configure(&[(optmem::ConfigKey::WakeLines, Some(2))])
        .unwrap();
    let blocked = call(&tools, "memory_wake", json!({}));
    assert_eq!(data(&blocked)["result"]["state"], "needs_compression");
    assert_eq!(data(&blocked)["result"]["request"]["block"], "0-1");
    validate_result(wake_tool, &blocked);
    let task = call(&tools, "memory_nap", json!({}));
    assert_eq!(
        data(&task)["result"]["next_compression"]["sources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    validate_result(nap_tool, &task);
    for block in ["0-1", "2-3", "0-3"] {
        let result = call(
            &tools,
            "memory_nap",
            json!({"block": block, "text": "faithful summary"}),
        );
        assert_eq!(data(&result)["result"]["submission"], "saved");
        validate_result(nap_tool, &result);
    }
    assert!(data(&call(&tools, "memory_nap", json!({})))["result"]["next_compression"].is_null());
    let ready = call(&tools, "memory_wake", json!({}));
    assert_eq!(data(&ready)["result"]["state"], "ready");
    assert_eq!(data(&ready)["result"]["awake"], true);
    validate_result(wake_tool, &ready);
}

#[test]
fn lifecycle_calls_reject_partial_submissions_and_read_only_writes() {
    let tools = tools().with_caller_managed_lifecycle();
    for args in [
        json!({"block": "0-1"}),
        json!({"text": "partial"}),
        json!({"block": "0-1", "text": "ok", "identity": "other"}),
    ] {
        assert!(tools.call_tool(request("memory_nap", args), day()).is_err());
    }
    let nap_tool = tools
        .tool_definitions()
        .into_iter()
        .find(|t| t.name == "memory_nap")
        .unwrap();
    tools.memory().note(day(), "one").unwrap();
    tools.memory().note(day(), "two").unwrap();
    let invalid = call(
        &tools,
        "memory_nap",
        json!({"block": "0-1", "text": "ã".repeat(150)}),
    );
    assert_eq!(invalid.is_error, Some(true));
    validate_result(&nap_tool, &invalid);
    let read_only = tools.with_policy(ToolPolicy::read_only());
    let query = call(&read_only, "memory_nap", json!({}));
    assert_eq!(query.is_error, Some(false));
    let refused = call(
        &read_only,
        "memory_nap",
        json!({"block": "0-1", "text": "unauthorized"}),
    );
    assert_eq!(data(&refused)["error"]["code"], "permission_denied");
    validate_result(&nap_tool, &refused);
    assert_eq!(read_only.memory().pending_count(None).unwrap(), 1);
}

#[test]
fn lifecycle_wake_pagination_has_a_fixed_snapshot_and_explicit_terminator() {
    let tools = tools().with_caller_managed_lifecycle();
    for i in 0..3 {
        tools.memory().note(day(), &format!("fact {i}")).unwrap();
    }
    while let Some(request) = tools.memory().next_compression(None).unwrap() {
        tools
            .memory()
            .submit_summary(request.block, "settled")
            .unwrap();
    }
    tools
        .memory()
        .configure(&[(optmem::ConfigKey::PartLines, Some(1))])
        .unwrap();
    let first = call(&tools, "memory_wake", json!({}));
    assert_eq!(data(&first)["result"]["part"], 1);
    assert_eq!(data(&first)["result"]["snapshot"], 3);
    assert_eq!(data(&first)["result"]["total_parts"], 3);
    assert_eq!(data(&first)["result"]["awake"], false);
    tools.memory().note(day(), "new note mid-read").unwrap();
    let last = call(&tools, "memory_wake", json!({"part": 3, "snapshot": 3}));
    assert_eq!(data(&last)["result"]["awake"], true);
    assert_eq!(data(&last)["result"]["entries"][0]["memory"]["id"], 2);
    let future = call(&tools, "memory_wake", json!({"snapshot": 100}));
    assert_eq!(future.is_error, Some(true));
    let definition = tools
        .tool_definitions()
        .into_iter()
        .find(|t| t.name == "memory_wake")
        .unwrap();
    validate_result(&definition, &future);
}
