# CLAUDE.md

Project orientation for Claude Code sessions working in this repo.

## What this is

over/ACP — a small framework for running LLM agents on remote compute
behind a single multiplexed WebSocket tunnel. Extracted from
Overfolder. The high-level design and milestone roadmap live in
[`SPEC.md`](./SPEC.md); current status is in [`STATUS.md`](./STATUS.md);
concrete next steps in [`TODO.md`](./TODO.md).

## Design docs

Authoritative subsystem specs live under [`docs/design/`](./docs/design).
Always read the relevant design doc before changing code in the
matching crate.

- [`docs/design/INDEX.md`](./docs/design/INDEX.md) — index of all
  design docs.
- [`docs/design/protocol.md`](./docs/design/protocol.md) — wire
  protocol spec. Source of truth for the `overacp-protocol` crate.

If the doc and the code disagree, the doc is wrong — update it as
part of the same change.

## Workspace layout

Authoritative state lives in [`STATUS.md`](./STATUS.md). At time of
writing the only landed crate is:

- `loop/` — `overloop`, the reference agent (vendored). Speaks
  over/ACP on stdio.

Planned crates per the SPEC roadmap:

- `protocol/` — `overacp-protocol`, pure wire types (no I/O).
- `agent/` — `overacp-agent`, the WS supervisor that connects to the
  server and bridges stdio to a child agent process.

Check `STATUS.md` before assuming a crate exists.

## House rules (enforced by clippy)

- `absolute-paths-max-segments = 2` — bring paths into scope with
  `use` rather than writing 3-segment call sites.
- `too-many-lines-threshold = 500` — keep functions under 500 lines.
- `module_inception` denied.

These are workspace-level lints; new crates inherit them via
`[lints] workspace = true`.
