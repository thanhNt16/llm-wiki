//! `page-show` — frontmatter, body, fresh claim details, staleness and
//! registered deps for one derived page
//! (wiki.py:cmd_page_show 408-411 parity). Read-only.

use serde_json::Value;

use crate::err::EngineError;
use crate::store::Wiki;
use crate::synthesize;

pub struct PageShowArgs {
    /// wiki/<kind>/<slug>.md (required)
    pub artifact: String,
}

pub fn run(wiki: &Wiki, args: &PageShowArgs) -> Result<Value, EngineError> {
    synthesize::page_show(wiki, &args.artifact)
}
