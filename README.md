# OptMem Rust

A Rust library and `memo` CLI port of [OptMem](https://github.com/VictorTaelin/OptMem).
Raw memories are permanent and append-only. An agent supplies one-line summaries
for an aligned binary tree; recent memories stay detailed and older ones collapse
to fit a reading budget. Nothing calls an LLM or runs in the background.

## How It Works

OptMem separates **what the agent remembers** from **how much it reads at once**.
The full history keeps growing, while the context loaded into a session stays
bounded. Summarization reduces the reading cost, not the stored history.

![OptMem's memory tree growing and compressing over time](https://raw.githubusercontent.com/VictorTaelin/OptMem/main/anim/optmem.gif)

Memory design and animation credit: [Victor Taelin's original OptMem](https://github.com/VictorTaelin/OptMem).

1. **Record lasting facts.** The agent saves short notes about decisions,
   discoveries, and other information worth keeping. Each note gets a stable ID
   in an append-only log; later summaries never replace the original text.
2. **Build a hierarchy of summaries.** Adjacent notes form groups of two, four,
   eight, and so on. For example, `0-1` covers two notes, while `0-3` covers four.
   Small groups are summarized from raw notes; larger groups use their two child
   summaries. This forms a binary tree over the history.
3. **Load a bounded view.** At session startup, `wake` selects a mix of raw notes
   and summaries covering the whole history without overlapping. Recent memories
   get more detail; older ranges get broader summaries. The reading budget changes
   this view without deleting notes or rebuilding existing summaries.
4. **Recover detail when needed.** `recall` searches the original notes with a
   regular expression. `zoom` opens a summary into its two halves; repeated zooms
   lead back to the raw notes. A summary is an index into history, not its only
   surviving copy.

The agent supplies the meaning: OptMem identifies pending summary work and stores
the result, but does not generate summaries itself. With the CLI and MCP server,
the calling agent uses its current context to write them through `nap`. Automatic
harness maintenance is planned, not implemented. `forget` is an optional repair
operation for bad summaries; it leaves the original notes intact so the affected
summaries can be rebuilt.

The Rust library keeps this algorithm independent of storage. The CLI uses files,
but harnesses can provide an in-memory, database, or remote backend through the
storage traits.

## CLI

### Installation

With a current stable Rust toolchain installed, run from this repository's root:

```sh
cargo install --path . --locked
```

This installs the `memo` binary into Cargo's binary directory, normally
`~/.cargo/bin`. Make sure that directory is on your PATH. No Python runtime or
model API key is required. To update, pull the latest source and run the same
installation command again.

### Usage

Initialize your memory, then start recording and reading notes:

```sh
memo init
memo note "The project uses Rust and storage must be replaceable"
memo wake
```

`init` creates the identity and user config, then prints a static integration block
to put in your agent's instructions. It is idempotent. Memory operations refuse to
create a missing identity; `config` can inspect/edit settings before initialization.
Instructions use plain `memo`, assuming the Rust binary is on PATH. They contain
no executable, storage-directory, or byte-limit placeholders. If a command used an
explicit `--config` or directory override, its follow-up retains those flags so it
cannot accidentally switch identity.

| Command | Purpose |
| --- | --- |
| `memo init` | Explicitly create/open the store and print agent instructions |
| `memo wake [part [T]]` | Read the bounded context, using snapshot `T` for subsequent pages |
| `memo note "..."` | Append one line, at most 280 UTF-8 bytes by default |
| `memo nap` | Show the next pending compression |
| `memo nap 0-1 "summary"` | Submit it; summaries must be built in the offered order |
| `memo recall 'regex'` | Search all raw lines, keeping newest matches within the output budget |
| `memo zoom 16-31` | Open a tree node into its two halves |
| `memo forget 16-31` | Drop a summary, later summaries at that level, and dependent ancestors |
| `memo config [NAME=VALUE ...]` | Show/edit user settings, including `storage_dir`; `NAME=` resets |
| `memo import FILE` | Append dated `YYYY-MM-DD <text>` lines, validated before writing |

Sizes: `WAKE_LINES=96`, `ENTRY_CHARS=280`, `PART_CHARS=20000`, `PART_LINES=500`.
Changing the reading budget does not recompute stored summaries. All sizes must
be positive; `ENTRY_CHARS` cannot exceed 280. A cover needs at least the number
of set bits in its snapshot count; an impossibly small `WAKE_LINES` is reported
rather than silently exceeding the budget. Paging preserves whole lines, so a
single line may exceed an artificially tiny `PART_CHARS` setting.

`recall` uses Rust's `regex` syntax, not Python's: lookaround and backreferences
are not supported. IDs and dates are searched along with the text. `note` dates
use the local system date; the library accepts dates explicitly. Import dates
must be real calendar dates in nondecreasing order, including against the last
stored memory.

## Configuration

Both `memo` and `optmem-mcp` read the same TOML user config:
`$XDG_CONFIG_HOME/optmem/config.toml`, defaulting to `~/.config/optmem/config.toml`.
New memory data defaults to `$XDG_DATA_HOME/optmem/memory`, or
`~/.local/share/optmem/memory`. Relative XDG environment paths are ignored. Config
and data are separate; new stores do not get a per-store config file.

An example config:

```toml
storage_dir = "~/.local/share/optmem/memory"
entry_chars = 280
wake_lines = 96
part_chars = 20000
part_lines = 500
```

`memo init` (or MCP `--init`) persists the chosen storage directory and sizes.
Inspect or edit them without editing memory data:

```sh
memo config
memo config storage_dir=~/my-memory entry_chars=200
memo config wake_lines=300
memo config entry_chars=
```

An empty size value restores the tool default. Lower-case TOML names are used in
the config file; legacy upper-case command assignments such as `ENTRY_CHARS=200`
are also accepted. Edits are validated as a batch, locked across processes, and
written atomically. Reading or editing config does not create a memory identity.
After selecting a missing directory, initialize it explicitly.

`--config PATH` overrides `OPTMEM_CONFIG` and XDG discovery. For storage selection,
`--memory-dir PATH` overrides `MEMORY_DIR`, which overrides `storage_dir` in the
config. A directory selected on `init` becomes the stored default; on other
commands it is a temporary override. `~` expands to HOME; relative paths manually
written in TOML resolve relative to the config's directory, not the working
directory. CLI storage assignments resolve to absolute paths when written.
The CLI reloads settings each command; restart a long-running MCP server after
editing its settings.

With no user config and no explicit XDG data-home override, an existing
`~/.optmem/memory/LOG.txt` is recognized and retained as the identity. Its legacy
sizes are copied into the user config on initialization, leaving the log, tree,
and old config untouched. Once the user config exists it is authoritative; an
omitted/reset setting uses today's tool default, not an old per-store override.
If both legacy and XDG identities exist without an explicit selection, the tool
refuses to choose. No automatic move or deletion is performed.

## Library

```rust
use chrono::NaiveDate;
use optmem::{OptMem, Wake, storage::MemoryStorage};

fn main() -> optmem::Result<()> {
    let memory = OptMem::new(MemoryStorage::new());
    let date = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
    memory.note(date, "Storage does not need to be files")?;
    memory.note(date, "Summaries are supplied by the caller")?;

    if let Some(request) = memory.next_compression(None)? {
        // Use request.sources to produce a faithful summary.
        memory.submit_summary(request.block, "Caller-supplied summaries; replaceable storage")?;
    }
    match memory.wake(1, None)? {
        Wake::Ready(page) => {
            for entry in page.entries { println!("{entry}"); }
        }
        Wake::NeedsCompression { request, .. } => println!("Compress {} first", request.block),
    }
    Ok(())
}
```

`OptMem<S: Storage>` has no path assumptions or filesystem calls. Results are
structured: raw memories, summary nodes, compression requests, wake pages,
recall results, and typed errors. `Block::new(lo, hi)` uses half-open bounds;
parsing/displaying CLI IDs uses inclusive bounds (`16-31` means `[16,32)`).
Use `OptMem::with_config(storage, config)` to supply validated settings explicitly,
without reading or writing backend configuration. This also works with in-memory
and database backends and never consults HOME. `OptMem::new(storage)` retains
backend-local config behavior for compatibility; `configure` applies only in that
mode. User config helpers are separate in `optmem::settings::AppSettings`.
`AppSettings::config()` supplies user sizes without looking at a storage directory,
so database and in-memory integrations can use the same settings as file backends.

## Storage

- `FileStorage::initialize(path)` explicitly creates an identity.
- `FileStorage::open(path)` opens an existing identity.
- `MemoryStorage::new()` is thread-safe, isolated and entirely in-memory.
- Implement `Storage` and `StorageTransaction` for a database, remote service,
  or another backend. Both traits are object-safe; `Box<dyn Storage>` and
  `Arc<dyn Storage>` work with the engine.

The backend supplies an exclusive transaction guard for a consistent view of
entries, summary levels and configuration. Each raw ID is its immutable position;
each summary level is a dense prefix of blocks in order. The interface exposes
range reads, append-only writes, summary suffix truncation, and configuration.
Guard lifetime serializes operations; it is **not** a rollback journal. Successful
writes must be durable before returning, and an I/O failure may leave earlier
completed writes. Backend implementers must preserve these contracts and serialize
all handles to the same identity, including separate processes when applicable.
See the trait documentation in `src/storage/mod.rs` for the complete contract.

The file backend retains the original `LOG.txt` (320-byte records), `TREE/<size>`
(288-byte records), and support for reading/writing the legacy `config` format.
Existing memory files need no migration. The CLI and server use the XDG user
config instead; initializing a new store does not write local size settings.
It uses the original `.lock` advisory lock, synchronizes writes, repairs incomplete
trailing records before append, and reports corrupt summaries with a recoverable
block ID. Forgetting summaries never changes the raw log. Readers also lock, so concurrent Rust operations cannot
see a partially updated tree; long recalls serialize writers. The original Python
tool does not lock its reads, so this reader guarantee applies to Rust clients.

## Agent Tools

The workspace's `optmem-agent` crate exposes `rmcp::model::Tool`,
`CallToolRequestParams`, `CallToolResult`, and `ErrorData` directly. It re-exports
`rmcp` so adapters can use the same SDK version. There is no separate custom
tool-definition format, registry framework, MCP transport, or provider client.
The core `optmem` crate and `memo` CLI do not depend on rmcp.

```rust
use std::sync::Arc;
use chrono::NaiveDate;
use optmem::{OptMem, storage::MemoryStorage};
use optmem_agent::{MemoryTools, CallToolRequestParams};

let memory = Arc::new(OptMem::new(MemoryStorage::new()));
let tools = MemoryTools::new(memory.clone());
let definitions = tools.tool_definitions(); // Vec<rmcp::model::Tool>
let arguments = serde_json::json!({"text": "Keep tool bindings separate from storage"});
let request = CallToolRequestParams::new("memory_note")
    .with_arguments(arguments.as_object().unwrap().clone());
let result = tools.call_tool(
    request,
    NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
)?; // rmcp::model::CallToolResult
```

The default tools are `memory_note`, `memory_recall`, and `memory_zoom`.
`ToolPolicy::read_only()` disables notes; setting `allow_forget` explicitly
registers `memory_forget`. Policy is enforced at dispatch even if a caller names
a hidden tool. Each binding targets one identity; model arguments cannot select
paths, storage backends, other users, or note dates. Schemas are generated from
Serde/Schemars argument and response types; arguments reject unknown fields.
MCP annotations describe effects but are not used as authorization.

Successful responses include typed data under `{"status":"ok","result":...}`.
Execution failures use `{"status":"error","error":...}` and `is_error=true`.
Both satisfy the tool's output schema. Malformed arguments and unknown tools
return protocol errors instead. rmcp provides structured content plus a JSON text
fallback; adapters should choose one representation, not insert both into context.

The tool handler is synchronous and may block on storage. Async harnesses and an
MCP server must offload it to their blocking executor. A successful note includes
a pending-compression count; failure to check that count returns a warning in the
successful receipt, rather than implying the note should be appended again.

**Automatic harness lifecycle integration is not implemented yet.** A harness
adapter will record tool results, capture the main agent's complete current
context, and run compression maintenance before the next main-agent continuation.
The shared engine is available via `MemoryTools::memory()` for those hooks.
By default, context preparation handles `wake` and maintenance handles `nap`.
`with_caller_managed_lifecycle()` adds `memory_wake` and `memory_nap` for callers
that manage that loop themselves, including the MCP server below.

## MCP Server

`optmem-mcp` is a standalone stdio server, plus a reusable `OptMemServer<S>` for
embedding with any storage backend. It uses rmcp for initialization, discovery,
tool calls, protocol negotiation, and transport. Storage calls run on blocking
workers, not the async event loop. Stdout contains only protocol messages;
startup errors go to stderr. No model API key, Python runtime, or HTTP port is
needed. The MCP client starts and owns the process.

Install from the repository:

```sh
cargo install --path crates/optmem-mcp
```

Launch it through your MCP client with command `optmem-mcp`. It reads storage_dir
and size limits from the shared XDG user config. `--config`/`OPTMEM_CONFIG` selects
another config; `MEMORY_DIR` or `--memory-dir PATH` temporarily selects another store.
A missing identity is an error, not an empty memory. Initialize it using `memo init`,
or explicitly give the server `--init` (safe to repeat, preserves existing
data). Use an absolute executable path when the client does not inherit your PATH.

A typical stdio client configuration (the surrounding format is client-specific):

```json
{
  "mcpServers": {
    "optmem": {
      "command": "optmem-mcp",
      "args": ["--init"]
    }
  }
}
```

For local development, build with `cargo build -p optmem-mcp` and point the client
at `target/debug/optmem-mcp`. Running `cargo run -p optmem-mcp -- --init` also starts
the stdio server, but it waits for MCP input rather than opening a web UI.

The default catalog has five tools: `memory_note`, `memory_recall`, `memory_zoom`,
`memory_wake`, and `memory_nap`. `--allow-forget` adds `memory_forget`.
`--read-only` hides notes/forget and rejects note appends and summary submissions,
including direct calls to hidden tools; wake, recall, zoom, and nap queries remain
available. `--read-only` and `--allow-forget` cannot be combined. Initialization
advertises instructions for the complete memory lifecycle.

**Compression uses the connected agent's full context, not a separate source-only
model.** MCP does not automatically provide the server with the caller's complete
conversation. This server therefore uses caller-managed compression instead of
claiming to inherit unavailable context or relying on client sampling support:

1. Call `memory_wake` with `{}` at startup or after compaction. For subsequent
   pages, supply its `snapshot` and the next `part`; finish only when `awake=true`.
2. If wake returns `state="needs_compression"`, summarize the supplied request's
   sources using the calling agent's current context, submit via `memory_nap`,
   then retry wake. This state is a successful control response, not an empty
   or completed memory context.
3. After a note reports pending work, call `memory_nap` with `{}`. Submit with
   `{"block":"0-1","text":"faithful summary"}` and follow `next_compression`
   until it is null. Preserve lasting facts and corrections; invent nothing.

Both summary fields are required together. Requests are built in order, validated
against the configured byte limit, and cannot overwrite a settled summary.
After a concurrent forget/rebuild, discard old compression requests and query nap
again: block ordering is checked, but requests are not generation-versioned yet.
If a summary was accepted but fetching the next request fails, its receipt retains
the submission status and includes `maintenance_warning`; do not mistake that for
a failed save. Read-only connections needing compression must ask an authorized
operator. In-process harness adapters can later automate this loop after their
tool-turn hooks capture the main agent's full context. No background compression
or server-side summarizer is enabled by this server.

## Verification

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Tests cover both backends, block geometry, a synthetic history, summary ordering,
forget/rebuild, pagination snapshots, bounded searches, UTF-8, invalid imports,
process concurrency, idempotent initialization, and interrupted/corrupt records.
Agent-tool tests also validate MCP serialization, generated input/output schemas,
permission enforcement, recoverable errors, and successful post-save receipts.
Settings tests cover XDG paths, plain PATH-based commands, user-config precedence,
byte limits shared by both binaries, atomic edits, and legacy identity adoption.
MCP tests use a real rmcp client over duplex streams and launch the actual stdio
binary to verify initialization, shared handlers, restart persistence, permissions,
protocol errors, and shutdown on client disconnect.
Optional differential tests check the original implementation's cover math and
read/write interoperability:

```sh
OPTMEM_PYTHON=/path/to/OptMem/memo cargo test --test python_compat -- --ignored
```

This port is based on the Python implementation at upstream commit `1fb164c`.
