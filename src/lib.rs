#![deny(unsafe_code)]

pub mod app_server;
pub mod artifact;
pub mod audit;
pub mod caller_scope;
pub mod cli;
pub(crate) mod codex_socket;
pub(crate) mod collaboration;
pub mod conformance;
pub mod controller;
pub mod delegation_identity;
pub mod delegation_service;
pub mod direct_review;
pub mod direct_review_target;
pub mod domain;
pub mod engagement;
pub mod event;
pub mod external_engagement;
pub mod fault;
pub mod gateway;
pub mod gateway_event;
pub mod gateway_observation;
pub mod gateway_projection;
pub mod gateway_projection_v2;
pub mod gateway_service;
pub mod gateway_socket;
pub mod gateway_v2;
pub mod global_profile;
pub mod global_runtime;
pub mod host_invocation;
pub mod instructions;
pub(crate) mod interaction;
pub(crate) mod interaction_payload;
pub mod jcs;
pub mod ledger;
pub(crate) mod ledger_generation;
pub mod live_transport;
pub mod live_transport_mcp;
pub mod machine;
pub mod maintenance;
pub mod markdown_policy;
pub mod mcp_review;
pub mod mcp_review_server;
pub mod migration;
pub(crate) mod mutation_admission;
pub mod operation;
pub mod orchestration;
pub mod paths;
pub mod pool;
pub mod primary_bridge;
pub mod primary_tool;
pub mod process_inventory;
pub mod profile;
pub mod projection;
pub mod protocol;
pub mod providers;
pub mod provisioning;
pub mod review;
pub mod review_output;
pub mod review_recovery;
pub mod review_target;
pub mod run;
pub mod runtime;
pub mod semantic;
pub mod snapshot;
pub mod specialist;
pub(crate) mod specialist_facade;
pub mod specialist_policy;
pub mod specialist_stdio;
pub mod storage_observation;
pub mod successor_capture;
pub(crate) mod successor_interaction;
pub mod task_request;
pub mod timeline;
pub mod trusted_host;
pub mod turn;
pub mod worker;
pub mod workspace;
pub mod writer;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod darwin;

pub mod session_policy;

// Staged assignment foundation; the central scheduler owns live activation.
#[allow(dead_code)]
pub(crate) mod assignment_queue;

// Internal adapter joins are wired into native transport at the next boundary.
#[allow(dead_code)]
pub(crate) mod source_binding;

pub(crate) mod managed_specialist;

// Private consumer composition is registered by the next native delivery boundary.
#[allow(dead_code)]
pub(crate) mod consumer_bridge;
#[allow(dead_code)]
pub(crate) mod native_consumer;
#[allow(dead_code)]
pub(crate) mod podway_client;

pub(crate) mod public_resolution;
