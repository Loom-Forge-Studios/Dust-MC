//! The game's own rules about blocks and the things in the world.
//!
//! Entities, physics, inventory, redstone, fluids, AI and combat will live
//! here. What is here today is [`placement`]: which *state* a block takes when
//! a player puts it down; [`drops`]: what breaking one yields;
//! [`crafting`]: what a grid of items makes; and [`cooking`]: what a fire turns
//! one item into.
//!
//! Nothing in this crate knows about a socket or a save file. It takes what a
//! click said and answers with a block state, which is what lets it be tested
//! without running a server — the same argument `dust-guard` makes about the
//! reach check, and the same reason both are crates rather than functions
//! inside the session.
//!
//! [`mining`] is the other half of a break: [`drops`] says what comes out of
//! one and `mining` says how long it takes.

#![forbid(unsafe_code)]

pub mod cooking;
pub mod crafting;
pub mod cutting;
pub mod drops;
pub mod mining;
pub mod placement;
pub mod smithing;
pub mod updates;

pub use cooking::{Cooked, Cooking, Fire};
pub use crafting::{Recipe, Recipes};
pub use drops::{compile, Break, Drop, Rng, Table, Tables, Tool};
pub use mining::{tool_is_correct, Digger, Progress};
pub use placement::{replaces_beside, replaces_clicked, state_for, state_for_item, Click, Face};
pub use updates::{Reaction, Rules};
