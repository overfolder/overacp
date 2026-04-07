# Controlplane

The over/ACP controlplane is the **HTTP + WebSocket service that owns
compute provisioning, agent lifecycle, and protocol routing**. It is
the centerpiece of milestones 0.4 and 0.5; the protocol crate (0.2),
the agent supervisor (0.3), and the reference loop are all designed
to terminate against it.

This doc is the source of truth for the controlplane's REST API, the
`ComputeProvider` trait, and the agent lifecycle. The wire-protocol
spec lives in [`protocol.md`](./protocol.md); the loop's tool
architecture is in [`loop-tools.md`](./loop-tools.md).

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
  impl, distributed as its own crate so the server binary can pick
  which to compile in.
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

All endpoints live under `/api/v1`. Authentication is via
`Authorization: Bearer <jwt>` (the same JWT format as the WebSocket
tunnel — see [`protocol.md`](./protocol.md) § 2). Admin endpoints
require an additional scope claim (`admin: true` or similar — the
exact shape is left to the `Authenticator` impl).

### 3.1 Compute providers (the plugin catalogue)

Lists provider types compiled into the running server binary. Read
only.

```
GET  /api/v1/compute/providers
GET  /api/v1/compute/providers/{provider_type}
POST /api/v1/compute/providers/{provider_type}/config/validate
```

`POST .../config/validate` takes a candidate pool config and runs
the provider's validation hook without provisioning anything. Mirrors
Kafka Connect's `/connector-plugins/{name}/config/validate`.

### 3.2 Compute pools (declarative provider instances)

The Kafka-Connect-style surface for managing compute backends. The
server persists the pool config in its `SessionStore`-equivalent
database table.

```
GET    /api/v1/compute/pools
POST   /api/v1/compute/pools                       # create
GET    /api/v1/compute/pools/{name}
GET    /api/v1/compute/pools/{name}/config
PUT    /api/v1/compute/pools/{name}/config         # replace config
DELETE /api/v1/compute/pools/{name}
GET    /api/v1/compute/pools/{name}/status
POST   /api/v1/compute/pools/{name}/pause
POST   /api/v1/compute/pools/{name}/resume
```

#### 3.2.1 Pool config blob

Modeled on Kafka Connect connector configs. Flat key/value map; the
provider class is one of the keys; everything else is provider
specific. Secret values are **references**, never inline literals
(see § 3.5).

```jsonc
// POST /api/v1/compute/pools
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

    "max_nodes":  "50",
    "idle_ttl_s": "1800"
  }
}
```

The `provider.class` value is matched against the registered
provider types; the rest is opaque to the controlplane and validated
by the provider impl.

### 3.3 Compute nodes (instances inside a pool)

```
GET    /api/v1/compute/pools/{pool}/nodes
GET    /api/v1/compute/pools/{pool}/nodes/{node_id}
DELETE /api/v1/compute/pools/{pool}/nodes/{node_id}
POST   /api/v1/compute/pools/{pool}/nodes/{node_id}/exec
GET    /api/v1/compute/pools/{pool}/nodes/{node_id}/logs       # SSE stream
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
GET    /api/v1/agents                # list all agents the caller can see
POST   /api/v1/agents                # create a new agent on a pool
GET    /api/v1/agents/{id}           # describe (incl. compute node + pool)
DELETE /api/v1/agents/{id}           # tear down conversation + node
GET    /api/v1/agents/{id}/status
```

#### 3.4.1 Create

```jsonc
// POST /api/v1/agents
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
the underlying VM via `/api/v1/compute/pools/{pool}/nodes/{node_id}`.

### 3.5 Sending and receiving ACP commands

Two flavours, both keyed on `agent_id`:

```
POST   /api/v1/agents/{id}/messages           # enqueue a user message
GET    /api/v1/agents/{id}/messages?since=…   # poll the conversation history
GET    /api/v1/agents/{id}/stream             # SSE: stream/textDelta, stream/toolCall, ...
POST   /api/v1/agents/{id}/cancel             # cancel the current turn
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
}
```

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

## 5. Reference providers (planned)

| Crate                          | `provider.class`  | Description |
|---|---|---|
| `overacp-compute-local`        | `local-process`   | Spawns `overacp-agent` as a local subprocess. Zero infra; great for `cargo run --example` and CI. |
| `overacp-compute-docker`       | `docker`          | Pulls an image and runs the agent inside a container via the Docker daemon. |
| `overacp-compute-morph`        | `morph`           | Lifted from Overfolder's existing `backend/src/routes/workspace.rs:125-442` Morph integration. |
| `overacp-compute-k8s`          | `kubernetes`      | (Future.) Creates a Pod per agent. |
| `overacp-compute-firecracker`  | `firecracker`     | (Future.) Bare-VMM, for the bring-your-own-cluster case. |

The server binary picks providers at compile time (Cargo features)
or — better — loads them via dyn dispatch from a `ProviderRegistry`
populated at startup. Each provider crate stays small (one trait
impl, its own HTTP/CLI client, and per-provider tests).

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
