//! The costs wire contract (`MeasuredCosts.swift`) lives in core (`crates/core/src/costs.rs`)
//! — the indexer tallies it and `speed --model` writes it. Re-exported here so the daemons'
//! `services::costs` spelling keeps working, as it has since the estimate stage.

pub use digup_core::costs::{enough, Costs, MeasuredCosts, Speed, Tally, KIND_FIELDS};
