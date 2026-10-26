//! The run API. Each stage of a render is one function with typed inputs:
//!
//! * [`strip`]: render a zoom once, as an exponential-map strip on disk
//!   (resumable; the manifest is the checkpoint).
//! * [`assemble`]: make a video, or single frames, from a stored strip, with
//!   any palette, fps, zoom speed or size.
//! * [`per_frame`]: the frame-by-frame run (PNG frames, then the video).
//!
//! The command line and the web UI are both callers. Stages report what they
//! do through [`Events`] as well as the log, so a caller can follow a run
//! without parsing log lines.

// See the note in rosie_mandlebrot_core's lib.rs on MbError's size.
#![allow(clippy::result_large_err)]

pub mod assemble;
pub mod events;
pub mod per_frame;
pub mod signals;
pub mod strip;
pub mod sysstats;

pub use events::{Event, Events, JsonLines, NoEvents, PlaceEvent};
