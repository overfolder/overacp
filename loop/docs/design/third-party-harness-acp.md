# Third-Party Harness via ACP — Exploration

**Status:** Under Consideration (not planned for implementation)
**Date:** 2026-03-31
**Related:** [acp-node-protocol.md](acp-node-protocol.md), [overfolder-jsonrpc-api.md](overfolder-jsonrpc-api.md), [third-party-harness.md](third-party-harness.md)

---

## Question

How far is Overfolder's JSON-RPC API from the Agent Client Protocol (ACP) standard? Could Overfolder support third-party agent harnesses (Claude Code, Codex, OpenCode, etc.) instead of or alongside our own harness?

## Context

Our `acp/` crate adopted ACP's framing (JSON-RPC 2.0 over stdio) but defines a custom method set (26 methods) purpose-built for Overfolder's platform concerns. The overlap with the ACP standard is mostly at the transport layer.

### How OpenClaw Does It

OpenClaw (open-source personal AI framework) integrates Claude Code and Codex through two layers:

**Layer 1 — ACP Bridge (`openclaw acp`):** Translates between ACP protocol and OpenClaw's internal Gateway WebSocket. IDE clients speak ACP, the bridge maps `prompt` → `chat.send`, `cancel` → `chat.abort`, etc.

**Layer 2 — CLI Runner:** Spawns `claude` or `codex` as a **black-box subprocess**:
```bash
claude -p --output-format json --permission-mode bypassPermissions \
  --model opus --session-id {sessionId} --append-system-prompt "..."
```
Waits for JSON output, parses result, maps back to internal format. Session continuity via Claude Code's own `--resume {sessionId}`.

**What OpenClaw can see:** Final text output only. **Cannot see:** Real-time streaming, tool calls, files modified, cost/usage. No quota control — the agent owns its own API key.

**ACPX** (separate tool) wraps various agents as ACP servers. Supports 15+ harnesses: Claude Code (via `claude-agent-acp`), Codex (via `codex-acp`), Gemini, Kiro, OpenCode, Kilo, and others. Uses ACP's `fs/*` and `terminal/*` methods for file/command access.

### Key Differences: Overfolder ACP vs Standard ACP

| Concern | ACP (JetBrains/Zed) | Overfolder |
|---------|---------------------|------------|
| LLM calls | Platform-mediated | Harness calls OpenRouter directly |
| Tool execution | Protocol-defined, client-visible | Harness-internal, compute provider abstraction |
| Context assembly | Client's job | Platform ships or harness assembles from workspace |
| Platform services | Minimal (lifecycle) | Rich: scheduling, approvals, multi-agent, quota, messaging |
| Scope | Generic agent ↔ IDE contract | Full cloud platform contract |

---

## Integration Tiers

### Tier 1: CLI Runner (OpenClaw's approach)

Spawn `claude -p --output-format json` as subprocess. Parse output. No streaming, no tool visibility, no quota control.

- **Effort:** Weeks
- **Experience:** Batch in/out. User sees nothing until turn completes.
- **Verdict:** Insufficient for Overfolder (real-time Telegram streaming is core).

### Tier 2: ACP Client (speak ACP to harness's ACP server)

Agent-runner acts as ACP client. Harness runs as ACP server (e.g., via ACPX adapters). Gets streaming text + tool call notifications.

- **Effort:** Months
- **Experience:** Real-time streaming to Telegram, tool visibility, session management.
- **Gap:** No LLM cost control (harness makes its own API calls).

### Tier 3: MCP Platform Server

Expose Overfolder platform services as MCP tools that any MCP-capable harness can discover:

- `schedule_create/list/delete` — scheduling
- `approval_request/check` — gated actions
- `message_send` — channel delivery (Telegram, web)
- `workspace/*` — memory, config as MCP resources

- **Effort:** ~1,200 lines
- **Experience:** Harness gets Overfolder superpowers via standard MCP protocol.

### Required: LLM Proxy

Running third-party harnesses with subscription keys likely violates provider ToS for a hosted platform. With Overfolder org API keys, we need quota tracking. **Solution: proxy all LLM calls.**

Both Claude Code and Codex support base URL overrides:
```bash
ANTHROPIC_BASE_URL=https://llm-proxy.overfolder.internal/v1
OPENAI_BASE_URL=https://llm-proxy.overfolder.internal/v1
```

The proxy:
1. Authenticates (session token → user + quota)
2. Checks quota, rejects if exhausted
3. Forwards to upstream (Anthropic, OpenRouter, OpenAI) with org key
4. Streams response back
5. Meters tokens, calculates cost, deducts from quota

