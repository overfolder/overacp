# Context management

How over/ACP separates the agent's working context from the
operator's canonical conversation record.

## Principle

The agent owns its **working context** — the message array it feeds
the LLM. The operator owns the **canonical record** — the
authoritative history of what was actually said.

The boundary between them is the protocol:

- `initialize` (operator → agent): the operator seeds the working
  context with whatever history window it chooses.
- `stream/*` (agent → operator): the operator observes every turn in
  real time.
- `context/compacted` (agent → operator): the agent tells the
  operator "I compacted; here is the surviving canonical history and
  a summary of what was dropped — please persist."
- `turn/end` (agent → operator): a lightweight turn-complete signal
  carrying `usage` only (the `messages` field is deprecated).

The agent is free to inject any scaffolding it wants into its working
context — compaction summaries, turn markers, wind-down warnings,
silence nudges — without those synthetic messages reaching the
operator's store.

## Auto-compaction

overloop monitors estimated token usage on every loop iteration. When
the working context exceeds a configurable threshold of the context
window, the agent compacts:

1. **Split** — keep the system prompt and the last N messages
   (default: 10). Everything in between is the compaction target.
2. **Summarize** — an LLM call reduces the compaction target to a
   prose summary preserving decisions, file changes, findings, task
   state, and commitments.
3. **Rebuild** — the working context becomes:
   `[system_prompt, <compacted_context>summary</compacted_context>, ...recent]`.
   The `<compacted_context>` message is `Role::System` — it tells the
   LLM that prior context was summarized.
4. **Notify** — the agent emits `context/compacted` carrying the
   summary and the canonical (non-synthetic) recent messages so the
   operator can update its store.
5. **Cap** — a per-session counter limits compaction rounds (default:
   3). After the cap, the agent stops compacting and lets the LLM hit
   a natural length limit.

### Configuration

All thresholds are environment variables with sensible defaults:

| Env var                  | Default   | Description |
|--------------------------|-----------|-------------|
| `CONTEXT_WINDOW`         | `128000`  | Total context window in tokens |
| `COMPACTION_THRESHOLD`   | `0.80`    | Fraction of window that triggers compaction |
| `COMPACTION_KEEP_RECENT` | `10`      | Messages preserved during compaction |
| `MAX_COMPACTIONS`        | `3`       | Max compaction rounds per session |

### Token estimation

Token counts are estimated locally using a `chars / 4` heuristic for
text and a flat 765 tokens per image block. No external tokenizer
dependency.

## `context/compacted` notification

```jsonc
{
  "jsonrpc": "2.0",
  "method": "context/compacted",
  "params": {
    "summary": "User asked to refactor auth middleware. We identified ...",
    "messages": [
      { "role": "user", "content": "now deploy it" },
      { "role": "assistant", "content": "Deploying to staging..." }
    ],
    "usage": { "input_tokens": 1200, "output_tokens": 400 }
  }
}
```

The operator SHOULD:
- Replace its stored message history for this conversation with
  `messages`.
- Store `summary` as the compaction prefix so that a future
  `BootProvider::initialize` can return both the summary and the
  surviving messages.

## `turn/end` (updated)

`turn/end` is now a lightweight completion signal:

```jsonc
{
  "jsonrpc": "2.0",
  "method": "turn/end",
  "params": {
    "usage": { "input_tokens": 5000, "output_tokens": 1200 }
  }
}
```

The `messages` field is deprecated and omitted by conforming agents.
Operators that need per-turn message persistence should reconstruct
from `stream/*` notifications or wait for `context/compacted`.

## Interaction with `initialize`

On cold start, `BootProvider::initialize` returns the system prompt
and a curated history window. The agent loads this into its working
context and immediately layers on any scaffolding it needs — turn
markers, nudges, etc. Those injections are ephemeral; they exist
only in the agent's in-memory message array.

If the operator previously received a `context/compacted`
notification, it can include the `summary` as a system message at
the front of the returned history so the agent starts with prior
context already folded in.

## Agent-internal scaffolding

The following are injected into the working context but never flow
to the operator:

- **`<compacted_context>` system message** — the LLM-generated
  summary of dropped history, wrapped in a sentinel tag.
- **Wind-down warning** — a system message injected when the
  iteration budget is nearly exhausted.
- **Silence nudge** — injected when the LLM produces no output for
  several iterations.
- **Turn markers** (future) — a `<turn>` system message injected
  before the current user input to delineate prior history from the
  current request. Not yet implemented; can be added as a follow-up
  without protocol changes.

## Optional memory-flush hook (planned)

Before summarizing, the compaction path can check for an
operator-provided `memory_flush` tool. If registered, the agent
invokes it with the to-compact messages so the operator can extract
long-term facts (user preferences, architectural decisions, etc.)
to durable storage before the messages are summarized away.

This is not yet implemented; the hook point is marked with a TODO
in `loop/src/compaction.rs`.
