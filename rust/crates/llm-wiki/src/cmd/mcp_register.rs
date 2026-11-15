//! `mcp-register` (spec §7.8) — thin clap→domain shim: per-client MCP
//! server registration plus the CLAUDE.md/AGENTS.md managed memory
//! block. All mechanics live in [`crate::mcp_register`].

use std::path::Path;

use serde_json::Value;

use crate::err::EngineError;
use crate::mcp_register::McpClient;

pub fn run(client: McpClient, uninstall: bool, root: &Path) -> Result<Value, EngineError> {
    crate::mcp_register::run(client, uninstall, root)
}
