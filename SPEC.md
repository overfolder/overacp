# over/ACP — remote agentic compute, factored out

**Status:** Draft (initial commit, 2026-04-06)
**Repo:** github.com/overfolder/overacp
**License:** Apache-2.0

## What this is

over/ACP is the framework that runs LLM agents on **remote compute** behind a
**single multiplexed WebSocket tunnel**, with a small **agent loop** at the
other end. It is the substrate that Overfolder runs on, extracted so anyone
can build on it without taking on Overfolder's product surface (Telegram,
billing, channels, secret vault).

The primitives:

- **Protocol** — JSON-RPC 2.0 messages for session control, streaming, tool
  calls, quota, persistence. The wire format is small and stable.
- **Server** — accepts WebSocket tunnels from agents, dispatches the protocol,
  optionally proxies LLM calls (OpenAI-compatible passthrough), and exposes
  pluggable traits for storage, auth, quota, tools, and compute backends.
- **Agent** — runs *inside* the compute environment (a VM, container, bare
  metal, your laptop). Holds one WebSocket to the server, supervises a child
  agent process, and bridges its stdio to the wire.
- **Loop** — the reference agent: a minimal agentic loop with built-in
  filesystem/exec tools, optional MCP, and an OpenAI-compatible LLM client.
  Talks the protocol on stdin/stdout. Other agents can be plugged in.

```
┌─────────────────────────┐                   ┌─────────────────────────┐
│  overacp-server         │   one WebSocket   │  overacp-agent          │
│  (control plane)        │◄─────────────────►│  (one per compute unit) │
│                         │   JSON-RPC 2.0    │                         │
│  - WS hub               │                   │  - reconnects            │
│  - Auth (JWT/OIDC/...)  │                   │  - spawns child         │
│  - LLM proxy            │                   │  - bridges stdio        │
│  - SessionStore trait   │                   │                         │
│  - QuotaPolicy trait    │                   │      stdin/stdout       │
│  - ToolHost trait       │                   │           ▲             │
│  - ComputeBackend trait │                   │           │             │
└─────────────────────────┘                   │  ┌────────┴────────┐    │
                                              │  │  overloop        │   │
                                              │  │  (or any agent)  │   │
                                              │  │                  │   │
                                              │  │  - LLM client    │   │
                                              │  │  - built-in tools│   │
                                              │  │  - MCP client    │   │
                                              │  └──────────────────┘   │
                                              └─────────────────────────┘
```

## Why ACP

The framework speaks "ACP" — short for **Agent Control Protocol**. It is a
small JSON-RPC 2.0 vocabulary covering session lifecycle, streaming output,
tool calls, persistence, and quota. It is intentionally close to other
emerging agent protocols (Zed/Anthropic Agent Client Protocol, IBM ACP) so
that adapters can be cheap, but it is its own thing because it includes the
control-plane responsibilities (storage, quota, multiplexing, compute
provisioning) those other protocols leave open.

The protocol crate is the contract; the server and agent are reference
implementations.

## Crate map (target)

| Crate | Status | What |
|---|---|---|
| `overacp-protocol` | TODO | Wire types, JSON-RPC method names, JWT claims helpers, no I/O |
| `overacp-agent` | TODO | WS client, child supervisor, stdio bridge, `WorkspaceSync` and `AgentAdapter` traits |
| **`overloop`** | **here** | Reference agent: agentic loop + built-in tools + LLM client |
| `overacp-server` | TODO | The controlplane: REST API for compute pools/nodes/agents, `ComputeProvider` trait, tunnel + dispatcher + LLM proxy |
| `overacp-compute-local` | TODO | `ComputeProvider` impl: spawns the agent as a local subprocess. Zero-infra default. |
| `overacp-compute-docker` | TODO | `ComputeProvider` impl: Docker daemon |
| `overacp-compute-morph` | TODO | `ComputeProvider` impl: Morph Cloud (lifted from `overfolder/backend/src/routes/workspace.rs`) |
| `overacp-workspace-gcs` | TODO | `WorkspaceSync` impl: GCS object prefix |
| `overacp-workspace-s3` | TODO | `WorkspaceSync` impl: S3-compatible |
| `overacp-tools-mcp` | TODO | `ToolHost` impl that fans out to backing MCP servers (controlplane-side) |

