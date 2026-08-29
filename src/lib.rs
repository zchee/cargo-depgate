//! A high-performance dependency policy enforcer and CI gatekeeper for Cargo workspaces.

pub mod cli;
pub mod config;
pub mod error;
pub mod graph;
pub mod manifest;
pub mod metadata;
pub mod pipeline;
pub mod rules;
pub mod timings;
