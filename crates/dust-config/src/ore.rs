//! How common each ore is — the configuration half.
//!
//! The arithmetic that turns these numbers into placement decisions lives in
//! `dust-gen`, because that is where the vanilla baseline it modifies lives.
//! This module owns the vocabulary, the limits and the validation.
//!
//! The shape of the setting, and why:
//!
//! ```toml
//! [worldgen.ores]
//! enabled = true
//! default_frequency = 1.0
//!
//! [worldgen.ores.overrides.diamond]
//! frequency = 3.0
//! vein_size = 1.5
//! min_y = -64
//! max_y = 16
//! ```
//!
//! **Multipliers, not absolute counts.** `frequency = 3.0` means three times as
//! much diamond as the world would otherwise have had — whatever that world is.
//! An absolute count would have to be written against one specific baseline, and
//! would silently stop meaning what it said the moment a datapack such as
//! Terralith changed the baseline underneath it. A multiplier composes with the
//! datapack; a count fights it.
//!
//! **Keyed by ore, not by placement.** Vanilla generates diamond through four
//! separate placements — an ordinary one, a medium one, a large-vein one and a
//! buried one. An operator who wants more diamond means all four, and which
//! four is read out of the world's data rather than written down here; see
//! `dust-gen::vanilla_ores`. The multiplier applies to
//! every placement of the group, which preserves the *character* of the ore's
//! distribution while changing its quantity.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{ConfigSection, Finding};

/// The widest sensible frequency multiplier.
///
/// Not a technical limit — the resolver is happy above it — but past roughly
/// sixty-four times vanilla an ore stops being an ore and becomes the stone,
/// and the value is far more likely to be a typo than an intention.
pub const MAX_FREQUENCY: f64 = 64.0;

/// The widest sensible vein-size multiplier.
///
/// Vanilla's ore feature caps a vein at 64 blocks, so multipliers past eight
/// have nothing left to scale on the ores that already generate large veins.
pub const MAX_VEIN_SIZE: f64 = 8.0;

/// The smallest vein-size multiplier that still leaves a vein.
pub const MIN_VEIN_SIZE: f64 = 0.05;

/// The absolute vertical bounds of any Minecraft dimension.
///
/// A dimension's own bounds are narrower and are not known until the world
/// loads, so [`OresConfig::check`] only rejects what no dimension could ever
/// accept. The narrower check belongs to `validate_against`.
pub const WORLD_MIN_Y: i32 = -2032;
/// See [`WORLD_MIN_Y`].
pub const WORLD_MAX_Y: i32 = 2031;

/// The ore groups vanilla 1.21.1 generates.
///
/// Dust's names, not Mojang's — the group is a Dust concept that gathers the
/// several vanilla placements of one ore under one knob. Datapacks may add
/// groups beyond these, which is why the configuration accepts any name and the
/// unknown-name check happens against the loaded world's data rather than
/// against this list.
pub const VANILLA_ORE_GROUPS: &[&str] = &[
    "coal",
    "copper",
    "iron",
    "gold",
    "redstone",
    "lapis",
    "diamond",
    "emerald",
    "nether_quartz",
    "nether_gold",
    "ancient_debris",
];

/// The name of an ore group, as written in `dust.toml`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OreGroup(String);

impl OreGroup {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the name could be an identifier at all.
    ///
    /// Deliberately permissive about *which* ore it names — that is the world's
    /// question, not the parser's — and strict about the character set, so a
    /// name that will never match anything fails at the file rather than at the
    /// first chunk.
    fn is_well_formed(&self) -> bool {
        let body = self
            .0
            .split_once(':')
            .map_or(self.0.as_str(), |(_, rest)| rest);
        let namespace = self.0.split_once(':').map(|(ns, _)| ns);
        let ok = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '.' | '-'))
        };
        ok(body) && namespace.is_none_or(ok)
    }
}