~800 lines. Stateless HTTP reverse proxy with auth + metering.

---

## Transport: Morph Integration

Morph currently has **HTTP exec API only** (no vsock, no WebSocket). ovfguest CLI runs inside the VM.

**Recommended: Reverse WebSocket from VM**

1. Agent-runner sends HTTP exec to Morph: "start harness, connect back to `wss://agent-runner/acp/{session}`"
2. ovfguest inside VM spawns the harness (e.g., Claude Code in ACP mode)
3. ovfguest opens outbound WebSocket TO agent-runner
4. Bridges harness stdio ↔ WebSocket
5. Bidirectional ACP JSON-RPC flows over the WebSocket

Works because the VM already needs outbound network for LLM proxy calls. No Morph API changes needed.

---

## Code Impact Analysis

### What we'd delete (~18.5k lines)

| Component | LOC | Replaced by |
|-----------|-----|-------------|
| `harness/src/tools/` | 6,558 | Harness built-in tools |
| `harness/src/agent/` | 4,727 | Harness agentic loop |
| `harness/src/compute/` | 2,024 | Direct execution in VM |
| `harness/src/llm/` | 1,414 | Harness owns LLM calls (through proxy) |
| `harness/src/platform/` | 460 | Replaced by MCP |
| `harness/src/main.rs` + classifier | 714 | No harness binary |
| `standalone/src/` | 2,599 | Harness IS standalone |

### What we'd build (~3k lines)

| Component | Est. LOC | What |
|-----------|----------|------|
| LLM Proxy | ~800 | Auth, meter, forward, stream |
| MCP Server | ~1,200 | Platform tools + resources |
| ACP Client adapter | ~600 | In agent-runner, relay streaming to Valkey |
| VM bootstrap + CLAUDE.md gen | ~400 | Install harness, configure env |

### Net: ~15k lines eliminated

---

## Harness Comparison for Overfolder

Criteria: headless, ACP, MCP client, multi-provider LLM, open source, lightweight (VM), session resume, non-code tasks.

| Harness | OSS | Lang | ACP | MCP | LLM Flex | BASE_URL | Headless | Resume | Non-Code | VM Size | Fit |
|---------|:---:|:----:|:---:|:---:|:--------:|:--------:|:--------:|:------:|:--------:|:-------:|:---:|
| **Claude Code** | No | Node | Yes | Yes | Anthropic | Yes | Yes | Yes | Excellent | ~200MB | **A** |
| **OpenCode** | Yes | Node | Yes | ? | 75+ | Yes | Yes | Yes | Good | ~200MB | **A-** |
| **Codex CLI** | Yes | Rust | Yes | Yes | OpenAI | Yes | Yes | Yes | Limited | ~50MB | **B+** |
| **Goose** | Yes | Rust | ? | Yes | Any | Yes | Yes | ? | Good | ~80MB | **B+** |
| **Cline CLI** | Yes | Node | Yes | Yes | Any | Yes | Yes | Yes | Good | ~200MB | **B+** |
| **Kilo** | No | TS | Yes | ? | 500+ | Yes | Yes | Yes | Good | ~200MB | **B+** |
| **Aider** | Yes | Py | Yes | No | 100+ | Yes | Yes | Yes | Limited | ~300MB | **B** |
| **Gemini CLI** | Yes | ? | ? | ? | Gemini | ? | Yes | Yes | Good | ? | **B** |
| **Kiro CLI** | No | ? | Yes | Yes | Claude | ? | Yes | Yes | Limited | ? | **B-** |
| **Our Harness** | Ours | Rust | Native | No | Any (OR) | N/A | Yes | Yes | Excellent | ~15MB | **A+** |

### Top 3 Candidates

1. **Claude Code** — Best agentic loop (80.9% SWE-bench), MCP client, excellent at non-code tasks. Downside: Anthropic-only, ~$0.01-0.05/turn, not open source, 200MB.

2. **OpenCode** — 75+ LLM providers (cheap model routing), open source, ACP, good at general tasks. Downside: Node.js, less battle-tested.

3. **Codex CLI** — Open source Rust (same as our stack), smallest binary ~50MB, ACP + Agents SDK. Downside: OpenAI-only, code-focused (weak at research/planning/communication).

---

## Cost Tradeoff

