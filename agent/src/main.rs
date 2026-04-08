//! `overacp-agent` binary entry point.
//!
//! The supervisor is configured exclusively through environment
//! variables (see `docs/design/protocol.md` § 2.4). The only CLI
//! surface is `--help` / `--version`.

use std::env;
use std::process::ExitCode;

use overacp_agent::BootConfig;

const HELP: &str = "\
overacp-agent — over/ACP supervisor

USAGE:
    overacp-agent [--help] [--version]

The supervisor is configured exclusively through environment
variables. There are no positional arguments and no config file.

REQUIRED:
    OVERACP_TUNNEL_URL   Full WebSocket URL (wss://host/tunnel/<conv>)
    OVERACP_JWT          Bearer token for the tunnel and LLM proxy
    OVERACP_AGENT_ID     Controlplane agents.id (UUID)

OPTIONAL:
    OVERACP_ADAPTER              Adapter to load (default: loop)
    OVERACP_WORKSPACE_DIR        Child working directory (default: launch CWD)
    OVERACP_RECONNECT_BACKOFF_MS Test override for backoff base

Any non-OVERACP_* env vars are forwarded verbatim to the child adapter.
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if let Some(a) = args.first() {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("overacp-agent {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("error: unexpected argument `{other}`. See --help.");
                return ExitCode::from(2);
            }
        }
    }

    match BootConfig::from_process_env() {
        Ok(cfg) => {
            // 0.4 milestone only ratifies the boot contract; the
            // supervisor loop lands in a follow-up commit.
            eprintln!(
                "overacp-agent: boot config parsed (agent_id={}, adapter={}, workspace={})",
                cfg.agent_id,
                cfg.adapter,
                cfg.workspace_dir.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("overacp-agent: {e}");
            ExitCode::from(1)
        }
    }
}
