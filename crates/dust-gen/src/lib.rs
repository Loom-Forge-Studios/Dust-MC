//! Worldgen: density functions, biome source, surface rules, aquifers,
//! carvers and features.
//!
//! All of that exists now, in the order vanilla's own pipeline runs it.
//! [`noise`] evaluates the operator's own density functions, [`biome`] answers
//! which biome a cell gets by sampling six climate values and matching them
//! against their parameter list, [`terrain`] turns `final_density` into rock
//! and water, [`aquifer`] decides what an enclosed pocket holds, [`surface`]
//! paints the dimension's surface rules over the result, [`carver`] cuts the
//! caves, and [`feature`] puts something in them. [`worldgen`] is the
//! vocabulary the whole of it was built against — which density functions
//! exist, what a noise router wires, and what shape a biome-parameter entry
//! takes.
//!
//! Two modules answer to `[worldgen.ores]` rather than to the pack.
//! [`ore_density`] is the part of ore placement that is arithmetic over
//! whatever baseline a world has, and [`vanilla_ores`] is the extracted table
//! of vanilla's own placements — one caller that happens to supply vanilla's
//! numbers.
//!
//! The two are separate on purpose. `ore_density` never reaches for a vanilla
//! constant, so it is right on a modded world as well as a vanilla one;
//! `vanilla_ores` is one caller that happens to supply vanilla's numbers. The
//! *grouping* rule that decides which knob turns which vein lives in
//! `ore_density` for the same reason and one more: `cargo xtask extract` uses
//! it too, and two implementations of a naming rule are two chances for an
//! operator's `[worldgen.ores.overrides.diamond]` to name nothing.
//!
//! Nothing Mojang's is in this crate. Every number the generator runs on
//! arrives at run time from the operator's own copy of Minecraft — decision
//! records 0006, 0007 and 0008.

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
