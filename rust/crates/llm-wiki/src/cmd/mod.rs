//! CLI command implementations — the thin layer between clap args and
//! the domain modules. Each `run` returns the stdout JSON payload or an
//! [`crate::err::EngineError`]; `main` owns emission and exit codes.
//! Flags accepted but not yet backed by a lane return explicit
//! `Txn("not yet implemented: …")`, never a silent no-op.

pub mod build;
pub mod build_pages;
pub mod check_evidence;
pub mod compile;
pub mod compile_plan;
pub mod context_pack;
pub mod decide;
pub mod doctor;
pub mod dream;
pub mod export_okf;
pub mod graph_data;
pub mod graph_export;
pub mod jev;
pub mod ingest;
pub mod mcp;
pub mod mcp_register;
pub mod ingest_feed;
pub mod page_show;
pub mod page_targets;
pub mod query_prepare;
pub mod write_page;
pub mod record_decision;
pub mod reconcile_apply;
pub mod reconcile_prepare;
pub mod stage_candidates;
pub mod review;
pub mod ingest_git;
pub mod status;
pub mod subjects;
pub mod topics;
pub mod triage;
pub mod verify;
pub mod viz_serve;