impl std::fmt::Display for OreGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How common each ore is, and where it generates.
///
/// Every setting here scales what the world would have generated anyway. With
/// the defaults, generation is bit-for-bit what it would have been with this
/// feature absent — which is what lets the Phase 6 vanilla parity test run
/// against a server that has the feature compiled in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
pub struct OresConfig {
    /// Apply Dust's ore settings at all. With this off, every ore generates
    /// exactly where and as often as the world's own data says, and the settings
    /// below are ignored — which is the switch the vanilla parity test uses.
    #[config(new_chunks)]
    pub enabled: bool,

    /// Frequency multiplier for every ore group with no entry of its own.
    /// `1.0` is untouched, `2.0` is twice as much ore, `0.5` is half.
    #[config(new_chunks)]
    pub default_frequency: f64,

    /// Per-ore settings, keyed by ore group — `coal`, `iron`, `diamond` and so
    /// on. An ore with no entry here uses `default_frequency` and is otherwise
    /// left alone.
    #[config(map)]
    pub overrides: BTreeMap<OreGroup, OreOverride>,
}

impl Default for OresConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_frequency: 1.0,
            overrides: BTreeMap::new(),
        }
    }
}

/// Settings for one ore group.
///
/// Every field is optional and every omitted field means "leave this as the
/// world generates it". An entry that sets only `frequency` changes only how
/// much ore there is, not the depths it appears at or the size of its veins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
#[config(key_label = "ore group")]
pub struct OreOverride {
    /// Generate this ore at all. `false` removes it from new chunks entirely,
    /// which is not the same as `frequency = 0.0` only in that it reads as a
    /// decision rather than as an extreme value.
    #[config(new_chunks)]
    pub enabled: bool,

    /// Frequency multiplier for this ore. `1.0` is untouched, `3.0` is three
    /// times as much. Omit to use `default_frequency`.
    #[config(new_chunks)]
    pub frequency: Option<f64>,

    /// Vein-size multiplier for this ore: how many blocks a single vein tries to
    /// place. Independent of `frequency` — doubling the size at half the
    /// frequency leaves roughly the same amount of ore in bigger, rarer clumps.
    #[config(new_chunks)]
    pub vein_size: Option<f64>,

    /// Lowest Y this ore may generate at, replacing the world's own lower bound.
    /// Omit to leave the ore's natural depth range alone.
    #[config(new_chunks)]
    pub min_y: Option<i32>,

    /// Highest Y this ore may generate at, replacing the world's own upper
    /// bound. Omit to leave the ore's natural depth range alone.
    #[config(new_chunks)]
    pub max_y: Option<i32>,
}

impl Default for OreOverride {
    fn default() -> Self {
        Self {
            enabled: true,
            frequency: None,
            vein_size: None,
            min_y: None,
            max_y: None,
        }
    }
}

impl OresConfig {
    /// The frequency multiplier that applies to `group`, and whether it
    /// generates at all.
    ///
    /// This is the one place the precedence between the master switch, the
    /// default and a per-ore entry is decided, so that `dust-gen` and the
    /// documentation cannot disagree about it.
    pub fn resolve_group(&self, group: &OreGroup) -> GroupSettings {
        if !self.enabled {
            return GroupSettings::UNCHANGED;
        }
        let over = self.overrides.get(group);
        let enabled = over.is_none_or(|o| o.enabled);
        let frequency = over
            .and_then(|o| o.frequency)
            .unwrap_or(self.default_frequency);
        GroupSettings {
            enabled,
            frequency,
            vein_size: over.and_then(|o| o.vein_size).unwrap_or(1.0),
            min_y: over.and_then(|o| o.min_y),
            max_y: over.and_then(|o| o.max_y),
        }
    }