| Harness | Cost/Turn | Viable Tier |
|---------|-----------|-------------|
| Our harness + Minimax | ~$0.001 | Free, all tiers |
| OpenCode + cheap model | ~$0.002 | Free extended, paid |
| Claude Code + Anthropic | ~$0.01-0.05 | Paid, BYOK only |
| Codex + OpenAI | ~$0.005-0.02 | Paid, BYOK only |

This suggests a **dual-harness model** if we ever pursue this:
- Free tier → our harness (Minimax, cheapest)
- Paid tier → user chooses: our harness (default) or premium harness (opt-in)
- BYOK → any harness (user pays their own API costs)

---

## Observability: Langfuse Support

| Harness | Langfuse | How | Depth |
|---------|:--------:|-----|-------|
| **Claude Code** | Yes | [Hooks-based](https://langfuse.com/integrations/other/claude-code) — `Stop` hook captures conversations + tool calls. Community: [claude-langfuse-monitor](https://github.com/michaeloboyle/claude-langfuse-monitor) (zero-instrumentation). | Good — inputs, responses, tools, timing. Post-hoc (hook fires after response), not per-LLM-call. |
| **OpenCode** | WIP | [Issue #6142](https://github.com/anomalyco/opencode/issues/6142) — Langfuse exporter plugin in development. Vercel AI SDK has [native Langfuse provider](https://ai-sdk.dev/providers/observability/langfuse). Missing sessionID correlation. | Incomplete — not production-ready. |
| **Codex CLI** | Via MCP | `codex mcp add langfuse` — [Langfuse MCP server](https://github.com/avivsinai/langfuse-mcp) for querying traces. Trace emission via OpenAI SDK instrumentation. | Indirect — MCP lets Codex *query* Langfuse; emission depends on SDK layer. |
| **Goose** | Native | [Official guide](https://langfuse.com/integrations/no-code/goose) — env vars (`LANGFUSE_PUBLIC_KEY`, `LANGFUSE_SECRET_KEY`, `LANGFUSE_HOST`). Captures prompts, responses, tokens, latency, tool steps. | Best — native, per-call traces, tool invocation detail. |
| **Our Harness** | Native | Direct Langfuse SDK integration in `agent-runner`. Per-session traces, generations, tool spans, routing events. | Best — full control, per-LLM-call + tool-level spans. |

### LLM Proxy as Universal Observability Layer

With the LLM proxy (required for quota), we get Langfuse for **any harness for free**:

```
Any Harness → LLM Proxy → Langfuse trace (prompt, response, tokens, cost, latency) → Upstream API
```

The proxy sees raw API requests — ideal instrumentation point. Per-LLM-call granularity regardless of harness support. What we'd lose vs harness-native: tool call context (which tool triggered which LLM call). But tool calls are visible via ACP streaming (`stream/tool_call`), so we can correlate in our own pipeline.

**The proxy solves quota, cost tracking, and observability in one place.**

---

## Decision

**Under consideration — not pursuing now.** The exploration confirms it's architecturally feasible but the ROI isn't clear yet:

- Our harness at ~20k LOC is manageable and purpose-built for Overfolder's needs
- The LLM proxy is independently valuable (centralized metering) and could be built first
- Third-party harness support becomes compelling when/if:
  - Users demand Claude Code quality for non-code tasks
  - ACP ecosystem matures with better streaming/tool visibility
  - We want to reduce harness maintenance burden
  - A multi-harness marketplace becomes a differentiator

The LLM proxy and MCP server are prerequisites regardless — they improve our own harness too.

---

## References

- [OpenClaw ACP Agents](https://docs.openclaw.ai/tools/acp-agents)
- [ACPX — Headless ACP CLI](https://github.com/openclaw/acpx)
- [OpenCode ACP Support](https://opencode.ai/docs/acp/)
- [Codex CLI](https://developers.openai.com/codex/cli)
- [ACP Agent Registry — JetBrains](https://blog.jetbrains.com/ai/2026/01/acp-agent-registry/)
- [Kiro CLI ACP](https://kiro.dev/docs/cli/acp/)
- [15 AI Coding CLI Tools Compared — Tembo](https://www.tembo.io/blog/coding-cli-tools-comparison)
- [15 AI Coding Agents Tested — Morph](https://www.morphllm.com/ai-coding-agent)
- [Claude Code Tracing with Langfuse](https://langfuse.com/integrations/other/claude-code)
- [Goose + Langfuse Integration](https://langfuse.com/integrations/no-code/goose)
- [Langfuse MCP Server](https://github.com/avivsinai/langfuse-mcp)
- [Vercel AI SDK Langfuse Provider](https://ai-sdk.dev/providers/observability/langfuse)
