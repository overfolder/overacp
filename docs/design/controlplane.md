---
status: Active
---

# Controlplane

The over/ACP controlplane is the **HTTP + WebSocket service that owns
compute provisioning, agent lifecycle, and protocol routing**. It is
the centerpiece of milestones 0.4 and 0.5; the protocol crate (0.2),
the agent supervisor (0.3), and the reference loop are all designed
to terminate against it.

This doc is the source of truth for the controlplane's REST API, the
`ComputeProvider` trait, and the agent lifecycle. The wire-protocol
spec lives in [`protocol.md`](./protocol.md).

## 1. Goals

- **Bring-your-own compute.** Operators register one or more
  *compute pools*, each backed by a *provider* (Morph VM, Docker,
  Kubernetes, local process, ...). Adding a new provider is one
  trait impl plus one crate.
- **Declarative pool configuration over a REST API.** Patterned on
  Kafka Connect's connector REST surface: a pool is a name plus a
  config blob, secrets are referenced rather than embedded, and the
  same config can be POSTed against multiple environments.
- **Agents are first-class.** Listing, describing, and deleting
  agents are top-level REST verbs. Each agent record carries the
  compute pool and node ID it lives on so operators can trace
  conversations to compute.
- **Compute nodes are inspectable.** Operators can list nodes in a
  pool, describe a single node, send `exec` to it, stream its logs,
  and delete it — all via REST. The over/ACP server is the only
  process that holds provider credentials.
- **The wire protocol is unchanged.** End-user messages reach the
  agent through the existing JSON-RPC tunnel; the REST surface sits
  in front of it. Any client (web UI, CLI, Telegram bridge,
  third-party app) drives the system through HTTP.

## 2. Concepts

```
ComputeProvider  one Rust trait, several impls
       │
       ▼
ComputePool      one named instance of a provider with its config
       │
       ├──► ComputeNode (a VM/container/process spawned in this pool)
       │         │
       │         └──► overacp-agent (running inside the node)
       │                   │
       │                   └──► child agent process (overloop or other)
       │
       └──► …more nodes…

Agent            a logical conversation pinned to one ComputeNode
                 (and therefore to exactly one ComputePool)
```

- **`ComputeProvider`** is a Rust trait. Each provider type
  (`morph`, `docker`, `local-process`, `k8s`, ...) is a separate
  impl. In-tree providers ship as modules under
  `overacp-compute-core::providers::*` so the server binary picks
  them up automatically; out-of-tree providers can still live in
  their own crate and be registered at startup.
- **`ComputePool`** is a *configured instance* of a provider. The
  same provider type can back multiple pools (e.g. a `morph-prod`
  pool and a `morph-staging` pool with different API keys and
  default sizes).
- **`ComputeNode`** is a single VM/container/process. Pools own
  their nodes and are responsible for create/list/describe/delete.
- **Agent** is the user-facing concept: a conversation. The
  controlplane records which node hosts each agent so REST calls on
  `/agents/{id}` can route through to the right node.

## 3. REST API

All endpoints are served at the root (no `/api/v{n}` prefix). Stability
comes from software semver, not URL versioning — breaking REST changes
ride a major release of `overacp-server` and operators stay on the
prior version if they need the old shape.

**Authentication** splits along the agent vs. operator axis:

- **Agent-facing routes** — `/tunnel/{session_id}` (WS upgrade) and
  the REST adapters under § 3.5 (`/agents/{id}/messages`,
  `/agents/{id}/stream`, `/agents/{id}/cancel`) — use
  `Authorization: Bearer <jwt>`, the same session JWT format as the
  WebSocket tunnel (see [`protocol.md`](./protocol.md) § 2).
- **Control-plane routes** — `/compute/*` (§ 3.1–3.3) and the admin
  `/agents` lifecycle (§ 3.4) — use HTTP Basic
  (`Authorization: Basic <base64(user:pass)>`). Credentials are
  loaded once at startup from an htpasswd(5) file pointed to by the
  `OVERACP_BASIC_AUTH_FILE` env var. Only bcrypt hashes are accepted
  — generate the file with the apache2-utils `htpasswd` CLI:

  ```
  htpasswd -B -c /etc/overacp/creds alice
  ```

  If `OVERACP_BASIC_AUTH_FILE` is unset the server still boots, but
  every control-plane request returns `503 Service Unavailable` with
  a body pointing at the env var. There is no open-by-default mode.

  Because HTTP Basic carries no notion of a user UUID and the
  storage layer (and the existing JWT `Claims`) wants one, an
  optional `OVERACP_DEFAULT_USER_ID` env var supplies the UUID
  attributed to control-plane writes (create pool, create agent, …).

