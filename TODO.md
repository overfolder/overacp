# over/ACP — TODO

Tracks concrete next steps. High-level roadmap lives in SPEC.md.

## 0.1 — vendor `loop`

- [x] Set up workspace `Cargo.toml` at repo root.
- [x] CI: fmt, clippy, test on stable.
- [x] Apache-2.0 LICENSE + NOTICE file at repo root.
- [x] README pointing at SPEC.md.

## 0.2 — `overacp-protocol`

- [x] Extract wire types from `overfolder/controlplane/src/{acp,session}.rs`.
- [x] Pure types, no I/O, no tokio.
- [x] Decide method naming: borrow Zed/Anthropic ACP names where they fit,
      add controlplane-only methods (quota, persistence, compute) under our
      own namespace. Document the mapping. *(see `docs/design/protocol.md`)*
- [x] JWT claims helpers.
- [x] Round-trip tests against captured fixtures.

## 0.3 — `overacp-agent`

- [x] Lift `overfolder/overlet` into the workspace.
- [x] WS client + reconnect/backoff.
- [x] Child-process supervisor + stdio bridge.
- [x] `AgentAdapter` trait so the supervised child can be any
      ACP-speaking harness.
- [x] Built-in adapters:
  - [x] `loop` — identity passthrough to `overloop`.
  - [ ] `claude-code` — spawn `agentclientprotocol/claude-agent-acp` as a
        Node subprocess. Document Node version requirement. *(deferred)*
  - [ ] `codex` — depend on `cola-io/codex-acp` (verify Apache-2.0 still
        current) OR vendor `zed-industries/codex-acp` after LICENSE check.
        *(deferred)*
- [x] Workspace sync abstracted behind a trait (optional impl).

## 0.4 — `overacp-server` (current)

- [ ] Lift generic parts of `overfolder/controlplane`: WS hub, dispatcher,
      LLM proxy, SSE dev surface.
- [ ] Define traits: `SessionStore`, `QuotaPolicy`, `ToolHost`,
      `ComputeBackend`, `Authenticator`.
- [ ] Reference impls: in-memory + SQLite for `SessionStore`; static-JWT
      `Authenticator`; no-op `QuotaPolicy`.
- [ ] OpenAI-compatible LLM proxy with auth/model-routing/metering hooks.

## 0.5 — `overacp-tools-mcp` and examples

- [ ] `overacp-tools-mcp`: `ToolHost` impl that fans out to N MCP client
      connections, namespaces tool names per server, injects per-session
      short-lived credentials from the controlplane.
- [ ] `examples/local-process` compute backend.
- [ ] `examples/docker` compute backend.
- [ ] `examples/morph` compute backend.
- [ ] End-to-end demo: clone repo, `cargo run --example local-process`, chat.

## 0.6 — Overfolder cutover

- [ ] Shrink `overfolder/controlplane` to a thin shim over `overacp-server`.
- [ ] Archive `overfolder/overloop`.

## 1.0

- [ ] Freeze wire format. Semver guarantees on `overacp-protocol`.

## Cross-cutting / open questions

- [ ] Verify `zed-industries/codex-acp` LICENSE (GitHub reports `NOASSERTION`).
- [ ] Decide whether `overacp-workspace-gcs` ships here or stays in Overfolder.
- [ ] Worked example for multi-agent (parent agent spawns sub-agent on a new
      VM via a `ComputeBackend` tool call, not a special protocol method).
- [x] Document the protocol-naming mapping table once 0.2 lands.
      *(landed in `docs/design/protocol.md`)*
