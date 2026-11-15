//! `viz-serve` — serve the visualizer on 127.0.0.1 with SSE live reload
//! (wiki.py:cmd_viz_serve 671-676 parity). Delegates to
//! [`vizserve::serve`], which prints `{"serving","ui"}` itself and then
//! blocks — so this `run` never returns `Ok`; only pre-bind failures
//! surface as `EngineError`.

use std::path::PathBuf;

use serde_json::Value;

use crate::err::EngineError;
use crate::store::Wiki;
use crate::vizserve;

pub struct VizServeArgs {
    /// first candidate port (scan covers port..=port+20)
    pub port: u16,
    /// do not open the browser
    pub no_open: bool,
    /// prebuilt UI dist dir (default: auto-detect; WIKI_UI_DIST env)
    pub ui_dist: Option<PathBuf>,
}

pub fn run(wiki: &Wiki, args: &VizServeArgs) -> Result<Value, EngineError> {
    // serve() takes ownership of the Wiki — rebuild an equivalent one
    // (same root; carries the --allow-repair flag over).
    let owned = Wiki {
        root: wiki.root.clone(),
        dot: wiki.dot.clone(),
        allow_repair: wiki.allow_repair,
    };
    vizserve::serve(owned, args.port, args.ui_dist.as_deref(), args.no_open)
}