The `Authenticator` trait remains the extension point for swapping
either side out in production (OIDC, mTLS, API keys, …).

### 3.1 Compute providers (the plugin catalogue)

Lists provider types compiled into the running server binary. Read
only.

```
GET  /compute/providers
GET  /compute/providers/{provider_type}
POST /compute/providers/{provider_type}/config/validate
```

`POST .../config/validate` takes a candidate pool config and runs
the provider's validation hook without provisioning anything. Mirrors
Kafka Connect's `/connector-plugins/{name}/config/validate`.

### 3.2 Compute pools (declarative provider instances)

The Kafka-Connect-style surface for managing compute backends. The
server persists the pool config in its `SessionStore`-equivalent
database table.

```
GET    /compute/pools
POST   /compute/pools                       # create
GET    /compute/pools/{name}
GET    /compute/pools/{name}/config
PUT    /compute/pools/{name}/config         # replace config
DELETE /compute/pools/{name}
GET    /compute/pools/{name}/status
POST   /compute/pools/{name}/pause
POST   /compute/pools/{name}/resume
```

#### 3.2.1 Pool config blob

Modeled on Kafka Connect connector configs. Flat key/value map; the
provider class is one of the keys; everything else is provider
specific. Secret values are **references**, never inline literals
(see § 3.5).

```jsonc
// POST /compute/pools
{
  "name": "morph-prod",
  "config": {
    "provider.class": "morph",

    "morph.api_key":   "${env:MORPH_API_KEY}",
    "morph.base_url":  "https://api.morph.so",

    "default.image":     "ghcr.io/overfolder/overacp-agent:latest",
    "default.cpu":       "4",
    "default.memory_gb": "8",
    "default.disk_gb":   "20",

    "multi_agent_nodes": "false",
    "node_reuse":        "true",

    "max_nodes":  "50",       // FUTURE — not enforced in 0.4
    "idle_ttl_s": "1800"      // FUTURE — not enforced in 0.4
  }
}
```

The `provider.class` value is matched against the registered
provider types; the rest is opaque to the controlplane and validated
by the provider impl.

Two well-known boolean keys steer the agent → node mapping and are
read by the controlplane itself, not the provider:

| Key                  | Meaning                                                                 |
|----------------------|-------------------------------------------------------------------------|
| `multi_agent_nodes`  | Allow more than one agent to share a single node concurrently.          |
| `node_reuse`         | After the last agent on a node detaches, keep the node for the next agent instead of destroying it. |

Both default to the **provider's** capability (see § 4). A pool may
only **restrict** these flags relative to its provider — enabling a
flag the provider does not advertise causes `POST /compute/pools` to
fail with `422 Unprocessable Entity` and a structured error pointing
at the offending key. `max_nodes` and `idle_ttl_s` are reserved for
a future scheduling/idle-reaper layer and are not enforced in 0.4.

### 3.3 Compute nodes (instances inside a pool)

```
GET    /compute/pools/{pool}/nodes
GET    /compute/pools/{pool}/nodes/{node_id}
DELETE /compute/pools/{pool}/nodes/{node_id}
POST   /compute/pools/{pool}/nodes/{node_id}/exec
GET    /compute/pools/{pool}/nodes/{node_id}/logs       # SSE stream
```

Node creation is **not** a top-level endpoint. Nodes are spawned by
the controlplane when an agent is created (§ 3.4) — the operator
asks for "an agent on pool `morph-prod`" and the controlplane picks
or provisions a node.

`POST .../exec` mirrors **Morph Cloud's Instance.exec** API
(`POST /instance/{id}/exec`) so the Morph `ComputeProvider` impl is
a passthrough and the wire shape is familiar to operators already
running on Morph:

```jsonc
{
  "command":   ["bash", "-lc", "ls /workspace"],   // array form, no shell parsing
  "cwd":       "/workspace",                       // optional
  "env":       { "FOO": "bar" },                   // optional
  "timeout_s": 30                                  // optional
}
```

Returns:

```jsonc
{
  "stdout":    "...",
  "stderr":    "...",
  "exit_code": 0
}
```

Notes:

- `command` is an **array**, never a string. The first element is
  the program; the rest are argv. Shell parsing is the caller's
  responsibility (`["bash", "-lc", "..."]`). This matches Morph's
  array form and avoids the quoting hazards of free-form strings.
- `cwd` and `env` are optional. If omitted, the provider uses the
  node's defaults (typically `/workspace` and the agent's
  environment).
