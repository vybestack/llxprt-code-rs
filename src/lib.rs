//! llxprt-code-rs: a headless, process-per-request coding agent built on SerdesAI.

#![deny(rustdoc::broken_intra_doc_links)]

pub mod adapter;
pub mod agent;
pub mod cli;
pub mod context_eval;
pub mod context_kernel;
pub mod envelope;
pub mod grade;
pub mod harness;
pub mod memory_profile;
pub mod model;
mod model_api;
pub mod process;
pub mod profile;
pub mod redact;
mod safe_file;
pub mod session;
pub mod tools;

pub mod context_ingress;
pub mod context_policy;
pub mod context_store;
pub mod context_txn;
