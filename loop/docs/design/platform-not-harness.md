# Platform, Not Harness — Strategic Architecture

**Status:** Under Consideration
**Date:** 2026-03-31
**Related:** [third-party-harness-acp.md](third-party-harness-acp.md), [acp-node-protocol.md](acp-node-protocol.md), [overfolder-jsonrpc-api.md](overfolder-jsonrpc-api.md)

---

## Core Thesis

Overfolder's value is the **platform** — zero-setup, persistent workspace, orchestration, connectors, channels — not the agentic loop. The harness is a commodity (15+ competitors, big tech budgets, open source alternatives). Competing on harness quality is a losing game. Instead: **commoditize the harness, own the platform.**

The Heroku analogy: Heroku didn't build Ruby or PostgreSQL. They built `git push heroku main`. The runtime was commodity — the platform was the moat.

---

## What Makes the Experience A+

| Factor | Who provides it |
|--------|-----------------|
| Smart, capable responses | Harness + model (commodity) |
| Zero setup — message on Telegram | **Platform** |
| Persistent workspace with files | **Platform** (Morph VM + sync) |
| Remembers me across sessions | **Platform** (workspace memory) |
| Schedules tasks, sends briefings | **Platform** (scheduler + channels) |
| Reads email, checks calendar | **Platform** (connectors) |
| Available on Telegram, WhatsApp, web | **Platform** (channel routing) |
| Sandboxed, won't leak data | **Platform** (VM isolation) |
| Affordable at scale | **Platform** (LLM proxy + quota) |

9 out of 10 A+ factors are platform. The harness is one row — and the most commoditized one.

---

## Decision: Single Harness, Model Ladder

**One harness across all tiers. The LLM proxy routes to different model qualities. Same experience, different intelligence levels.**

Two harnesses (cheap for free, premium for paid) was rejected because:
- Two codebases to maintain
- Inconsistent UX — free users get a worse first impression
- Bad retention — users churn before seeing the platform value
- The cost delta (~$0.40/month per free user) doesn't justify the experience gap

### Recommended: Claude Code + Anthropic Model Ladder

Claude Code is the best agentic loop available (80.9% SWE-bench). Anthropic's model lineup covers the full cost/quality spectrum:

| Tier | Model | Cost/turn (est.) | Experience |
|------|-------|-----------------|------------|
| Free | Haiku 4.5 | ~$0.005 | Good — fast, competent, handles tools well |
| Paid | Sonnet 4.6 | ~$0.015 | Great — strong reasoning, excellent tools |
| Premium / BYOK | Opus 4.6 | ~$0.05 | A+ — best available |

**One harness. One codebase. One experience. Model quality scales with tier.**

### Why Claude Code over alternatives

| Candidate | LLM Flex | OSS | Fit | Why not single-harness |
|-----------|:--------:|:---:|:---:|------------------------|
| **Claude Code** | Anthropic only | No | **A** | Recommended — best loop, MCP client, ACP, Haiku is cheap enough for free tier |
| **OpenCode** | 75+ providers | Yes | A- | True model agnosticism, but tool quality varies by model — cheap models fumble tools, degrading free tier |
| **Codex CLI** | OpenAI only | Yes | B+ | Code-focused, weak at personal AI tasks (research, communication, scheduling) |
| **Goose** | Any | Yes | B+ | MCP-native but less mature, unclear ACP support |

OpenCode's model flexibility is appealing but creates a hidden problem: **tool orchestration quality varies by model**. A cheap model through OpenCode may technically work but fumble tool calls, producing a worse experience than Haiku through Claude Code. The harness is the same — the model can't keep up.

Claude Code + Haiku is consistently good because Claude Code is **optimized for Claude models**. Tool use, extended thinking, context management all work best with the model family it was built for.

### Anthropic lock-in: risk and mitigation

**Risk:** Vendor dependency on Anthropic for both harness and models.

**Mitigations:**
- The LLM proxy is the abstraction layer — if we need to switch models, users don't notice
- ACP + MCP means the harness is swappable — replace Claude Code with OpenCode/Goose without changing the platform
- Anthropic is incentivized to maintain a cheap model tier (Haiku) for developer adoption
- The platform architecture is harness-agnostic by design — Claude Code is a choice, not a cage

---

## Architecture

```
All tiers, same stack:

Telegram/Web/WhatsApp
        │
        ▼
    Backend (message routing, auth, channels)
        │
        ▼
    Agent-Runner (orchestration, sessions, scheduling)
        │
        │  HTTP exec (kick off) + Reverse WebSocket (ACP streaming)
        ▼
    ┌─────────────────────────────────────────────────┐
    │   Morph VM (per-user persistent workspace)      │
    │                                                 │
    │   Claude Code (same binary, all tiers)          │
    │       │                                         │
    │       ├── ACP ──► ovfguest ──► Agent-Runner     │
    │       │          (reverse WS bridge)            │
    │       │                                         │
    │       ├── MCP ──► Overfolder MCP Server          │
    │       │          (schedule, approve, message,   │
    │       │           connectors, memory)            │
    │       │                                         │
    │       └── LLM ──► LLM Proxy ──► Anthropic API   │
    │                   │                             │
    │                   │  Proxy routes by tier:      │
    │                   │  free → haiku 4.5           │
    │                   │  paid → sonnet 4.6          │
    │                   │  premium → opus 4.6         │
    │                   │                             │
    └─────────────────────────────────────────────────┘
```

