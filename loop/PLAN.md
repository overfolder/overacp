# Plan: Build Overloop — Minimal Agent Harness

## Context

Overloop is a ~2k LOC Rust **binary** that runs an agentic loop. It speaks ACP (to the platform), MCP (to tool providers), and OpenAI-compatible chat API (to the LLM). It contains 5 built-in filesystem tools and nothing platform-specific.

This is a fresh repo. The crate lives at the root (`Cargo.toml` + `src/` at repo root). Design docs are in `docs/design/`.

**Constraints:**
- Binary crate at repo root (not a workspace, not a subdirectory)
- Max 500 lines per file — split when approaching limit
- Max 2 path segments in absolute paths (e.g. `crate::llm` ok, `crate::llm::client` not — re-export from `mod.rs` instead)
- Clippy clean on every phase

---

## Quality Gates

### Clippy Configuration (`Cargo.toml`)

```toml
[lints.clippy]
module_inception = "deny"
```

### CI / Pre-commit Checks

```bash
cargo clippy -- -D warnings
```

### File Rules

| Rule | Enforcement |
|------|-------------|
| No file > 500 lines | Review at PR, split proactively |
| Absolute use paths ≤ 2 segments | Re-export from `mod.rs` so consumers write `use crate::llm::SomeType` not `use crate::llm::client::SomeType` |

**How re-exports work:**

```rust
// src/llm/mod.rs — re-export public API
mod client;
mod types;

pub use client::LlmClient;
pub use types::{Message, Role, ToolCall, Usage, StreamEvent, ContentBlock, CompletionResponse, StopReason};
```

Consumers write `use crate::llm::LlmClient` (2 segments). Never `use crate::llm::client::LlmClient` (3 segments).

---

## Phase 1: Overloop Core — Loop + Built-in Tools + LLM Client (~1,400 LOC)

A Rust binary with the agentic loop, 5 built-in tools, and an OpenAI-compatible LLM client. No MCP yet — only built-in tools available.

### Files to Create

| File | LOC | What | Source |
|------|-----|------|--------|
| `Cargo.toml` | ~40 | Binary crate. Deps: `tokio`, `reqwest`, `serde`, `serde_json`, `futures-util`, `uuid`, `tracing`, `tracing-subscriber`, `anyhow`, `chrono`, `walkdir`, `dotenvy` + clippy lints | New |
| `src/main.rs` | ~120 | Entry point: parse config, init ACP transport, init LLM client, notification loop (session/message, session/cancel) | New |
| `src/config.rs` | ~50 | Env vars: `LLM_API_KEY`, `LLM_API_URL` (default OpenRouter), `OVERFOLDER_MODEL`, `OVERFOLDER_WORKSPACE`, `MCP_SERVERS` | New |
| `src/agentic_loop.rs` | ~350 | The agentic loop: build messages → LLM call → dispatch tools → stream → repeat. Max 100 iters, 60min timeout, wind-down, loop_status injection, quota check, silence nudge, poll_new_messages | Simplified from harness |
| `src/acp.rs` | ~150 | Thin ACP client wrapping stdio transport. Methods: `initialize`, `stream_text_delta`, `stream_activity`, `turn_save`, `quota_check`, `quota_update`, `poll_new_messages`, `heartbeat`, `recv_notification` | New |
| `src/llm/mod.rs` | ~15 | Module + re-exports of `LlmClient`, all types | New |
| `src/llm/types.rs` | ~120 | `Role`, `Message`, `ToolCall`, `ContentBlock`, `CompletionResponse`, `StopReason`, `Usage`, `StreamEvent` | Adapted from harness |
| `src/llm/client.rs` | ~250 | `LlmClient` struct. OpenAI-compatible POST /v1/chat/completions with SSE streaming, prompt caching, retry with backoff | Adapted from harness |
| `src/tools/mod.rs` | ~25 | Module + re-exports of `ToolRegistry`, `ToolResult` | New |
| `src/tools/builtin.rs` | ~200 | 5 tools as async functions — direct fs/process: `tool_read`, `tool_write`, `tool_exec`, `tool_glob`, `tool_grep` | New |
| `src/tools/registry.rs` | ~100 | `ToolRegistry` with builtin tools. `definitions()` returns tool schemas. `execute()` dispatches by name | New |

### Key Design Decisions

1. **No `Tool` trait** — tools are plain `async fn(Value) -> ToolResult`. No trait objects for 5 functions.
2. **No `PlatformClient` trait** — `AcpClient` is a concrete struct with the ~8 methods overloop needs.
3. **Synchronous exec** — `tokio::time::timeout(Duration, child.wait_with_output())` replaces pending_job/poll/wait.
4. **`LLM_API_URL` env var** — defaults to OpenRouter now, points to proxy later. Zero code change.
5. **Re-exports enforce path depth** — all `mod.rs` files re-export public API so consumers never reach into submodules.

### What's Dropped vs Harness

