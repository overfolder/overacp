# over/ACP — Status

**Updated:** 2026-04-06

## Current milestone

**0.1 — vendor `loop`** (in progress)

The repo currently contains a single vendored crate, `overloop`, copied
from `overfolder/overloop`. Workspace wiring is the only structural work
item at this stage. No protocol, server, or agent crates yet.

## Crates

| Crate | State |
|---|---|
| `overloop` | Vendored, builds. Reference agent. |
| `overacp-compute-core` | Landed. `ComputeProvider` trait + node/exec/log types + `${provider:path:key}` config resolver (env + file). Bundled providers: `local-process` (`providers::local`). |
| `overacp-protocol` | Not started. |
| `overacp-agent` | Not started. |
| `overacp-server` | Not started. |
| `overacp-tools-mcp` | Not started. |
| `examples/*` | Not started. |

## Decisions locked in

- **MCP injection model:** controlplane-hosted only (case A in SPEC). The
  server runs MCP clients and re-exposes tools via `ToolHost`. Child-process
  MCP injection (case B) is explicitly out of scope.
- **Foreign agent harnesses:** `overacp-agent` will host Claude Code and
  Codex CLI as child processes via existing third-party ACP adapters rather
  than reimplementing translation layers in Rust:
  - Codex → `cola-io/codex-acp` (Rust, Apache-2.0) or
    `zed-industries/codex-acp` (pending LICENSE check).
  - Claude Code → `agentclientprotocol/claude-agent-acp` (TS/Node, spawned
    as subprocess).
  - This pushes a soft constraint on `overacp-protocol`: stay close enough
    to Zed/Anthropic ACP that no middle translation layer is needed.

## Not yet decided

See TODO.md and the "Open questions" section of SPEC.md.