    /// Everything wrong with this section. See [`crate::DustConfig::check`].
    pub fn check(&self, path: &str, findings: &mut Vec<Finding>) {
        check_frequency(
            &format!("{path}.default_frequency"),
            self.default_frequency,
            findings,
        );

        if !self.enabled && !self.overrides.is_empty() {
            findings.push(Finding::warning(
                format!("{path}.enabled"),
                format!(
                    "is false, so the {} ore override(s) below it do nothing. \
                     Set it to true, or remove them.",
                    self.overrides.len()
                ),
            ));
        }

        for (group, over) in &self.overrides {
            let base = format!("{path}.overrides.{group}");
            if !group.is_well_formed() {
                findings.push(Finding::error(
                    &base,
                    "is not a usable ore name. Ore names are lowercase letters, \
                     digits, underscores, dots and dashes, optionally with a \
                     `namespace:` prefix.",
                ));
            }
            if let Some(f) = over.frequency {
                check_frequency(&format!("{base}.frequency"), f, findings);
            }
            if let Some(v) = over.vein_size {
                if !v.is_finite() || !(MIN_VEIN_SIZE..=MAX_VEIN_SIZE).contains(&v) {
                    findings.push(Finding::error(
                        format!("{base}.vein_size"),
                        format!("must be between {MIN_VEIN_SIZE} and {MAX_VEIN_SIZE}, got {v}"),
                    ));
                }
            }
            for (name, y) in [("min_y", over.min_y), ("max_y", over.max_y)] {
                if let Some(y) = y {
                    if !(WORLD_MIN_Y..=WORLD_MAX_Y).contains(&y) {
                        findings.push(Finding::error(
                            format!("{base}.{name}"),
                            format!("must be between {WORLD_MIN_Y} and {WORLD_MAX_Y}, got {y}"),
                        ));
                    }
                }
            }
            if let (Some(min), Some(max)) = (over.min_y, over.max_y) {
                if min > max {
                    findings.push(Finding::error(
                        format!("{base}.min_y"),
                        format!("is above max_y ({min} > {max}), which leaves no room to generate"),
                    ));
                }
            }
        }
    }

    /// Report configured ore groups the loaded world has never heard of.
    ///
    /// Split out from [`check`](Self::check) because the set of real ore groups
    /// comes from the world's data — vanilla plus whatever datapacks are
    /// installed — and is not known when the file is parsed. A misspelled ore
    /// otherwise fails silently, which is the worst outcome available: the
    /// operator sees a server that started and a setting that did nothing.
    pub fn validate_against(&self, known: &BTreeSet<OreGroup>, path: &str) -> Vec<Finding> {
        self.overrides
            .keys()
            .filter(|g| !known.contains(*g))
            .map(|g| {
                let suggestion = nearest(g, known);
                Finding::error(
                    format!("{path}.overrides.{g}"),
                    match suggestion {
                        Some(s) => {
                            format!("is not an ore this world generates. Did you mean `{s}`?")
                        }
                        None => "is not an ore this world generates.".to_owned(),
                    },
                )
            })
            .collect()
    }
}

/// The settings that apply to one ore group, after precedence is applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupSettings {
    pub enabled: bool,
    pub frequency: f64,
    pub vein_size: f64,
    pub min_y: Option<i32>,
    pub max_y: Option<i32>,
}

impl GroupSettings {
    /// What every ore resolves to when the feature changes nothing.
    pub const UNCHANGED: Self = Self {
        enabled: true,
        frequency: 1.0,
        vein_size: 1.0,
        min_y: None,
        max_y: None,
    };

    /// Whether these settings would leave generation exactly as it was.
    pub fn is_identity(&self) -> bool {
        *self == Self::UNCHANGED
    }
}

fn check_frequency(path: &str, value: f64, findings: &mut Vec<Finding>) {
    if !value.is_finite() || !(0.0..=MAX_FREQUENCY).contains(&value) {
        findings.push(Finding::error(
            path,
            format!("must be between 0.0 and {MAX_FREQUENCY}, got {value}"),
        ));
    }
}

