//! Shared rmcp tools for in-process harness adapters and an MCP server.
//!
//! This crate uses MCP's definitions, requests and results without starting a
//! server or choosing an LLM provider. The engine remains in `optmem`.
//!
//! ```
//! use std::sync::Arc;
//! use chrono::NaiveDate;
//! use optmem::{OptMem, storage::MemoryStorage};
//! use optmem_agent::{MemoryTools, CallToolRequestParams};
//! let memory = Arc::new(OptMem::new(MemoryStorage::new()));
//! let tools = MemoryTools::new(memory);
//! let args = serde_json::json!({"text": "Prefer explicit storage boundaries"});
//! let request = CallToolRequestParams::new("memory_note")
//!     .with_arguments(args.as_object().unwrap().clone());
//! let date = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
//! let result = tools.call_tool(request, date)?;
//! assert_eq!(result.is_error, Some(false));
//! assert_eq!(tools.tool_definitions().len(), 3);
//! # Ok::<(), optmem_agent::ErrorData>(())
//! ```

mod tools;
pub mod types;

pub use rmcp;
pub use rmcp::model::{CallToolRequestParams, CallToolResult, ErrorData, Tool, ToolAnnotations};
pub use tools::{MemoryTools, ToolPolicy};