### Platform Components

| Component | Role | New? |
|-----------|------|:----:|
| **LLM Proxy** | Auth, quota enforcement, model routing by tier, cost metering, Langfuse tracing | New (~800 LOC) |
| **MCP Server** | Exposes platform services as MCP tools — scheduling, approvals, message sending, connectors, workspace config, memory | New (~1,200 LOC) |
| **ACP Relay** | ovfguest bridges Claude Code stdio ↔ reverse WebSocket to agent-runner | New (~600 LOC) |
| **VM Bootstrap** | Install Claude Code, write CLAUDE.md, set env vars, start ACP relay | New (~400 LOC) |
| **Agent-Runner** | Orchestration, session management, Valkey streaming, persistence | Exists (adapt subprocess.rs for WS) |
| **Backend** | Channels, auth, message routing | Exists (unchanged) |
| **Morph VM** | Persistent per-user workspace with file sync | Exists (unchanged) |

### What Gets Deleted (~18.5k LOC)

| Component | LOC | Replaced by |
|-----------|-----|-------------|
| `harness/src/tools/` | 6,558 | Claude Code built-in tools |
| `harness/src/agent/` | 4,727 | Claude Code agentic loop |
| `harness/src/compute/` | 2,024 | Direct execution in VM |
| `harness/src/llm/` | 1,414 | Claude Code LLM calls (via proxy) |
| `harness/src/platform/` | 460 | Replaced by MCP |
| `harness/src/main.rs` + classifier | 714 | No harness binary |
| `standalone/src/` | 2,599 | Claude Code IS standalone |

### What Gets Built (~3k LOC)

| Component | Est. LOC |
|-----------|----------|
| LLM Proxy | ~800 |
| MCP Server | ~1,200 |
| ACP Relay (ovfguest) | ~600 |
| VM bootstrap + CLAUDE.md gen | ~400 |

**Net: delete ~18.5k, build ~3k. ~15k lines eliminated.**

---

## The LLM Proxy as Control Plane

The proxy is the most load-bearing new component. It solves three problems in one:

### 1. Quota & Billing
- Authenticate session token → resolve user + tier + remaining quota
- Reject requests when quota exhausted
- Deduct usage after each response
- Support BYOK (route to user's key instead of org key)

### 2. Model Routing
- Free tier → `claude-haiku-4-5` (cheap, fast, competent)
- Paid tier → `claude-sonnet-4-6` (strong reasoning)
- Premium/BYOK → `claude-opus-4-6` or user-specified model
- Model allowlists per tier (prevent free users requesting Opus)

### 3. Observability
- Emit Langfuse traces for every LLM call (prompt, response, tokens, cost, latency)
- Works for ANY harness — the proxy sees raw API requests
- Correlate with ACP tool call events for full trace chains

**The proxy is stateless** — auth via session token in header, quota check via Redis/PostgreSQL, forward to Anthropic, stream back, meter. Could be a small Cloud Run service or middleware in agent-runner.

---

## Observability: Langfuse Support

| Harness | Langfuse | How | Depth |
|---------|:--------:|-----|-------|
| **Claude Code** | Yes | [Hooks-based](https://langfuse.com/integrations/other/claude-code) — `Stop` hook captures conversations + tool calls | Good — post-hoc, not per-LLM-call |
| **OpenCode** | WIP | [Issue #6142](https://github.com/anomalyco/opencode/issues/6142) — exporter plugin in development | Incomplete |
| **Codex CLI** | Via MCP | [Langfuse MCP server](https://github.com/avivsinai/langfuse-mcp) for querying traces | Indirect |
| **Goose** | Native | [Official integration](https://langfuse.com/integrations/no-code/goose) — env vars, per-call traces | Best native support |

**With the LLM proxy, we get Langfuse for any harness at the API call level.** The proxy emits per-call traces (prompt, response, tokens, cost, latency). Tool call context comes from ACP streaming. Full observability regardless of harness choice.

### Sentry: Process-Level Error Reporting

Langfuse and Sentry cover orthogonal axes and should not be confused:

| Axis | Tool | What it captures |
|------|------|------------------|
| LLM call traces | Langfuse | Prompt, response, tokens, cost, latency — one trace per API call. |
| Process errors | Sentry | Panics and `tracing::error!` events from the agent process itself — broker disconnects, MCP failures, config errors, tool-handler bugs. |

`overloop` ships optional Sentry integration behind the `sentry` cargo feature (off by default, zero compile-time and runtime cost when unused). At runtime, activation requires `SENTRY_DSN` — otherwise everything is a no-op. When enabled, every `tracing::error!` automatically becomes a Sentry event via the `sentry-tracing` layer; no call-site changes required.

**Fleet identification.** A fleet of overloop processes on many VMs needs a way to attribute an error (or a log line) back to one specific process. That is what `OVERLOOP_AGENT_NAME` is for: a process-local identity env var that becomes a field on the root tracing span and a tag on every Sentry event (`agent_name:worker-42`). It is deliberately decoupled from the over/ACP wire `agent_id` (the JWT `sub` claim) — the broker never sees it, and operators can set it to whatever fits their orchestration (pod name, VM id, worker label, …). See [`SPEC.md § Fleet observability and identity`](../../../SPEC.md#fleet-observability-and-identity).

---

## Transport: Morph VM Integration

Morph currently has HTTP exec API only. ovfguest CLI runs inside the VM.

### Reverse WebSocket (recommended)

1. Agent-runner sends HTTP exec to Morph: "start Claude Code ACP relay, connect to `wss://agent-runner/acp/{session}`"
2. ovfguest inside VM spawns Claude Code with ACP mode
3. ovfguest opens **outbound** WebSocket to agent-runner (VM already needs outbound for LLM proxy)
4. Bridges Claude Code stdio ↔ WebSocket
5. Bidirectional ACP JSON-RPC flows over the connection

No Morph API changes needed. One exec call to kick off, then persistent bidirectional streaming.

---

## User Experience: The Upgrade Path

The upgrade is invisible — same Telegram bot, same workspace, same memory. Responses just get smarter.

**Free user (Haiku):**
```
User: Research the best restaurants near Plaza Mayor for tonight

🔧 Searching the web...
🔧 Reading restaurant reviews...

Here are 5 well-rated restaurants near Plaza Mayor with availability
tonight. La Barraca has the best reviews for paella, Casa Lucio is
famous for huevos rotos. Want me to check if any take reservations?
```

**Same user after upgrading (Sonnet):**
```
User: Research the best restaurants near Plaza Mayor for tonight

🔧 Searching the web for "restaurants Plaza Mayor Madrid"...
🔧 Cross-referencing Google Maps ratings with recent reviews...
🔧 Checking reservation availability via web...

I found 8 restaurants within 5 minutes of Plaza Mayor. Here's my
analysis by cuisine type:

**Traditional Spanish:**
- Casa Lucio — famous for huevos rotos (4.5★, 2,300 reviews)
  Available tonight at 21:00 and 21:30
- Sobrino de Botín — world's oldest restaurant (4.3★)
  Fully booked, but I can check for cancellations

**Seafood:**
- La Barraca — best paella in the area (4.4★)
  Available at 20:30

Want me to make a reservation? I can also check your calendar to
confirm you're free.
```

Same bot. Same workspace. Same conversation history. Just smarter.

---

## Execution Path

1. **Now:** Finish ACP refactor with our harness. Ship what works.
2. **Next:** Build the LLM proxy. Independently valuable — centralizes metering for our current harness too.
3. **Then:** Build the MCP platform server. Works with our harness immediately, enables Claude Code later.
4. **When ready:** Add Claude Code as the harness. Reverse WebSocket transport, VM bootstrap, CLAUDE.md generator. Replace our harness entirely.
5. **Delete:** Remove `harness/`, `standalone/`, reduce `acp/` to types only.

Steps 2 and 3 improve our current harness — nothing wasted even if step 4 is delayed.

---

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Anthropic vendor lock-in | Medium | Proxy + ACP + MCP = harness-swappable. OpenCode as backup. |
| Claude Code licensing changes | Medium | Architecture is harness-agnostic. Can switch to OSS alternative. |
| Haiku quality insufficient for free tier | Low | Haiku 4.5 is genuinely capable. Monitor retention metrics. |
| Claude Code not designed for personal AI | Low | MCP tools + CLAUDE.md customization adapt it. The platform provides the personal AI context. |
| 200MB VM footprint for Claude Code | Low | Morph VMs can handle it. Cache the binary in the base image. |
| No streaming during free tier (cost concern) | Low | Streaming is via ACP, orthogonal to model choice. All tiers stream. |

---

## Defensibility

The harness market is a **red ocean** — 15+ competitors, big tech players, open source.

The personal AI platform market is **blue ocean**:
- Zero-setup via Telegram (ghost accounts, no API keys, no CLI)
- Persistent VM workspace per user
- Scheduling + proactive briefings
- OAuth connector ecosystem (Gmail, Calendar, Drive)
- Multi-channel delivery (Telegram, WhatsApp, web)
- All metered, observable, and wrapped in a managed platform

**Network effects compound:** workspace, memory, skills, connectors, conversation history make Overfolder stickier over time. The harness is replaceable. The accumulated context is not.

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
