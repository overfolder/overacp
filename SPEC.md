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
| `overacp-server` | TODO (extracted from `overfolder/controlplane`) | WS hub, dispatcher, LLM proxy, traits + built-in adapters |
| `overacp-agent` | TODO (extracted from `overfolder/overlet`) | WS client, child supervisor, stdio bridge |
| **`overloop`** | **here** (vendored from `overfolder/overloop`) | Reference agent: agentic loop + built-in tools + LLM client |
| `overacp-tools-mcp` | TODO | Optional MCP host adapter so the server can speak MCP to backing tool servers |
| `examples/local`, `examples/morph`, `examples/docker` | TODO | Reference compute backends |

The current commit ships only `overloop`. The protocol/server/agent
crates are being extracted from the Overfolder monorepo.

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

1. **0.1 — vendor `loop`** *(this commit)* — copy the reference agent in
   as the `overloop` crate, set up the workspace.
2. **0.2 — `overacp-protocol`** — extract the wire types from Overfolder's
   `controlplane/src/{acp,session}.rs`. Pure types, no I/O. The contract.
3. **0.3 — `overacp-agent`** — lift `overfolder/overlet` here, depend on
   `overacp-protocol`. Workspace sync becomes optional via a trait.
4. **0.4 — `overacp-server`** — lift `overfolder/controlplane`'s generic
   parts: tunnel, dispatcher, LLM proxy, SSE dev surface. Define the
   `SessionStore`, `QuotaPolicy`, `ToolHost`, `ComputeBackend`,
   `Authenticator` traits. Ship in-memory + SQLite reference impls.
5. **0.5 — examples** — `local-process`, `docker`, `morph` compute backends.
   End-to-end demo: clone repo, `cargo run --example local-process`, chat.
6. **0.6 — Overfolder cutover** — Overfolder's `controlplane` shrinks to
   ~80 LOC of glue. Archive `overfolder/overloop`.
7. **1.0 — protocol freeze** — semver-stable wire format.

## Open questions

- **Naming the protocol crate's methods.** Reuse Anthropic's ACP method names
  (`session/new`, `session/message`, ...) for surface compatibility, or pick
  our own and ship adapters? Leaning toward "borrow where it doesn't conflict,
  add what we need".
- **Tool model.** `ToolHost` is the trait, MCP is the default adapter. Tools
  reach the agent through three controlplane-mediated sources, listed in
  full in [`docs/design/loop-tools.md`](./docs/design/loop-tools.md):

  1. **ACP-tunnelled tools** — the controlplane runs MCP clients itself and
     re-exposes them through `ToolHost` as a unified `tools/list` /
     `tools/call` surface over the ACP tunnel. The agent never learns
     these came from MCP. Default and recommended; keeps secrets and
     egress on the controlplane.
  2. **Agent-side MCP descriptors** — the controlplane includes a list of
     MCP server descriptors (URL or stdio command, headers, short-lived
     credentials) in the `initialize` response. The agent connects to
     them with its own MCP client. The controlplane remains the
     authoritative source of which servers a session sees and can revoke
     at any time, but the agent VM does open direct sockets, so the
     controlplane must either point those URLs at its own egress proxy
     or accept that the listed URLs are reachable from the compute
     environment. Deployments that want zero agent-side MCP set this
     array to empty.
  3. **Supervisor-injected tools** — the agent supervisor
     (`overacp-agent`) passes a stdio or HTTP MCP descriptor to the
     reference loop via env var or first-line handshake. Use case:
     sandbox-local helpers (LSP, fs proxies) that should not round-trip
     through the controlplane.

  Tool names are namespaced per source (`builtin/`, `injected/`, `acp/`,
  `mcp/<server>/`). Authors who want to bypass MCP entirely can implement
  `ToolHost` directly as the escape hatch.
- **Workspace sync.** Reference impl over GCS exists in Overfolder; should it
  ship as an optional crate (`overacp-workspace-gcs`) or stay product-side?
- **Multi-agent.** Today one tunnel = one agent process. Multi-agent (a
  parent agent that spawns sub-agents on new VMs) should be expressible as
  a `ComputeBackend` call from a tool, not a special protocol method. Needs
  a worked example.
