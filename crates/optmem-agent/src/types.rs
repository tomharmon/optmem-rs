//! Typed arguments and structured result payloads. Transport envelopes are rmcp types.

use optmem::{CompressionRequest, Memory, MemoryView, SummaryStatus};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoteParams {
    /// One nonempty line. The configured limit (default 280) is UTF-8 bytes,
    /// not Unicode characters, and is validated when the note is appended.
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecallParams {
    /// Case-insensitive Rust regex over original IDs, dates and text.
    /// Lookaround and backreferences are not supported.
    pub pattern: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlockParams {
    /// Aligned power-of-two range, inclusive at both ends, e.g. "16-31".
    pub block: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WakeParams {
    /// One-based page number; defaults to 1.
    #[serde(default = "first_part")]
    pub part: usize,
    /// Entry count returned by the first page. Reuse it for subsequent pages.
    #[serde(default)]
    pub snapshot: Option<u64>,
}

fn first_part() -> usize {
    1
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NapParams {
    /// Omit both fields to read the next compression request. Otherwise supply
    /// the inclusive block ID from that request together with its summary text.
    #[serde(default)]
    pub block: Option<String>,
    /// One faithful summary line, within the requested UTF-8 byte limit.
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryRecord {
    pub id: u64,
    /// Calendar date formatted as YYYY-MM-DD.
    pub date: String,
    pub text: String,
}

impl From<Memory> for MemoryRecord {
    fn from(memory: Memory) -> Self {
        Self {
            id: memory.id,
            date: memory.date.to_string(),
            text: memory.text,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryNode {
    Raw {
        memory: MemoryRecord,
    },
    Summary {
        /// Inclusive block ID, usable in a subsequent memory_zoom call.
        block: String,
        /// Null means this summary has not been built yet.
        text: Option<String>,
    },
}

impl From<MemoryView> for MemoryNode {
    fn from(view: MemoryView) -> Self {
        match view {
            MemoryView::Raw(memory) => Self::Raw {
                memory: memory.into(),
            },
            MemoryView::Summary { block, text } => Self::Summary {
                block: block.to_string(),
                text,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompressionTask {
    pub block: String,
    pub sources: Vec<MemoryNode>,
    pub max_bytes: usize,
    /// Number of other pending compressions after this one.
    pub remaining: u64,
}

impl From<CompressionRequest> for CompressionTask {
    fn from(request: CompressionRequest) -> Self {
        Self {
            block: request.block.to_string(),
            sources: request.sources.into_iter().map(Into::into).collect(),
            max_bytes: request.max_bytes,
            remaining: request.remaining,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WakeOutput {
    Ready {
        snapshot: u64,
        part: usize,
        total_parts: usize,
        entries: Vec<MemoryNode>,
        /// True only on the final page. Missing summaries never claim awake.
        awake: bool,
        next_compression: Option<CompressionTask>,
    },
    NeedsCompression {
        missing_block: String,
        request: CompressionTask,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionStatus {
    Saved,
    AlreadySettled,
    NothingPending,
}

impl From<SummaryStatus> for SubmissionStatus {
    fn from(status: SummaryStatus) -> Self {
        match status {
            SummaryStatus::Saved => Self::Saved,
            SummaryStatus::AlreadySettled => Self::AlreadySettled,
            SummaryStatus::NothingPending => Self::NothingPending,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NapOutput {
    pub submitted_block: Option<String>,
    pub submission: Option<SubmissionStatus>,
    pub next_compression: Option<CompressionTask>,
    /// A submitted summary remains acknowledged if reading the next request fails.
    pub maintenance_warning: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteOutput {
    /// Acknowledges the persisted append, independently of maintenance status.
    pub memory: MemoryRecord,
    /// Pending compression count, or null if checking it failed after saving.
    pub compression_pending: Option<u64>,
    /// A post-save maintenance check failure does not make the append fail.
    pub maintenance_warning: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecallOutput {
    pub memories: Vec<MemoryRecord>,
    pub total_matches: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZoomOutput {
    pub block: String,
    pub children: Vec<MemoryNode>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ForgetOutput {
    pub requested_block: String,
    /// Summary ranges invalidated by the operation, including dependent suffixes.
    pub invalidated_blocks: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolFailure {
    pub code: String,
    pub message: String,
    /// A corrupt summary's block ID, for operator or authorized tool repair.
    pub recovery_block: Option<String>,
}

/// Both successful and failed execution payloads satisfy the advertised output schema.
/// The enclosing rmcp CallToolResult also sets is_error accordingly.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolResponse<T> {
    Ok { result: T },
    Error { error: ToolFailure },
}
