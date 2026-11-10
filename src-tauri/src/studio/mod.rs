//! Everything that is not GPU work, ported from the music-studio skill v3.7:
//! layered config and the knob catalog, style / mood / workflow / lexicon libraries,
//! planning and captions, the job queue and the take library.

pub mod api;
pub mod catalog;
pub mod config;
pub mod json;
pub mod pipeline;
pub mod plan;
pub mod pyrandom;
pub mod queue;
pub mod takes;
