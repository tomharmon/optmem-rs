## Memory

Your memory is OptMem:
- The tool is `memo`, installed on PATH.
- `memo config` shows the configured storage directory and size limits.
- Use the same HOME/XDG_CONFIG_HOME/OPTMEM_CONFIG environment for every command.
- MEMORY_DIR, when set, overrides the configured storage directory.

OptMem outlives every session, compaction, model and vendor change.
Without it you do not know who you are, or what was decided and tried.

### At startup: activating OptMem (mandatory)

Run `memo wake` before any other tool call, in every session, and
then do exactly what it prints, to the end of its output.

### While working: register memories (mandatory)

Call `memo note "<one line>"` whenever you learn
something new, or something worth keeping happens. That covers a task
worth real effort, a fact or insight the user teaches you, anything you
learn about their life (even indirectly), any event of lasting effect.

Do not register redundant memories.

Respect the configured entry_chars byte limit; `memo` validates every note.

If `memo note` asks a compression: do it before your next action.

Never edit or delete memory data directly: the tool manages its configured store.

### When you need an old memory: search, or navigate

`memo recall <regex>` searches every memory, word for word.

Your memories also form a binary tree: #0-1, #2-3 ... exist as one-line
summaries, pairs of those as #0-3, and so on -- every `#a-b` line wake
prints is one node of it. `memo zoom <a-b>` opens a node into its
two halves, down to the raw memories.

### If you're a subagent: skip everything above

Parallel sessions on this machine are all you, and may all write memories.
A subagent is not: it must never run `memo`, because it cannot judge what
is already known, and its notes would arrive duplicated and incorrectly.
When you spawn one, write: `You are a subagent. Don't run memo.`
