//! Trackside domain model and data store.
//!
//! Everything here is deliberately odds-free: the model has no prices, no markets and no
//! bookmakers, so nothing downstream can surface wagering content. Facts carry a
//! `source` so answers can be attributed ("According to Racing Australia…").

pub mod model;
pub mod store;

pub use model::*;
pub use store::{
    horse_key, looks_like_track_code, norm, venue_matches, Fixture, FixtureStore, Store,
};
