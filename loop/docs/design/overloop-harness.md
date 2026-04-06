# Overloop — Minimal Agent Harness

**Status:** Design (not started)
**Date:** 2026-03-31
**Crate:** `overloop/` (to be extracted to `overfolder/overloop` repo later)
**Related:** [platform-not-harness.md](platform-not-harness.md), [acp-node-protocol.md](acp-node-protocol.md), [overfolder-jsonrpc-api.md](overfolder-jsonrpc-api.md)
**Context:** [composio-research.md](../references/composio-research.md) (meta-tool pattern)

---

## What Is Overloop

A ~2k line Rust binary that runs an agentic loop. Nothing else.

It speaks three protocols:
- **ACP** (to the platform) — lifecycle, streaming, turn persistence
- **MCP** (to tool providers) — tool discovery and execution
- **OpenAI-compatible chat API** (to the LLM proxy) — model calls

It does NOT contain tools, LLM provider abstractions, compute abstractions, context management, scheduling, approval state machines, connector code, or anything platform-specific.

---

## Principles

1. **The loop is the only job.** Call LLM, dispatch tools, stream results, repeat.
2. **Tools come from outside.** MCP servers announce tools. Overloop discovers and calls them. Zero built-in tools that require platform knowledge.
3. **The model is someone else's problem.** Overloop sends `POST /v1/chat/completions` to a URL. The proxy decides which model, enforces quota, meters cost.
4. **The platform is someone else's problem.** Overloop streams via ACP. The platform decides where those tokens go (Telegram, web, nowhere).
5. **Runs anywhere.** Static Rust binary, ~5MB. In a Morph VM, in a Docker container, on a laptop. No runtime dependencies.

---

## Tool Architecture: Built-in vs MCP

### What's built-in (~5 tools, ~200 LOC)

These are **VM-local filesystem operations** that must be fast (no network round-trip). They're trivially thin — just `std::fs` and `tokio::process::Command`:

| Tool | What it does | Why built-in |
|------|-------------|--------------|
| `read` | `fs::read_to_string(path)` | Latency — called 10-50x per turn |
| `write` | `fs::write(path, content)` | Latency — called frequently |
| `exec` | `Command::new("bash").arg("-c").arg(cmd)` | Latency + streaming stdout |
| `glob` | Walk directory with glob pattern | Latency |
| `grep` | `Command::new("rg")` or manual search | Latency |

These are **NOT** the 200-line tool implementations from `harness/`. They're 20-30 lines each — no compute abstraction, no Morph HTTP, no bwrap. Just direct syscalls. They run in the VM; the VM IS the sandbox.

**Edit is intentionally absent** — the LLM can `write` the whole file or use `exec` with `sed`/`patch`. Edit is a UX optimization that belongs in the system prompt, not the harness. If we ever want it, it's an MCP tool.

### What comes from MCP (everything else)

The platform exposes an MCP server. Overloop connects at startup, calls `tools/list`, and adds discovered tools to the LLM context alongside the built-in 5.

```
Overloop startup:
  1. Connect to MCP server(s) from config
  2. tools/list → get tool definitions (name, description, inputSchema)
  3. Merge with built-in tools
  4. Send combined tool list to LLM on every call
```

When the LLM calls an MCP tool:

```
LLM returns: tool_call { name: "schedule", input: { cron: "0 9 * * *", ... } }

Overloop checks:
  - Is "schedule" a built-in? No.
  - Is "schedule" from MCP server X? Yes.
  → MCP call: tools/call { name: "schedule", arguments: { ... } }
  ← MCP response: { content: [{ type: "text", text: "Scheduled daily-briefing..." }] }
  → Add tool result to messages, continue loop.
```

### MCP Server Topology

```
┌─────────────────────────────────────────────────────────┐
│  Overloop (in VM)                                       │
│                                                         │
│  Built-in: read, write, exec, glob, grep                │
│                                                         │
│  MCP Client ──┬──► Platform MCP Server                  │
│               │    (schedule, approve, message,          │
│               │     connectors, memory, user config,     │
│               │     history, agents, secrets)            │
│               │                                         │
│               ├──► Composio MCP Server (optional)        │
│               │    (850+ app integrations,               │
│               │     meta-tools: search, schema, execute) │
│               │                                         │
│               └──► User MCP Servers (optional)           │
│                    (custom tools, local services)        │
└─────────────────────────────────────────────────────────┘
```

