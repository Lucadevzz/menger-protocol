//! Menger on Lightning: a saver keeps a balance whose value is fixed in gold inside a Lightning
//! channel; a provider takes the other side. Both keep their own keys and their own bitcoin.

pub mod api;
pub mod keys;
pub mod party;
pub mod prices;
pub mod regtest;
pub mod state;
pub mod web;

pub use party::{ChainSource, Party, PartyConfig, Report};
