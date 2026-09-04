#![deny(unsafe_code)]

pub mod app_server;
pub mod audit;
pub mod cli;
pub mod conformance;
pub mod controller;
pub mod domain;
pub mod engagement;
pub mod event;
pub mod external_engagement;
pub mod fault;
pub mod jcs;
pub mod ledger;
pub mod machine;
pub mod mcp_review;
pub mod mcp_review_server;
pub mod paths;
pub mod profile;
pub mod projection;
pub mod protocol;
pub mod providers;
pub mod review;
pub mod review_target;
pub mod run;
pub mod runtime;
pub mod semantic;
pub mod specialist;
pub mod turn;
pub mod worker;
pub mod workspace;
pub mod writer;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod darwin;