Multiple MCP servers, each providing a set of tools. Overloop merges them all.

### Tool Tiering (Carried Forward)

The current harness has a smart pattern: tier-1 tools always in LLM context, tier-2 tools only after discovery. This matters because sending 50+ tool definitions to the LLM wastes context and degrades tool selection.

In Overloop, tiering works differently:

**Built-in tools (5):** Always in context. These are the core workspace tools.

**MCP tools:** The MCP server controls what it exposes. Two approaches:

**Approach A — Platform controls tiers:** The platform MCP server only announces essential tools on `tools/list` (schedule, message, etc.). Advanced tools (connectors, history search) are behind a `discover_tools` meta-tool that the LLM calls when needed.

**Approach B — Overloop tiering config:** Overloop config declares which MCP tools are tier-1 vs tier-2. Tier-2 tools are executable but not in the LLM context until called by name or discovered.

**Recommended: Approach A.** The MCP server is smarter about what the LLM needs. Overloop stays dumb. The `discover_tools` pattern from Composio maps directly:

```
MCP Platform Server exposes:
  Tier 1 (always): schedule, approve, message, set_timezone, set_language
  Meta-tool: discover_tools → search across all available platform + Composio tools

When LLM calls discover_tools("send email"):
  → Platform searches its own tools + Composio
  → Returns: gmail_send_email (schema + auth status)
  → LLM now has the tool definition, calls it directly next turn
```

**Meta-tools do NOT live in the harness.** They're MCP tools provided by the platform server. The harness doesn't know about Composio, tool discovery semantics, or auth flows. It just calls whatever tools MCP gives it.

---

## MCP Support in the Harness

### MCP Client Implementation

Overloop embeds a minimal MCP client (~400 LOC). Two transport options:

**Option 1 — stdio (spawn subprocess):**
```
Overloop spawns MCP server as child process
  stdin/stdout = MCP JSON-RPC
```
Standard MCP pattern. Works for local MCP servers. The platform MCP server would need to be a binary in the VM that proxies to agent-runner over HTTP.

**Option 2 — Streamable HTTP (connect to URL):**
```
Overloop connects to https://mcp.internal/platform
  POST /mcp → JSON-RPC requests
  GET /mcp (SSE) → notifications
```
Newer MCP transport. The platform MCP server runs as an HTTP endpoint on agent-runner (or a sidecar). No subprocess needed.

**Recommended: Streamable HTTP.** Reasons:
- Platform MCP server is remote (on agent-runner, not in the VM)
- No need to bundle MCP server binaries in the VM image
- Composio MCP is already HTTP (`mcp.composio.dev`)
- stdio still supported for user-provided local MCP servers

### MCP Capabilities Used

| MCP Feature | Used | How |
|-------------|:----:|-----|
| `tools/list` | Yes | Discover platform + connector tools at startup |
| `tools/call` | Yes | Execute any MCP tool when LLM requests it |
| `resources/list` | Maybe | Read workspace config, memory (alternative to built-in `read`) |
| `resources/read` | Maybe | Load AGENT.md, USER.md, MEMORY.md from platform |
| `prompts/list` | No | System prompt comes from ACP `initialize` or workspace files |
| `sampling` | No | Overloop owns LLM calls, not the MCP server |
| `notifications` | Maybe | Tool list changes, resource updates |

### MCP Tool Execution Flow

```
1. LLM returns tool_call: { name: "schedule", arguments: {...} }

2. Overloop dispatches:
   if name in built_in_tools:
     result = built_in_tools[name].execute(arguments)
   else if name in mcp_tools:
     server = mcp_tools[name].server  // which MCP server owns this tool
     result = server.call_tool(name, arguments).await
   else:
     result = error("Unknown tool")

3. Add result to messages, continue loop
```