- `ComputeProvider` trait + Morph/Local implementations
- `ToolContext` struct
- 34 platform tool files (→ MCP in Phase 3)
- `workspace/` context assembly (system prompt from ACP `initialize` or MCP resources)
- `pending_job` / exec-wait pattern
- Model classifier (proxy decides model)
- Tool middleware (post_process, save_to_secret)
- Compaction (Phase 6, add back simplified)

### Testing

- Unit tests: temp dir, test each built-in tool against real filesystem
- Integration test: mock HTTP server returning canned LLM completions, verify loop dispatches tools correctly

---

## Phase 2: MCP Client Support (~460 LOC)

Overloop connects to MCP servers, discovers tools, routes LLM tool calls.

### Files to Create/Modify

| File | LOC | What |
|------|-----|------|
| `src/mcp/mod.rs` | ~15 | Module + re-exports of `McpClient`, types |
| `src/mcp/client.rs` | ~300 | `McpClient` — Streamable HTTP transport. `connect(url)`, `list_tools()`, `call_tool(name, args)` |
| `src/mcp/types.rs` | ~80 | `McpRequest`, `McpResponse`, `McpToolDef`, `McpToolResult`, `McpContent` |
| `src/tools/registry.rs` | +50 | Add `mcp_tools: HashMap<String, (usize, McpToolDef)>` + `mcp_clients: Vec<McpClient>`. `connect_mcp(url)` discovers tools. `execute()` checks MCP after builtin |
| `src/config.rs` | +20 | Parse `MCP_SERVERS` env var |

### MCP Protocol

Streamable HTTP transport:
- `POST {url}` with `{"jsonrpc": "2.0", "method": "tools/list"}` → tool definitions
- `POST {url}` with `{"jsonrpc": "2.0", "method": "tools/call", "params": {"name": "...", "arguments": {...}}}` → tool result

### Testing

- Mock MCP server (simple Axum handler in tests)
- Integration: overloop + mock LLM + mock MCP → verify tool discovery and routing

---

## Phase 3: Platform MCP Server (~800 LOC)

Expose ~20 platform tools as MCP server. Lives in agent-runner (separate repo — has DB/Redis access).

**Note:** This phase happens in the Overfolder monorepo, not this repo. Listed here for completeness.

### Tools Exposed (~20)

| MCP Tool | Maps to |
|----------|---------|
| `schedule`, `list_schedules` | Platform scheduling |
| `send_to_channel` | Channel routing |
| `spawn_agent`, `check_agent`, `list_agents`, `kill_agent` | Multi-agent |
| `request_permission`, `check_approval`, `list_approvals`, `cancel_approval`, `resume_approval` | Approvals |
| `request_secret`, `store_secret`, `list_secrets` | Secrets |
| `set_language`, `set_timezone` | User config |
| `search_history` | History |
| `report_issue` | Feedback |

---

## Phase 4: Wire Agent-Runner to Spawn Overloop (~100 LOC changes)

**In Overfolder monorepo.** Agent-runner spawns overloop binary instead of harness. Both speak identical ACP — the only change is which binary to spawn and what env vars to pass.

---

## Phase 5: Compaction (~200 LOC)

Add `src/compaction.rs` — simplified version of harness compaction. LLM call to summarize older messages. Triggered at >80% context usage.

---

## Phase 6: Delete Harness (in Overfolder monorepo)

**Prerequisites:** All deployments use overloop for >2 weeks with no regressions.

Remove `harness/`, simplify `standalone/`, clean up workspace.

---

## Summary

| Phase | LOC | Where | Deliverable |
|-------|-----|-------|-------------|
| 1 | ~1,400 | This repo | Binary with 5 tools, working loop |
| 2 | ~460 | This repo | MCP client in overloop |
| 3 | ~800 | Overfolder repo | Platform MCP Server |
| 4 | ~100 | Overfolder repo | Agent-runner spawns overloop |
| 5 | ~200 | This repo | Context compaction |
| 6 | -18,500 | Overfolder repo | Delete harness/ |

---

## File Budget (this repo, after Phase 5)

| File | Est. LOC | Under 500? |
|------|----------|:----------:|
| `src/main.rs` | ~120 | Yes |
| `src/config.rs` | ~70 | Yes |
| `src/agentic_loop.rs` | ~350 | Yes |
| `src/acp.rs` | ~150 | Yes |
| `src/llm/mod.rs` | ~15 | Yes |
| `src/llm/types.rs` | ~120 | Yes |
| `src/llm/client.rs` | ~250 | Yes |
| `src/tools/mod.rs` | ~25 | Yes |
| `src/tools/builtin.rs` | ~200 | Yes |
| `src/tools/registry.rs` | ~150 | Yes |
| `src/mcp/mod.rs` | ~15 | Yes |
| `src/mcp/client.rs` | ~300 | Yes |
| `src/mcp/types.rs` | ~80 | Yes |
| `src/compaction.rs` | ~200 | Yes |
| **Total** | **~2,060** | All under 500 |