- `timeout_s` is enforced by the provider; on timeout the response
  has a non-zero `exit_code` and a structured `stderr` line
  indicating the timeout. The HTTP call still returns 200 — the
  caller distinguishes by `exit_code`.
- Streaming exec (live stdout/stderr while the command runs) is a
  future addition. v1 is one-shot. When it lands, it will reuse
  this body shape on a new endpoint
  (`POST .../exec/stream` returning SSE) so the one-shot path stays
  unchanged.
- `stdin` is **not** in v1. Morph's API doesn't carry it either.
  Adding it later is non-breaking.

### 3.4 Agents

```
GET    /agents                # list all agents the caller can see
POST   /agents                # create a new agent on a pool
GET    /agents/{id}           # describe (incl. compute node + pool)
DELETE /agents/{id}           # tear down conversation + node
GET    /agents/{id}/status
```

#### 3.4.1 Create

```jsonc
// POST /agents
{
  "pool": "morph-prod",
  "image": "overacp/loop:latest",   // optional, falls back to pool default
  "user":  "00000000-0000-0000-0000-000000000001",
  "metadata": { "tag": "prod-eu", "channel": "telegram" }
}
```

The controlplane:
1. Provisions (or reuses) a node in the named pool via the
   `ComputeProvider`.
2. Boots `overacp-agent` inside the node, passing it the JWT for
   the new conversation.
3. Records `(agent_id, pool, node_id, image, user, metadata)` in
   the agents table.
4. Returns the agent record + the freshly minted JWT (so the caller
   can hand it to whoever needs to drive the conversation).

#### 3.4.2 Describe response

```jsonc
{
  "id": "ag_01HQ...",
  "user": "00000000-...",
  "conversation_id": "00000000-...",
  "compute": {
    "provider_type": "morph",
    "pool": "morph-prod",
    "node_id": "morphvm_xyz123"
  },
  "image": "overacp/loop:latest",
  "status": "idle" | "running" | "exited" | "errored",
  "created_at": "2026-04-07T12:00:00Z",
  "metadata": { "tag": "prod-eu" }
}
```

The `compute` block is the answer to "which node is this agent
on" — it lets operators jump straight from a misbehaving agent to
the underlying VM via `/compute/pools/{pool}/nodes/{node_id}`.

#### 3.4.3 Refcount lifecycle and `POST /agents` decision tree

Each `compute_nodes` row carries an `agent_refcount INT` column that
the controlplane mutates **transactionally** with the corresponding
`agents` row. The two SessionStore primitives are
`acquire_node_for_agent` and `release_node_for_agent`; both are
atomic (single transaction) so the refcount cannot drift if the
process crashes between the agent insert and the node update.

`POST /agents` — decision tree, evaluated under a write transaction:

1. Look up the pool. Reject 404 if missing, 409 if `paused`/`errored`.
2. If `multi_agent_nodes = true` **or** `node_reuse = true`, scan
   pool nodes for a candidate to attach to:
   - `node_reuse = true, multi_agent_nodes = false`: pick any node
     with `agent_refcount = 0`.
   - `multi_agent_nodes = true`: pick any healthy node regardless of
     refcount (subject to a future per-node cap).
3. If no candidate is found, call `provider.create_node(NodeSpec)`
   with the agent's JWT and `OVERACP_*` env vars populated per
   [`protocol.md`](./protocol.md) § "Agent supervisor boot
   contract"; insert a fresh `compute_nodes` row with
   `agent_refcount = 0`.
4. Insert the `agents` row and bump the chosen node's
   `agent_refcount` by 1 in the same transaction.
5. Return the § 3.4.2 describe shape.

`DELETE /agents/{id}` — symmetric:

1. Mark the agent row deleted and decrement `agent_refcount` in one
   transaction.
2. If the new refcount is 0 **and** the pool has `node_reuse = false`,
   call `provider.delete_node()` and mark the row deleted.
3. If the new refcount is 0 **and** `node_reuse = true`, leave the
   node in place. An idle reaper (deferred, see § 8) will eventually
   collect it.

### 3.5 Sending and receiving ACP commands

Two flavours, both keyed on `agent_id`:

```
POST   /agents/{id}/messages           # enqueue a user message
GET    /agents/{id}/messages?since=…   # poll the conversation history
GET    /agents/{id}/stream             # SSE: stream/textDelta, stream/toolCall, ...
POST   /agents/{id}/cancel             # cancel the current turn
```

These are **REST adapters over the JSON-RPC verbs from the wire
protocol**. Internally:

