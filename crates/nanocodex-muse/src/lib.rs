#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

// The public lifecycle, transcripts, events, tools, and errors have one identity.
pub use nanocodex_agent::*;

/// Native Muse device OAuth, inference-key exchange, and caller-owned credentials.
#[cfg(all(feature = "openai", not(target_family = "wasm")))]
pub mod auth;

#[cfg(feature = "openai")]
mod muse;
#[cfg(feature = "openai")]
pub use muse::{Muse, MuseBuilder};
