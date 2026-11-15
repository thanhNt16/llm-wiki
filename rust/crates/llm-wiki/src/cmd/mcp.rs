//! `llm-wiki mcp` — run the MCP stdio server (spec §7). Thin cmd-layer
//! wrapper over [`crate::mcp::serve`]; the subcommand takes only the
//! global `--root` flag and never returns — stdin EOF exits 0.

use std::path::PathBuf;

/// `flag_root` is the raw `--root` flag (the first §7.3 rung); the rest
/// of the resolution chain lives in the server session.
pub fn run(flag_root: Option<PathBuf>) -> ! {
    crate::mcp::serve(flag_root)
}
