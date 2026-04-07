# Loop tool architecture

How `overloop` (and any future reference agent) discovers and invokes
tools. Source of truth for the loop migration tracked under
"0.3.x" in [`TODO.md`](../../TODO.md). The wire-level method names
referenced here are defined in
[`docs/design/protocol.md`](./protocol.md).

## Goals

- Loop must accept tools from **three sources**, in a single uniform
  registry, with explicit precedence:
  1. **Built-in** — in-process Rust functions compiled into the agent
     binary (`read`, `write`, `exec`, `glob`, `grep`).
  2. **Injected by the supervisor** — `overacp-agent` hands the loop
     a list of tool descriptors at spawn time. Use case: deployment
     wants to expose a sandbox-local helper that should not round-trip
     through the controlplane.
  3. **Remote** — declared in the `initialize` response from the
     controlplane. Two transports:
     - **ACP** — the controlplane re-exposes tools through the ACP
       tunnel itself (`tools/list` / `tools/call`). The agent never
       knows what backs them.
     - **MCP** — the controlplane sends a list of MCP server
       descriptors (URL, headers, optional short-lived credentials)
       and the agent talks to them with its own MCP client.
- The current `loop/src/mcp/client.rs` keeps working — it stops being
  configured from `MCP_SERVERS` env vars and starts being configured
  from the `initialize` response.
- Tool name collisions are resolved by **namespace prefix per source**:
  `builtin/read`, `injected/<source>/<name>`, `acp/<name>`,
  `mcp/<server>/<name>`. The LLM sees the prefixed name; the registry
  routes by prefix.

## Sources in detail

### 1. Built-in (already implemented)

`loop/src/tools/builtin.rs` — five in-process tools, registered at
startup, no protocol involvement. Stays as-is.

### 2. Injected via the supervisor

`overacp-agent` is a transparent stdio bridge — it doesn't speak the
protocol — so it can't put data in the `initialize` response. Two
mechanisms:

- **Env var (preferred):** the supervisor sets
  `OVERACP_INJECTED_TOOLS` to a JSON document on spawn. Loop reads it
  on startup and registers each entry. Schema:

  ```jsonc
  {
    "stdio_mcp": [
      { "name": "lsp", "command": "/usr/bin/my-lsp-mcp", "args": [], "env": {} }
    ],
    "http_mcp": [
      { "name": "vault", "url": "http://localhost:9999", "headers": {} }
    ]
  }
  ```

- **First-line handshake (fallback):** the supervisor writes a single
  JSON line to the child's stdin before forwarding tunnel traffic.
  Loop reads it once, then enters its normal main loop. Equivalent
  payload to the env var. Useful when env vars are awkward (very
  long descriptors, secrets the supervisor doesn't want to leak via
  `/proc/<pid>/environ`).

The `AgentAdapter` trait gains a `injected_tools(&self) -> Option<InjectedTools>`
hook so deployment-specific adapters can plug values in without
touching the supervisor itself.

### 3. Remote — ACP tunnel

The `initialize` response carries a flag in `tools_config`:

```jsonc
{
  "tools_config": {
    "acp_tools_enabled": true
  }
}
```

When set, loop calls `tools/list` over the ACP tunnel after
`initialize` returns and registers every tool under the `acp/`
namespace. Tool invocations route through `tools/call` over the
tunnel. The protocol crate already defines the method-name constants
in `overacp_protocol::methods`; loop will start consuming them
instead of the hard-coded strings in `loop/src/acp.rs`.

### 4. Remote — MCP, controlplane-described

The `initialize` response also carries MCP server descriptors:

```jsonc
{
  "tools_config": {
    "mcp_servers": [
      {
        "name": "github",
        "transport": "http",
        "url": "https://mcp.example.com/github",
        "headers": { "Authorization": "Bearer <short-lived-token>" }
      },
      {
        "name": "filesystem",
        "transport": "stdio",
        "command": "/usr/bin/mcp-fs",
        "args": ["--root", "/workspace"]
      }
    ]
  }
}
```

Loop spins up one MCP client per descriptor, calls `tools/list` on
each, and registers every tool under `mcp/<name>/`. Tool invocations
go to the matching client.

The descriptors come from the controlplane, so the controlplane
remains the authoritative source of which MCP servers a session sees,
which credentials it has, and how long they live. Direct connection
from the agent VM to the listed URL is permitted — see the SPEC
revision below for the trade-off.

## Protocol crate additions

- `overacp_protocol::messages::ToolsConfig` — typed struct replacing
  the opaque `Value` field on `InitializeResponse`. Backward-compat:
  any extra keys are ignored, and an empty config means "use built-ins
  only".
- Sub-types: `AcpToolsConfig { enabled: bool }`,
  `McpServerDescriptor { name, transport, ... }`, with
  `transport: Http { url, headers } | Stdio { command, args, env }`.
- A round-trip fixture under `protocol/tests/fixtures/` capturing a
  full `tools_config`.

## Loop crate refactor

```
loop/src/tools/
  registry.rs       (existing) — dispatches by prefixed name
  builtin.rs        (existing) — in-process tools
  injected.rs       (NEW)      — reads OVERACP_INJECTED_TOOLS / handshake
  acp_remote.rs     (NEW)      — ACP-tunneled tools (uses AcpClient)
  mcp_remote.rs     (NEW)      — MCP descriptors from initialize
loop/src/mcp/
  client.rs         (existing) — gets a sibling stdio_client.rs
  stdio_client.rs   (NEW)      — JSON-RPC over child-process stdio
```

`ToolRegistry::from_initialize(...)` becomes the single
construction site: it walks the four sources in order, namespaces,
and resolves duplicates by source priority (built-in → injected →
acp → mcp; later wins is *not* the policy, earlier wins is).

The current `MCP_SERVERS` env var goes away — once injected and
remote sources land, no env-var configuration of tools remains in
the loop crate.

## SPEC revision

The SPEC's "Tool model" entry currently forbids agent-side MCP
clients. This design relaxes that to allow MCP descriptors **issued
by the controlplane**, while keeping the agent unable to learn about
MCP servers on its own. The trade-off:

- The controlplane still mints credentials, picks the server set,
  and can revoke at any time.
- The agent VM does open direct sockets to listed MCP server URLs,
  so the controlplane is responsible for either pointing those URLs
  at its own egress proxy *or* accepting that the listed URLs are
  contactable from the compute environment.
- Deployments that want strict case-A behaviour (no agent-side MCP
  at all) set the `mcp_servers` array to empty and use only ACP
  tools — the agent then has no MCP client open.

`SPEC.md` is updated in the same commit so the open-question entry
matches this document.

## Verification (when implemented)

1. `cargo test -p overloop` — registry-source-priority and namespacing tests.
2. Loop integration test that:
   a. Sets `OVERACP_INJECTED_TOOLS` to a stdio MCP descriptor pointing
      at a fake MCP echo binary.
   b. Mocks an `initialize` response with one ACP tool and one HTTP
      MCP descriptor (wiremock).
   c. Asserts `tools/list` (LLM-facing) returns built-ins +
      injected/echo + acp/foo + mcp/<name>/bar.
   d. Calls each through the registry and asserts the call lands at
      the right transport.
3. `cargo metadata -p overloop` shows no new heavy deps (reqwest
   already present; stdio MCP can use tokio's `Command`, no new crate).

## Out of scope for this milestone

- Per-tool ACL / permission prompts. Deferred to a separate
  permissions design doc.
- Tool-result streaming. Tools return a single value today; partial
  results are a separate protocol revision.
- MCP server discovery / autoconfig. Descriptors are explicitly
  provided by the controlplane.