- `POST .../messages` writes the message to the conversation table
  and emits a `session/message` notification down the agent's
  WebSocket tunnel. The agent then fetches the message body via
  `poll/newMessages` exactly as documented in
  [`protocol.md`](./protocol.md) § 3.1.
- `GET .../stream` is a Server-Sent Events feed of the agent's
  `stream/*` notifications, fanned out by the controlplane's
  in-memory broker (or Valkey for multi-node deployments).
- `POST .../cancel` injects a cancel notification.

Direct WebSocket access to `/tunnel/{session_id}` remains available
for clients that prefer to speak raw JSON-RPC, but the REST surface
is the documented client API.

### 3.6 Secret references

Inspired by Kafka Connect's `ConfigProviders`. Any string value in a
pool config of the form `${provider:path:key}` is resolved at pool
load time:

| Provider | Syntax                          | Reads from |
|---|---|---|
| `env`    | `${env:VAR_NAME}`              | process environment |
| `file`   | `${file:/path/to/file:key}`    | TOML/JSON file at path, indexed by key |
| `vault`  | `${vault:secret/db:password}`  | HashiCorp Vault (future) |

The reference list is extensible: a `ConfigProvider` trait lets
deployments add their own (AWS Secrets Manager, GCP Secret Manager,
etc.). Resolved values **never** appear in `GET .../config`
responses; the API echoes back the original `${...}` reference so
configs can round-trip safely through GitOps.

## 4. The `ComputeProvider` trait

```rust
#[async_trait]
pub trait ComputeProvider: Send + Sync {
    /// Stable identifier matched against `provider.class` in pool config.
    fn provider_type() -> &'static str where Self: Sized;

    /// Construct from a resolved config map. Called once per pool at load
    /// time. Implementations should validate eagerly.
    fn from_config(config: ResolvedConfig) -> Result<Self, ProviderError>
    where Self: Sized;

    /// Provision a new node and return its handle. The handle's ID is
    /// what the REST surface exposes as `node_id`.
    async fn create_node(&self, spec: NodeSpec) -> Result<NodeHandle, ProviderError>;

    /// List every node currently owned by this pool.
    async fn list_nodes(&self) -> Result<Vec<NodeHandle>, ProviderError>;

    /// Describe a single node — status, image, resource usage, network info.
    async fn describe_node(&self, id: &NodeId) -> Result<NodeDescription, ProviderError>;

    /// Tear down a node. Idempotent.
    async fn delete_node(&self, id: &NodeId) -> Result<(), ProviderError>;

    /// One-shot command execution. The wire shape mirrors Morph Cloud's
    /// `Instance.exec`: `command` is an argv array, `cwd` and `env` are
    /// optional, the result carries `stdout`, `stderr`, and `exit_code`.
    /// Providers that don't speak Morph's API (Docker, k8s, local)
    /// translate transparently.
    async fn exec(&self, id: &NodeId, req: ExecRequest) -> Result<ExecResult, ProviderError>;

    /// Stream the node's stdout/stderr as a tokio stream of byte chunks.
    async fn stream_logs(&self, id: &NodeId) -> Result<LogStream, ProviderError>;

    /// Pure validation hook for `POST /compute/providers/{type}/config/validate`.
    fn validate_config(config: &serde_json::Map<String, serde_json::Value>)
        -> Result<(), ConfigError>
    where Self: Sized;

    /// Whether this provider can host more than one agent on the same node
    /// at the same time. Defaults to `false`.
    fn supports_multi_agent_nodes() -> bool where Self: Sized { false }

    /// Whether nodes spawned by this provider can outlive the agent that
    /// caused their creation and be reused for the next one. Defaults to
    /// `false`.
    fn supports_node_reuse() -> bool where Self: Sized { false }
}
```

The two `supports_*` methods feed the pool config defaults in
§ 3.2.1: each pool's `multi_agent_nodes` / `node_reuse` flags
default to the provider's value and may only be set to a more
restrictive value. Provider defaults:

| `provider.class` | `supports_multi_agent_nodes` | `supports_node_reuse` |
|---|---|---|
| `local-process`  | `false` | `false` |
| `docker`         | `false` | `true`  |
| `morph`          | `false` | `true`  |
| `kubernetes`     | `true`  | `true`  |

Notes:

- `NodeSpec` carries CPU, memory, disk, image, env vars, the JWT
  for the agent, and any provider-specific overrides.
- `NodeHandle` is the small persistent identity (`NodeId` + provider
  metadata) the controlplane stores. `NodeDescription` is the rich
  view returned to operators.
- The trait is `async_trait` because most providers are HTTP API
  clients (Morph, Docker daemon, k8s); a few impls are local
  (`local-process`).