---

## Crate Structure

```
overloop/
├── Cargo.toml
├── src/
│   ├── main.rs              # Entry point: parse args, connect, run loop
│   ├── config.rs             # Env vars, MCP server URLs, LLM endpoint
│   │
│   ├── loop.rs               # The agentic loop (~300 lines)
│   │                         # call_llm → dispatch_tool → stream → repeat
│   │
│   ├── llm/
│   │   ├── mod.rs
│   │   ├── client.rs         # OpenAI-compatible streaming HTTP client (~200 lines)
│   │   └── types.rs          # ChatMessage, ToolCall, Usage (~100 lines)
│   │
│   ├── tools/
│   │   ├── mod.rs
│   │   ├── builtin.rs        # read, write, exec, glob, grep (~200 lines)
│   │   ├── mcp.rs            # MCP client: connect, discover, call (~400 lines)
│   │   └── registry.rs       # Unified registry: built-in + MCP (~150 lines)
│   │
│   └── acp/
│       ├── mod.rs
│       ├── transport.rs      # JSON-RPC over stdio or WebSocket (~200 lines)
│       └── client.rs         # stream_text, stream_tool, turn_save, etc. (~200 lines)
│
└── tests/
    ├── loop_test.rs          # Mock LLM + mock MCP → verify loop behavior
    └── mcp_test.rs           # MCP client integration tests
```

