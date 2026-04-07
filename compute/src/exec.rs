use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One-shot exec request. Mirrors Morph Cloud's `Instance.exec` shape.
///
/// `command` is **always** an argv array — never a shell string. Use
/// `["bash", "-lc", "..."]` if you need shell parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub timeout_s: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}