- All errors flow through `ProviderError` so the REST layer can
  translate them to consistent HTTP status codes.

## 5. Reference providers

In-tree providers live as modules under
`overacp-compute-core::providers::*`. Out-of-tree providers (with
heavy or proprietary dependencies) can ship as separate crates and
register themselves at server startup.

| Module / crate                                  | `provider.class`  | Description |
|---|---|---|
| `overacp-compute-core::providers::local`        | `local-process`   | Landed. Spawns the agent binary as a local subprocess. Zero infra; great for `cargo run --example` and CI. |
| `overacp-compute-core::providers::docker`       | `docker`          | (Planned.) Pulls an image and runs the agent inside a container via the Docker daemon. |
| `overacp-compute-morph` (separate crate)        | `morph`           | (Planned.) Lifted from Overfolder's existing `backend/src/routes/workspace.rs:125-442` Morph integration. Kept out-of-tree because of the Morph SDK dependency. |
| `overacp-compute-k8s`                           | `kubernetes`      | (Future.) Creates a Pod per agent. |
| `overacp-compute-firecracker`                   | `firecracker`     | (Future.) Bare-VMM, for the bring-your-own-cluster case. |

The server binary loads providers via dyn dispatch from a
`ProviderRegistry` populated at startup. In-tree provider modules
keep `overacp-compute-core` self-contained for the demo path; the
trait stays object-safe so out-of-tree crates can plug in without
recompiling the server.

## 6. Persistence model

The controlplane's `SessionStore` (today: a single trait covering
conversations + messages) grows three new tables:

| Table          | Columns                                                           |
|---|---|
| `compute_pools`  | `name PK, provider_type, config_json, status, created_at, updated_at` |
| `compute_nodes`  | `node_id PK, pool_name FK, status, provider_metadata, created_at, deleted_at` |
| `agents`         | `id PK, user, conversation_id, pool_name FK, node_id FK, image, status, metadata, created_at` |

Reference impls ship for in-memory + SQLite first, Postgres later.
The Overfolder impl stays in `overfolder/controlplane`.

### 6.1 Pool runtime rehydration

Pool runtimes (the live `ComputeProvider` instances and their
in-process state) are **not** persisted directly — only the pool
config row is. On every server startup the controlplane MUST
rehydrate every `compute_pools` row by re-running provider
construction (`ComputeProvider::from_config`) before the HTTP listener
is bound. This is the `AppState::bootstrap_from_store()` step in
`main.rs`. `POST /compute/pools` registers the runtime synchronously
after validation, so a successful 2xx response means the runtime is
live in memory and persisted on disk.

Rehydration failures are non-fatal: the row is preserved, the pool
is marked `errored`, and the operator can hit
`POST /compute/pools/{name}/resume` to retry. 0.4 ships with the
in-memory `SessionStore` only; SQLite persistence is a non-blocking
follow-up tracked in [`TODO.md`](../../TODO.md).

## 7. How this changes the SPEC roadmap

The current SPEC roadmap (0.4 = "lift the controlplane") is
replaced by a compute-pool-shaped roadmap. See [`SPEC.md`](../../SPEC.md)
for the full text; the short version:

- **0.4** — `overacp-server`: REST surface for pools, nodes, and
  agents; `ComputeProvider` trait; in-memory + SQLite persistence;
  `local-process` provider; the existing tunnel/dispatcher/LLM proxy
  carried over.
- **0.5** — `overacp-compute-docker`, `overacp-compute-morph`,
  end-to-end demo: one `cargo run`, then `curl POST /compute/pools`,
  then `curl POST /agents`, then `curl POST /agents/{id}/messages`.
- **0.6** — Overfolder cutover: `overfolder/controlplane` shrinks to
  the Postgres `SessionStore`, the Telegram channel, and the
  Overslash auth provider. The Morph integration leaves Overfolder
  and lands here as `overacp-compute-morph`.

## 8. Out of scope for this design

- **Multi-tenancy across pools.** A pool is opaque; cross-pool
  scheduling, fair-share, and bin-packing are a future layer.
- **Live node migration.** Killing and recreating an agent is the
  v1 answer to "this VM is unhealthy".
- **Streaming exec.** v1 `POST .../exec` is one-shot; streaming
  arrives with the websocket exec channel later.
- **Image building.** The controlplane consumes images by URL; how
  they get built (CI, Earthly, Docker buildx) is product-side.
- **Workspace sync as a controlplane responsibility.** That belongs
  to the agent supervisor; see
  [`workspace-sync.md`](./workspace-sync.md).