### Dependencies

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json", "stream"] }  # LLM + MCP HTTP
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
glob = "0.3"                    # For built-in glob tool
futures-util = "0.3"            # Stream processing for SSE
```

No `sqlx`, no `redis`, no `axum`, no `parking_lot`. Just HTTP + JSON + async.

### Size Budget

| Component | Est. LOC |
|-----------|----------|
| `main.rs` + `config.rs` | ~200 |
| `loop.rs` | ~300 |
| `llm/` | ~300 |
| `tools/builtin.rs` | ~200 |
| `tools/mcp.rs` | ~400 |
| `tools/registry.rs` | ~150 |
| `acp/` | ~400 |
| **Total** | **~1,950** |

---

## What Happens to Current Tools

The 39 built-in tools in `harness/src/tools/builtin/` split into three categories:

### Stay as built-in in Overloop (5)

| Tool | Current LOC | Overloop LOC | Why built-in |
|------|-----------|-------------|-------------|
| `read` | 89 | ~25 | Latency — called constantly |
| `write` | 104 | ~20 | Latency |
| `exec` (+wait/abort) | 298 | ~80 | Latency + stdout streaming |
| `glob` | 82 | ~30 | Latency |
| `grep` | 100 | ~30 | Latency |

### Move to Platform MCP Server (~20)

| Tool | Why MCP |
|------|---------|
| `schedule`, `list_schedules` | Platform owns scheduling |
| `send_to_channel`, `message` | Platform owns channel routing |
| `spawn_agent`, `check_agent`, `list_agents`, `kill_agent`, `steer` | Platform owns multi-agent |
| `request_permission`, `check_approval`, `list_approvals`, `cancel_approval`, `resume_approval` | Platform owns approvals |
| `request_secret`, `check_secret_request`, `cancel_secret_request`, `store_secret`, `list_secrets`, `delete_secret` | Platform owns secrets |
| `set_language`, `set_timezone` | Platform owns user config |
| `report_issue`, `list_issues` | Platform owns feedback |
| `search_history`, `get_history_range`, `conversation_stats` | Platform owns history |

### Move to system prompt / drop (5)

| Tool | Disposition |
|------|------------|
| `view_file` | Drop — `read` with line range param covers it |
| `edit` | System prompt instructs LLM to use `write` or `exec sed` |
| `web_fetch` | MCP tool (platform or standalone MCP server) |
| `web_search` | MCP tool (platform or Brave MCP server) |
| `curl` | MCP tool (platform, with auth injection) |
| `compact_context`, `context_info` | ACP methods or MCP resources, not tools |

---

## Comparison: Current Harness vs Overloop

| Aspect | Current Harness | Overloop |
|--------|----------------|----------|
| LOC | ~18,500 (harness + acp + standalone) | ~1,950 |
| Built-in tools | 39 | 5 |
| LLM providers | OpenRouter (custom client) | Any (OpenAI-compatible via proxy) |
| Tool discovery | Hardcoded registry | MCP `tools/list` |
| Compute | Morph HTTP + bwrap abstraction | Direct in VM (`Command::new`) |
| Context management | Custom compaction, workspace cache | Platform decides (MCP resources or ACP) |
| Platform comms | Custom ACP RPC client | ACP (lifecycle) + MCP (tools) |
| Model routing | Built-in classifier | Proxy decides |
| Standalone mode | Separate `standalone/` binary | Same binary + local MCP server |
| Binary size | ~15MB (many deps) | ~5MB |
| Maintenance surface | Large — every new tool/feature touches harness | Small — new features are MCP tools |

---

## Open Questions

1. **ACP + MCP multiplexing?** Overloop needs both ACP (to platform for lifecycle) and MCP (to platform for tools). Are these two connections? Or does the platform expose one endpoint that speaks both? Keeping them separate is cleaner but means two connections from VM to platform.

2. **Context assembly:** Who builds the system prompt? Options:
   - **ACP `initialize`** ships the full system prompt (current model)
   - **MCP resources** — Overloop reads `AGENT.md`, `USER.md`, `MEMORY.md` via MCP and assembles itself
   - **Hybrid** — ACP ships base prompt, MCP provides dynamic resources

   Recommendation: ACP ships base system prompt in `initialize`. If the loop needs to refresh context mid-session, it reads MCP resources. This matches the Phase 2→3 transition already designed.

3. **Streaming exec output.** The built-in `exec` tool needs to stream long-running command output back to the LLM (and to ACP for user visibility). This is the most complex built-in tool. Current harness has `wait_exec` + `abort_exec` for async exec. Overloop should support this but simpler — one `exec` tool with a timeout, stdout streamed.

4. **Subagent model.** Current harness spawns subagents with restricted tool sets. In Overloop, a subagent is just another Overloop process with a different MCP config (fewer tools announced). The platform MCP server can provide `spawn_agent` as an MCP tool that tells agent-runner to start another Overloop instance.

5. **When to extract to separate repo.** Start as `overloop/` crate in the monorepo. Extract to `overfolder/overloop` when the interface stabilizes and we want independent versioning/CI.

---

## Relationship to Standalone Mode

Standalone mode = Overloop + local MCP server + direct LLM calls (no proxy).

```
┌──────────────────────────────────────────┐
│  User's machine                          │
│                                          │
│  overloop binary                         │
│    ├── ACP: stdio (interactive CLI)      │
│    │   or headless (gateway API)         │
│    │                                     │
│    ├── MCP: local-platform-mcp (sidecar) │
│    │   ├── schedule (cron files)         │
│    │   ├── memory (local files)          │
│    │   └── secrets (env vars / keyring)  │
│    │                                     │
│    └── LLM: direct to provider           │
│        (user's own API key)              │
│                                          │
│  .overfolder/                            │
│    ├── config.yaml                       │
│    ├── sessions/                         │
│    └── memory/                           │
└──────────────────────────────────────────┘
```

Same binary. Different MCP server (local files vs cloud platform). Different LLM endpoint (direct vs proxy). The loop doesn't know or care.

---

## Relationship to platform-not-harness.md

This design implements the "thin harness" described in [platform-not-harness.md](platform-not-harness.md) but **replaces the Claude Code recommendation with a purpose-built minimal harness**. Reasons:

- No LLM-agnostic pluggable harness exists in the market
- Claude Code is locked to Anthropic
- Codex is locked to OpenAI
- OpenCode is too large / Node.js
- Goose is building a full product, not a pluggable engine

At ~2k LOC in Rust, Overloop is small enough that "build vs buy" is a non-question. The maintenance burden is less than a single tool file in the current harness.

The strategic thesis remains: **the platform is the product, the harness is glue.** Overloop is just very minimal, very focused glue.