Each compute provider and each workspace-sync backend is its own
crate so the server binary stays small and operators pick what to
compile in. See [`docs/design/controlplane.md`](./docs/design/controlplane.md)
and [`docs/design/workspace-sync.md`](./docs/design/workspace-sync.md)
for the full design.

## Non-goals (intentional)

- **Not a hosted product.** No billing, no channels (Telegram/WhatsApp/Slack),
  no per-user dashboard. Build those on top.
- **Not a tool registry.** Tools come from the agent's built-ins, from MCP
  servers you wire in, or from your own `ToolHost` implementation.
- **Not opinionated about identity.** JWT works out of the box for dev; bring
  your own `Authenticator` for production (OIDC, API keys, mTLS, ...).
- **Not opinionated about storage.** Server traits over Postgres, SQLite, or
  in-memory. Pick what fits.
- **Not coupled to one compute provider.** Reference adapters for local
  process, Docker, Firecracker, and Morph; trait lets you add more.

## What goes here vs. what stays in Overfolder

**Lives in over/ACP (generic, OSS):**
- The wire protocol and its types.
- The WebSocket multiplexer and JSON-RPC dispatcher.
- The OpenAI-compatible LLM proxy with hooks (auth, model routing, metering).
- The agent process supervisor and stdio bridge.
- The reference agent loop and its built-in tools.
- Reference compute backends.

**Stays in Overfolder (product, closed or separate):**
- Channels gateway (Telegram/WhatsApp webhooks, web dashboard).
- Identity hierarchy and secret vault (Overslash).
- Platform MCP tools (`overfolder-mcp`: schedule, send_message, spawn_agent,
  search_history, ...).
- Postgres schema for users/conversations/messages (Overfolder's
  `SessionStore` impl points at this).
- Tier-based quota policy and Stripe billing.
- Morph image baking, Terraform, deployment scripts.

Overfolder's `controlplane` becomes a thin shim crate that depends on
`overacp-server` and plugs in Overfolder-specific implementations of the
traits.

## Roadmap

The roadmap is shaped around standing up a controlplane that owns
**compute provisioning and agent lifecycle**, not just protocol
plumbing. The server is the centerpiece; everything before it is
prep, everything after it is filling in providers and demos.

1. **0.1 — vendor `loop`** *(landed)* — copy the reference agent in
   as the `overloop` crate, set up the workspace.
2. **0.2 — `overacp-protocol`** — extract the wire types from
   Overfolder's `controlplane/src/{acp,session}.rs`. Pure types, no I/O.
3. **0.3 — `overacp-agent`** — lift `overfolder/overlet` here, depend
   on `overacp-protocol`. `AgentAdapter` and `WorkspaceSync` traits.
4. **0.3.x — `overloop` migration** — make the reference agent
   protocol-conformant: depend on `overacp-protocol`, fix the
   `session/message → poll/newMessages` flow, emit `stream/toolCall`
   and `stream/toolResult`, unify the four tool sources per
   [`docs/design/loop-tools.md`](./docs/design/loop-tools.md).
5. **0.4 — `overacp-server`: compute-pool controlplane** — the
   centerpiece. REST API for compute pools, compute nodes, and
   agents (Kafka-Connect-shaped); the `ComputeProvider` trait;
   in-memory + SQLite persistence; the existing tunnel/dispatcher/LLM
   proxy carried over; `SessionStore`, `QuotaPolicy`, `ToolHost`,
   `Authenticator` traits; secret references in pool configs;
   `overacp-compute-local` ships in the same milestone so the demo
   works without infra. Full design in
   [`docs/design/controlplane.md`](./docs/design/controlplane.md).
