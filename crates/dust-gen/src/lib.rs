//! Worldgen: density functions, biome source, surface rules, carvers, features,
//! structures.
//!
//! Almost none of that exists yet. What does exist is [`ore_density`], the
//! part of ore placement that is arithmetic over the vanilla baseline rather
//! than generation itself, and which is therefore buildable and testable before
//! the engine underneath it is written — and [`vanilla_ores`], the extracted
//! baseline it is arithmetic *over*.
//!
//! The newest piece is [`biome`], which answers "which biome is this cell" the
//! way Minecraft answers it: sample six climate values with [`noise`], and
//! match them against the parameter list the operator's own copy of Minecraft
//! carries. [`worldgen`] is the vocabulary those two were built against —
//! which density functions exist, what a noise router wires, and what shape a
//! biome-parameter entry takes.
//!
//! The two are separate on purpose. `ore_density` never reaches for a vanilla
//! constant, so it is right on a modded world as well as a vanilla one;
//! `vanilla_ores` is one caller that happens to supply vanilla's numbers.

pub mod aquifer;
pub mod biome;
pub mod carver;
pub mod feature;
pub mod generated;
pub mod noise;
pub mod ore_density;
pub mod surface;
pub mod terrain;
pub mod vanilla_ores;
pub mod worldgen;
