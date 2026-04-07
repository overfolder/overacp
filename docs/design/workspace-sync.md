# Workspace sync

How the working directory of an agent session is hydrated before the
agent boots and persisted after it exits, in a way that is fully
configurable per deployment without baking any one storage backend
into the core crates.

## 1. Where it lives in the architecture

Workspace sync is **the agent supervisor's job**, not the
controlplane's. The supervisor (`overacp-agent`) already runs inside
the compute environment, holds the session JWT, and brackets the
child agent's lifetime — that is exactly when sync needs to happen.
The controlplane only tells the supervisor *which* sync configuration
to use; it never moves bytes itself.

This is a deliberate split:

- **Controlplane** is small and stateless about workspaces. It
  records the storage descriptor on the agent record, the same way
  it records the compute pool, and serves it back over REST.
- **Supervisor** owns the actual transfer. It calls `pull()` before
  spawning the child, `push()` after the child exits. Both are
  trait methods so the supervisor doesn't grow per-backend
  dependencies.
- **Backends** ship as separate crates that implement the
  `WorkspaceSync` trait. Adding GCS, S3, rclone, or rsync is one
  crate; the core supervisor binary picks which to compile in.

The trait already exists in the agent crate as
`overacp_agent::workspace::WorkspaceSync` with a `NoopSync` default
impl. This document specifies what the configurable real impls look
like.

## 2. The trait

```rust
#[allow(async_fn_in_trait)]
pub trait WorkspaceSync {
    /// Hydrate the workspace from the external store.
    /// Called once before the child agent is spawned.
    async fn pull(&self) -> anyhow::Result<()>;

    /// Persist the workspace back to the external store.
    /// Called once after the bridge exits.
    async fn push(&self) -> anyhow::Result<()>;
}
```

This is the boundary. Everything below — manifest format, retry
policy, conflict resolution — is an implementation detail of the
backend crate.

## 3. Configuration model

The controlplane stores a workspace-sync descriptor on every agent
record. It is a small typed enum carried in the agent's create-time
`metadata` (or, for stricter typing, a dedicated column):

```jsonc
{
  "workspace_sync": {
    "backend": "gcs",
    "config": {
      "bucket":      "my-overacp-workspaces",
      "prefix":      "user-${user_id}/conv-${conv_id}",
      "credentials": "${env:GCS_BEARER_TOKEN}"
    }
  }
}
```

The `backend` key picks which `WorkspaceSync` impl to instantiate.
The `config` blob is opaque to the controlplane and validated by
the backend crate. Secret references use the same `${provider:...}`
syntax as compute pool configs (see
[`controlplane.md`](./controlplane.md) § 3.6) so the same secret
provider machinery serves both surfaces.

When the controlplane spawns an agent, it passes the resolved
descriptor to the supervisor as either:

- An env var (`OVERACP_WORKSPACE_SYNC` set to the JSON blob), or
- A field in the agent's `initialize` response (so the supervisor
  receives the descriptor over the tunnel after auth).

The env-var path is preferred because it lets the supervisor `pull()`
*before* opening the WebSocket — the workspace is ready by the time
the child agent process needs it.

## 4. Backend crate convention

Each backend ships as its own crate so the core agent binary doesn't
pull in cloud SDKs by default:

| Crate                          | `backend` value | Reads / writes      |
|---|---|---|
| `overacp-workspace-noop`       | `noop`         | Nothing. Default; useful for local development against a bind-mounted workspace. |
| `overacp-workspace-gcs`        | `gcs`          | Google Cloud Storage object prefix. Lifted from Overfolder's planned implementation. |
| `overacp-workspace-s3`         | `s3`           | S3-compatible object store (AWS S3, MinIO, R2). |
| `overacp-workspace-rclone`     | `rclone`       | Whatever rclone supports — wraps the rclone CLI. |
| `overacp-workspace-rsync`      | `rsync`        | SSH-reachable rsync target. |
| `overacp-workspace-restic`     | `restic`       | restic repository, deduplicated and encrypted. |

The supervisor's `WorkspaceSyncRegistry` is populated at startup
from compiled-in features or from a small dispatch table the binary
constructs at `main()`. New backends do not require touching the
agent core.

### 4.1 Algorithm freedom

The trait deliberately says nothing about *how* sync happens. The
GCS backend may use a manifest diff and parallel uploads; the rsync
backend may shell out to `rsync -avz`; the noop backend does
nothing. The supervisor only sees `pull()` and `push()`.

This is the right place to put per-backend complexity (manifest
format, hashing, exclusion globs, conflict policy) because it stays
out of the agent core and out of the controlplane.

## 5. Lifecycle in the supervisor

```rust
async fn run_with<A: AgentAdapter, S: WorkspaceSync>(
    config: Config, adapter: A, sync: S,
) -> Result<()> {
    sync.pull().await?;                    // 1. hydrate workspace
    let claims = peek_claims_unverified(&config.token)?;
    let (ws_read, ws_sink) =
        connect_with_retry(&config.tunnel_url(...), &config.token).await;
    let mut proc = spawn(adapter.command())?;   // 2. spawn child
    let exit = run_bridge(ws_read, ws_sink, proc.stdin, proc.stdout).await;
    /* reap child */
    sync.push().await?;                    // 3. persist workspace
    Ok(())
}
```

This is exactly the shape the supervisor already has in
`agent/src/run.rs`. The only thing missing is **the backend
selection**, which today is hard-coded to `NoopSync` and needs to
become a function of the runtime config.

## 6. Failure handling

- **`pull` failure** is fatal. The supervisor exits non-zero before
  spawning the child. The controlplane sees the agent transition
  to `errored` and surfaces the reason.
- **`push` failure** is logged and the supervisor still exits zero.
  The user already saw the conversation results over SSE; losing
  the workspace persist is bad but recoverable on the next run if
  the backend supports diffing. Backends that want stricter
  semantics can wrap `push` in their own retry loop.

## 7. What stays out of this design

- **Real-time sync.** Workspace changes during a turn are not
  pushed live. If you need that, build a different abstraction;
  this design is for the bracket-the-conversation case.
- **Per-file ACLs / encryption.** Backend-specific.
- **Cross-conversation sharing.** Two agents on the same workspace
  is undefined; deployments wanting multi-agent shared scratch
  should run them on the same compute node and use a shared
  filesystem mount.

## 8. Migration path

1. Land the `WorkspaceSyncRegistry` and the env-var dispatch in the
   agent crate (small refactor of `agent/src/run.rs` to read the
   descriptor).
2. Ship `overacp-workspace-noop` and `overacp-workspace-gcs` first
   (lifts Overfolder's planned GCS work directly).
3. Add `overacp-workspace-s3` and `overacp-workspace-rclone` as
   community-friendly defaults.
4. Drop the `OVERACP_WORKSPACE_SYNC` env-var fallback once the
   controlplane is wired to inject the descriptor over the tunnel.