6. **0.5 — production providers + demo** —
   `overacp-compute-docker`, `overacp-compute-morph`,
   `overacp-workspace-gcs`, `overacp-workspace-s3`. End-to-end demo:
   `cargo run`, then `curl POST /api/v1/compute/pools`, then
   `POST /api/v1/agents`, then `POST /api/v1/agents/{id}/messages`.
7. **0.6 — `overacp-tools-mcp` + Overfolder cutover** — controlplane
   ships its MCP `ToolHost` adapter; Overfolder's `controlplane`
   shrinks to the Postgres `SessionStore`, the Telegram channel, and
   the Overslash auth provider. Morph integration leaves Overfolder
   and lands here as `overacp-compute-morph`. Archive
   `overfolder/overloop`.
8. **1.0 — protocol + REST freeze** — semver-stable wire format and
   v1 REST surface.

## Resolved decisions

These were "open questions" earlier; the design docs now pin them
down. They are listed here so the SPEC reads as the current
position.

- **Protocol method naming.** Borrow Zed/Anthropic ACP names where
  they overlap (`initialize`, `session/message`); borrow MCP names
  for the tool surface (`tools/list`, `tools/call`); use over/ACP
  names for everything else. Per-method classification in
  [`docs/design/protocol.md`](./docs/design/protocol.md) § 3.
- **Tool model.** `ToolHost` is the trait. Tools reach the agent
  through three controlplane-mediated sources:
  1. **ACP-tunnelled**: the controlplane runs MCP clients itself
     and re-exposes them through `ToolHost` as
     `tools/list` / `tools/call` over the ACP tunnel. Default and
     recommended.
  2. **Agent-side MCP descriptors**: the controlplane sends MCP
     server descriptors (URL or stdio command, headers, short-lived
     credentials) in the `initialize` response and the agent
     connects to them with its own MCP client. The controlplane
     remains authoritative; deployments that want zero agent-side
     MCP set the descriptor list to empty.
  3. **Supervisor-injected**: `overacp-agent` passes a stdio/HTTP
     MCP descriptor to the child loop via env var or first-line
     handshake. Use case: sandbox-local helpers (LSP, fs proxies)
     that should not round-trip through the controlplane.

  Tool names are namespaced per source (`builtin/`, `injected/`,
  `acp/`, `mcp/<server>/`). Full design in
  [`docs/design/loop-tools.md`](./docs/design/loop-tools.md).
- **Workspace sync.** Belongs to the agent supervisor, not the
  controlplane. Shipped as a per-backend crate (`overacp-workspace-gcs`,
  `overacp-workspace-s3`, `overacp-workspace-rclone`, ...) implementing
  `WorkspaceSync`. The controlplane only persists the descriptor on
  the agent record. Full design in
  [`docs/design/workspace-sync.md`](./docs/design/workspace-sync.md).
- **Compute model.** Operators register *compute pools* (named
  configured instances of a `ComputeProvider`) over a Kafka-Connect-style
  REST API. Agents are pinned to a pool at creation and the
  controlplane records `(pool, node_id)` on every agent. Provider
  credentials never leave the controlplane. Full design in
  [`docs/design/controlplane.md`](./docs/design/controlplane.md).

## Open questions

- **Multi-agent orchestration.** A parent agent spawning child
  agents on new compute nodes is expressible as a `ComputeProvider`
  call inside `tools/call`, but no worked example exists yet. The
  REST surface in [`docs/design/controlplane.md`](./docs/design/controlplane.md)
  § 3.4 makes this *possible* (agents are top-level resources); the
  product pattern is open.
- **Streaming exec on compute nodes.** v1 `POST .../exec` is one-shot.
  Streaming exec is wanted but the wire shape is undecided (SSE? a
  second WebSocket route? piggyback on the existing tunnel?).
- **Cross-pool fair-share / scheduling.** Pools today are opaque;
  bin-packing across pools is a future scheduling layer.