/// The closest known name by edit distance, when one is close enough to be
/// worth suggesting.
fn nearest<'a>(target: &OreGroup, known: &'a BTreeSet<OreGroup>) -> Option<&'a OreGroup> {
    known
        .iter()
        .map(|k| (edit_distance(target.as_str(), k.as_str()), k))
        .filter(|(d, _)| *d <= 3)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str) -> OreGroup {
        OreGroup::new(name)
    }

    #[test]
    fn defaults_change_nothing() {
        let cfg = OresConfig::default();
        for name in VANILLA_ORE_GROUPS {
            assert!(
                cfg.resolve_group(&group(name)).is_identity(),
                "{name} must be untouched by default"
            );
        }
    }

    #[test]
    fn the_master_switch_beats_every_override() {
        let mut cfg = OresConfig {
            enabled: false,
            default_frequency: 9.0,
            ..Default::default()
        };
        cfg.overrides.insert(
            group("diamond"),
            OreOverride {
                frequency: Some(50.0),
                enabled: false,
                ..Default::default()
            },
        );
        assert!(cfg.resolve_group(&group("diamond")).is_identity());
    }

    #[test]
    fn an_override_beats_the_default() {
        let mut cfg = OresConfig {
            default_frequency: 2.0,
            ..Default::default()
        };
        cfg.overrides.insert(
            group("diamond"),
            OreOverride {
                frequency: Some(5.0),
                ..Default::default()
            },
        );
        assert_eq!(cfg.resolve_group(&group("diamond")).frequency, 5.0);
        assert_eq!(cfg.resolve_group(&group("iron")).frequency, 2.0);
    }

    #[test]
    fn a_partial_override_leaves_the_rest_of_the_ore_alone() {
        let mut cfg = OresConfig::default();
        cfg.overrides.insert(
            group("iron"),
            OreOverride {
                min_y: Some(-32),
                ..Default::default()
            },
        );
        let settings = cfg.resolve_group(&group("iron"));
        assert_eq!(settings.min_y, Some(-32));
        assert_eq!(settings.max_y, None, "an unset bound stays the world's own");
        assert_eq!(settings.frequency, 1.0);
        assert_eq!(settings.vein_size, 1.0);
    }

    #[test]
    fn out_of_range_values_are_named_individually() {
        let mut cfg = OresConfig {
            default_frequency: -1.0,
            ..Default::default()
        };
        cfg.overrides.insert(
            group("diamond"),
            OreOverride {
                vein_size: Some(99.0),
                min_y: Some(40),
                max_y: Some(10),
                ..Default::default()
            },
        );
        let mut findings = Vec::new();
        cfg.check("worldgen.ores", &mut findings);
        let paths: Vec<&str> = findings.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.contains(&"worldgen.ores.default_frequency"),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"worldgen.ores.overrides.diamond.vein_size"),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"worldgen.ores.overrides.diamond.min_y"),
            "{paths:?}"
        );
    }

    #[test]
    fn a_misspelled_ore_is_reported_with_a_suggestion() {
        let known: BTreeSet<OreGroup> = VANILLA_ORE_GROUPS.iter().map(|g| group(g)).collect();
        let mut cfg = OresConfig::default();
        cfg.overrides
            .insert(group("diamonds"), OreOverride::default());
        let findings = cfg.validate_against(&known, "worldgen.ores");
        assert_eq!(findings.len(), 1);
        assert!(
            findings[0].message.contains("`diamond`"),
            "{}",
            findings[0].message
        );
    }

    #[test]
    fn a_datapack_ore_is_not_reported_when_the_world_has_it() {
        let mut known: BTreeSet<OreGroup> = VANILLA_ORE_GROUPS.iter().map(|g| group(g)).collect();
        known.insert(group("spelunkery:rock_salt"));
        let mut cfg = OresConfig::default();
        cfg.overrides
            .insert(group("spelunkery:rock_salt"), OreOverride::default());
        assert!(cfg.validate_against(&known, "worldgen.ores").is_empty());
    }

    // What these tests do not catch: nothing here proves an ore actually
    // generates. Every assertion above is about precedence and validation, and
    // `dust_gen::ore_density` is where the numbers become placements —
    // `dust_gen::feature` is where the placements become blocks, and its own
    // tests are the ones that dig a chunk up and count. The test that would
    // catch generation being wrong seed for seed is the Phase 6 differential
    // against a real vanilla server, and that still does not exist; what does
    // exist is `cargo xtask harness worldgen`, which scores Dust's own chunks
    // against Minecraft's and says how far apart the ore is.
}
