# over/ACP

Remote agentic compute, factored out of [Overfolder](https://github.com/overfolder/overfolder).

A small framework for running LLM agents on remote compute behind a single
multiplexed WebSocket tunnel. See [SPEC.md](./SPEC.md) for the design.

> **Status:** early. The reference agent (`overacp-loop`) ships today; the
> protocol/server/agent crates are being extracted from Overfolder.

## Crates

- [`loop/`](./loop) — `overacp-loop`, the reference agent. A minimal agentic
  loop with built-in `read`/`write`/`exec`/`glob`/`grep` tools, optional MCP,
  and an OpenAI-compatible LLM client. Speaks the protocol on stdin/stdout.

## Build

```sh
cargo build --release
```

The reference agent is at `target/release/overacp-loop`. It expects to be
run as a child process with stdin/stdout wired to the over/ACP server (or
any compatible host). Standalone use is possible but not the primary mode.

## License

Apache-2.0.
