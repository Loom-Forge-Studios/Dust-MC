//! Applies `[worldgen.ores]` to the ore placements a world actually has.
//!
//! # Where the baseline comes from
//!
//! Nothing in this module knows how much diamond vanilla generates, and that is
//! deliberate twice over.
//!
//! The first reason is licensing. Mojang's data may not be redistributed, so
//! the vanilla placement values reach Dust through `xtask extract` running
//! against a server jar on the operator's own machine, never as a table typed
//! into this repository. See `Code Provenance.md`.
//!
//! The second reason is that the baseline is not a constant. A world running
//! Terralith has different ore placements from a vanilla world, and an operator
//! who asks for twice as much iron means twice as much of *their* world's iron.
//! A resolver that knew vanilla's numbers would quietly be wrong on every
//! modded world, and would be right in exactly the case that needs it least.
//!
//! So: [`Baseline`] is whatever the loaded world says, and this module scales
//! it.
//!
//! [`crate::vanilla_ores`] is one caller that happens to supply vanilla's, from
//! a table `cargo xtask extract` produced by reading a server jar. Nothing here
//! reaches for it; the dependency runs one way only, which is the whole point.
//!
//! # The identity property
//!
//! With default settings, [`resolve`] returns the baseline unchanged — not
//! approximately, exactly. That is what allows the Phase 6 seed-for-seed parity
//! test to run against a Dust that has this feature compiled in. It is asserted
//! in the tests below over invented placements, and again in
//! [`crate::vanilla_ores`] over vanilla's real ones. Any change here that breaks
//! it is a change that breaks vanilla parity.

use std::collections::{BTreeMap, BTreeSet};

use dust_config::ore::{OreGroup, OresConfig};

/// The vertical span an ore may generate in, inclusive at both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeightRange {
    pub min_y: i32,
    pub max_y: i32,
}

impl HeightRange {
    pub fn new(min_y: i32, max_y: i32) -> Self {
        Self { min_y, max_y }
    }

    /// Whether this range has any room in it at all.
    pub fn is_empty(self) -> bool {
        self.min_y > self.max_y
    }
}

/// How often a placement is attempted, as the world's data expresses it.
///
/// Vanilla writes this two ways — a count of attempts per chunk, or a rarity
/// filter meaning "one attempt in one chunk out of N" — and both have to
/// survive being multiplied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Attempts {
    /// `n` attempts in every chunk.
    PerChunk(u32),
    /// One attempt in one chunk out of `one_in`.
    RarityFilter { one_in: u32 },
}

impl Attempts {
    /// Attempts per chunk on average. This is the quantity a frequency
    /// multiplier multiplies, and expressing both forms as one number is what
    /// lets a single rule cover them.
    pub fn expected_per_chunk(self) -> f64 {
        match self {
            Self::PerChunk(n) => f64::from(n),
            Self::RarityFilter { one_in } => 1.0 / f64::from(one_in.max(1)),
        }
    }
}

/// One ore placement as the loaded world defines it, before Dust touches it.
#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    /// The placed feature's identifier, e.g. `minecraft:ore_diamond_buried`.
    pub id: String,
    /// The ore group this placement belongs to — the knob an operator turns.
    pub group: OreGroup,
    pub attempts: Attempts,
    /// Blocks a single vein tries to place.
    pub vein_size: u32,
    pub height: HeightRange,
}

/// A placement after `[worldgen.ores]` has been applied to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// `false` when the ore is switched off. The generator skips it entirely
    /// rather than placing zero of it, so no random numbers are drawn for it.
    pub generate: bool,
    /// Attempts made in every chunk.
    pub attempts_per_chunk: u32,
    /// Probability of one further attempt, in `0.0..1.0`.
    ///
    /// This is what carries a fractional multiplier. It is also exactly what a
    /// vanilla rarity filter already is — one attempt with probability `1/N` —
    /// so the two forms collapse into one representation instead of the
    /// generator having to handle both.
    pub extra_attempt_chance: f64,
    pub vein_size: u32,
    pub height: HeightRange,
}

impl Resolved {
    /// Attempts per chunk on average, for reporting and for tests.
    pub fn expected_attempts_per_chunk(&self) -> f64 {
        if self.generate {
            f64::from(self.attempts_per_chunk) + self.extra_attempt_chance
        } else {
            0.0
        }
    }
}

/// Vanilla's ceiling on how many blocks one ore vein places.
///
/// Scaling past it silently produces a vein the feature cannot make, so the
/// multiplier is clamped here and the clamp is reported by
/// [`resolve_reporting`] rather than happening in silence.
pub const MAX_VEIN_SIZE: u32 = 64;

/// Something that happened during resolution that an operator should know
/// about, because it means the world will not do quite what the file asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// The requested vein size was above what the ore feature can place.
    VeinSizeClamped {
        id: String,
        requested: u32,
        used: u32,
    },
    /// The configured height bounds left no room, so the ore cannot generate.
    HeightRangeEmpty { id: String, min_y: i32, max_y: i32 },
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VeinSizeClamped {
                id,
                requested,
                used,
            } => write!(
                f,
                "{id}: vein size {requested} is above the {MAX_VEIN_SIZE}-block maximum, \
                 generating {used} instead"
            ),
            Self::HeightRangeEmpty { id, min_y, max_y } => write!(
                f,
                "{id}: min_y {min_y} is above max_y {max_y} for this placement, so it will \
                 not generate at all"
            ),
        }
    }
}

/// Apply the configuration to one placement.
pub fn resolve(baseline: &Baseline, config: &OresConfig) -> Resolved {
    resolve_reporting(baseline, config).0
}

/// [`resolve`], plus anything worth telling the operator.
pub fn resolve_reporting(baseline: &Baseline, config: &OresConfig) -> (Resolved, Vec<Note>) {
    let settings = config.resolve_group(&baseline.group);
    let mut notes = Vec::new();

    // The fast, and much the most common, path. Written as an early return
    // rather than as arithmetic that happens to come out the same, because
    // "happens to come out the same" is a floating-point claim and this one has
    // to be exact — vanilla parity depends on it.
    if settings.is_identity() {
        return (identity(baseline), notes);
    }

    if !settings.enabled || settings.frequency <= 0.0 {
        return (
            Resolved {
                generate: false,
                attempts_per_chunk: 0,
                extra_attempt_chance: 0.0,
                vein_size: baseline.vein_size,
                height: baseline.height,
            },
            notes,
        );
    }

    let expected = baseline.attempts.expected_per_chunk() * settings.frequency;
    let whole = expected.floor();
    let attempts_per_chunk = whole.min(f64::from(u32::MAX)) as u32;
    let extra_attempt_chance = (expected - whole).clamp(0.0, 1.0);

    let scaled = (f64::from(baseline.vein_size) * settings.vein_size).round();
    let requested = scaled.clamp(1.0, f64::from(u32::MAX)) as u32;
    let vein_size = requested.min(MAX_VEIN_SIZE);
    if requested > vein_size {
        notes.push(Note::VeinSizeClamped {
            id: baseline.id.clone(),
            requested,
            used: vein_size,
        });
    }

    let height = HeightRange {
        min_y: settings.min_y.unwrap_or(baseline.height.min_y),
        max_y: settings.max_y.unwrap_or(baseline.height.max_y),
    };

    // An empty range is reachable from a configuration that validated fine:
    // `min_y = 100` is a legal value, and it is above the top of the range
    // vanilla gives diamond. The config parser cannot know that, because it has
    // not seen the world's data yet. This is where it becomes knowable, so this
    // is where it gets said.
    if height.is_empty() {
        notes.push(Note::HeightRangeEmpty {
            id: baseline.id.clone(),
            min_y: height.min_y,
            max_y: height.max_y,
        });
        return (
            Resolved {
                generate: false,
                attempts_per_chunk: 0,
                extra_attempt_chance: 0.0,
                vein_size,
                height,
            },
            notes,
        );
    }

    (
        Resolved {
            generate: true,
            attempts_per_chunk,
            extra_attempt_chance,
            vein_size,
            height,
        },
        notes,
    )
}

/// The baseline, expressed as a [`Resolved`], with nothing changed.
fn identity(baseline: &Baseline) -> Resolved {
    let (attempts_per_chunk, extra_attempt_chance) = match baseline.attempts {
        Attempts::PerChunk(n) => (n, 0.0),
        Attempts::RarityFilter { one_in } => (0, 1.0 / f64::from(one_in.max(1))),
    };
    Resolved {
        generate: true,
        attempts_per_chunk,
        extra_attempt_chance,
        vein_size: baseline.vein_size,
        height: baseline.height,
    }
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

/// One ore group as a world's own data defines it: a knob, and the placements
/// it turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: OreGroup,
    /// Every block state the group's placements put down, sorted and deduped.
    /// The name was derived from these, so the reason for it is on the page.
    pub targets: Vec<String>,
    /// Indices into the slice [`group`] was given, ascending.
    pub placements: Vec<usize>,
}

/// What [`group`] made of a world's ore placements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grouping {
    /// The groups, ascending by name.
    pub groups: Vec<Group>,
    /// The group each placement landed in, as an index into `groups`. `None`
    /// for a placement whose blocks yield no usable name — reported rather
    /// than dropped, because an ore with no knob is a thing an operator should
    /// hear about rather than discover.
    pub of_placement: Vec<Option<usize>>,
    /// Placements that share *some* but not all of their target blocks with
    /// another placement in the same group. See [`group`].
    pub overlapping: Vec<usize>,
}

/// Gather ore placements into the groups D6 keys its knob by, from the block
/// states they place and from nothing else.
///
/// Two placements are the same ore when they put down a block in common, taken
/// transitively — which is what makes vanilla's four diamond placements one
/// `diamond`. The rule is the data's and not a table's, so it is right on a
/// datapack world as well as a vanilla one, and it lives here rather than in
/// the extractor because both the extractor and the generator have to agree
/// about which knob turns which vein. Two implementations of a naming rule are
/// two chances for `[worldgen.ores.overrides.diamond]` to name nothing.
///
/// `placed[i]` is the block states placement `i` puts down; order within it
/// does not matter.
///
/// **`overlapping` is reported rather than assumed away.** Every pair of target
/// sets being identical or disjoint is a property of 1.21.1, not of the format:
/// a datapack could place copper and gold from one feature and merge two groups
/// an operator thinks of as separate. The day that stops being true should be a
/// line of output, not a surprise in somebody's world.
pub fn group(placed: &[Vec<String>]) -> Grouping {
    let mut parent: Vec<usize> = (0..placed.len()).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let mut owner: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, targets) in placed.iter().enumerate() {
        for target in targets {
            match owner.get(target.as_str()) {
                Some(&other) => {
                    let (a, b) = (find(&mut parent, index), find(&mut parent, other));
                    if a != b {
                        parent[a] = b;
                    }
                }
                None => {
                    owner.insert(target.as_str(), index);
                }
            }
        }
    }

    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..placed.len() {
        let root = find(&mut parent, index);
        members.entry(root).or_default().push(index);
    }

    let mut overlapping = Vec::new();
    let mut groups = Vec::new();
    let mut of_placement = vec![None; placed.len()];
    for indices in members.values() {
        let first: BTreeSet<&String> = placed[indices[0]].iter().collect();
        if indices
            .iter()
            .any(|&i| placed[i].iter().collect::<BTreeSet<_>>() != first)
        {
            overlapping.extend(indices.iter().copied());
        }
        let mut targets: Vec<String> = indices
            .iter()
            .flat_map(|&i| placed[i].iter().cloned())
            .collect();
        targets.sort();
        targets.dedup();
        if let Some(name) = group_name(&targets) {
            groups.push(Group {
                name: OreGroup::new(name),
                targets,
                placements: indices.clone(),
            });
        }
    }
    groups.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
    for (slot, group) in groups.iter().enumerate() {
        for &index in &group.placements {
            of_placement[index] = Some(slot);
        }
    }
    overlapping.sort_unstable();
    Grouping {
        groups,
        of_placement,
        overlapping,
    }
}

/// The group's name, from the block ids in it.
///
/// The longest run of `_`-separated segments every id ends with, or the longest
/// run they all begin with when they end differently, with a trailing `ore` or
/// `ores` dropped if anything survives it. `minecraft:` is elided because a
/// bare resource location means `minecraft:` everywhere else.
///
/// `None` when nothing is left, or when the blocks come from several
/// namespaces and there is no data-derived way to pick a winner. Both are
/// reported by the caller rather than dropped.
pub fn group_name(targets: &[String]) -> Option<String> {
    let namespaces: BTreeSet<&str> = targets
        .iter()
        .map(|t| t.split_once(':').map_or("minecraft", |(ns, _)| ns))
        .collect();
    let bodies: Vec<Vec<&str>> = targets
        .iter()
        .map(|t| {
            t.split_once(':')
                .map_or(t.as_str(), |(_, body)| body)
                .split('_')
                .collect()
        })
        .collect();

    let shortest = bodies.iter().map(Vec::len).min()?;
    let common = |take: for<'a> fn(&'a [&'a str], usize) -> &'a [&'a str]| -> Vec<String> {
        let mut best: Vec<String> = Vec::new();
        for n in 1..=shortest {
            let first = take(&bodies[0], n);
            if bodies.iter().all(|b| take(b, n) == first) {
                best = first.iter().map(|s| (*s).to_owned()).collect();
            } else {
                break;
            }
        }
        best
    };

    let mut segments = common(|b, n| &b[b.len() - n..]);
    if segments.is_empty() {
        segments = common(|b, n| &b[..n]);
    }
    if segments.len() > 1 && matches!(segments.last().map(String::as_str), Some("ore" | "ores")) {
        segments.pop();
    }
    if segments.is_empty() {
        return None;
    }

    let body = segments.join("_");
    match namespaces.iter().copied().collect::<Vec<_>>()[..] {
        ["minecraft"] => Some(body),
        [one] => Some(format!("{one}:{body}")),
        _ => None,
    }
}

/// Apply the configuration to every placement in a world.
pub fn resolve_all(baselines: &[Baseline], config: &OresConfig) -> (Vec<Resolved>, Vec<Note>) {
    let mut resolved = Vec::with_capacity(baselines.len());
    let mut notes = Vec::new();
    for baseline in baselines {
        let (one, mut its_notes) = resolve_reporting(baseline, config);
        resolved.push(one);
        notes.append(&mut its_notes);
    }
    (resolved, notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_config::ore::OreOverride;

    /// A stand-in for the placements `xtask extract` produces.
    ///
    /// These numbers are invented, and that is still on purpose even now that
    /// the real ones exist in [`crate::vanilla_ores`]. If a test here asserted
    /// `3.0 × diamond` against the real figures it would be testing this
    /// module's arithmetic *and* typing a copy of Mojang's data into a
    /// hand-written file, which is the thing the extraction pipeline exists to
    /// avoid. Invented numbers test the arithmetic and nothing else; the tests
    /// that run against vanilla's own figures live next to the table they come
    /// from and assert nothing about their values.
    fn fixture() -> Vec<Baseline> {
        vec![
            Baseline {
                id: "test:ore_diamond".to_owned(),
                group: OreGroup::new("diamond"),
                attempts: Attempts::PerChunk(7),
                vein_size: 8,
                height: HeightRange::new(-64, 16),
            },
            Baseline {
                id: "test:ore_diamond_large".to_owned(),
                group: OreGroup::new("diamond"),
                attempts: Attempts::RarityFilter { one_in: 9 },
                vein_size: 12,
                height: HeightRange::new(-64, 16),
            },
            Baseline {
                id: "test:ore_iron".to_owned(),
                group: OreGroup::new("iron"),
                attempts: Attempts::PerChunk(10),
                vein_size: 9,
                height: HeightRange::new(-24, 56),
            },
        ]
    }

    fn config_with(group: &str, over: OreOverride) -> OresConfig {
        let mut config = OresConfig::default();
        config.overrides.insert(OreGroup::new(group), over);
        config
    }

    #[test]
    fn the_defaults_change_nothing_at_all() {
        // The parity guard. Not "close enough" — identical, including for the
        // rarity-filter form, which is the one that would drift if the two
        // representations were reconciled with arithmetic.
        let config = OresConfig::default();
        for baseline in fixture() {
            let resolved = resolve(&baseline, &config);
            assert!(resolved.generate);
            assert_eq!(resolved.vein_size, baseline.vein_size, "{}", baseline.id);
            assert_eq!(resolved.height, baseline.height, "{}", baseline.id);
            assert_eq!(
                resolved.expected_attempts_per_chunk(),
                baseline.attempts.expected_per_chunk(),
                "{}",
                baseline.id
            );
        }
    }

    #[test]
    fn the_master_switch_off_changes_nothing_either() {
        // The switch the Phase 6 differential test uses. It has to be identity
        // even when the file below it is full of extreme values.
        let mut config = config_with(
            "diamond",
            OreOverride {
                frequency: Some(50.0),
                vein_size: Some(8.0),
                ..Default::default()
            },
        );
        config.enabled = false;
        config.default_frequency = 20.0;
        for baseline in fixture() {
            assert_eq!(
                resolve(&baseline, &config),
                identity(&baseline),
                "{}",
                baseline.id
            );
        }
    }

    #[test]
    fn tripling_the_frequency_triples_the_attempts() {
        let config = config_with(
            "diamond",
            OreOverride {
                frequency: Some(3.0),
                ..Default::default()
            },
        );
        let resolved = resolve(&fixture()[0], &config);
        assert_eq!(resolved.attempts_per_chunk, 21);
        assert_eq!(resolved.extra_attempt_chance, 0.0);
    }

    #[test]
    fn a_multiplier_applies_to_every_placement_of_the_ore() {
        // One knob, three placements: this is the whole reason the setting is
        // keyed by ore group rather than by placed feature.
        let config = config_with(
            "diamond",
            OreOverride {
                frequency: Some(2.0),
                ..Default::default()
            },
        );
        let (resolved, _) = resolve_all(&fixture(), &config);
        assert_eq!(resolved[0].expected_attempts_per_chunk(), 14.0);
        assert!((resolved[1].expected_attempts_per_chunk() - 2.0 / 9.0).abs() < 1e-12);
        // ...and not to any other ore.
        assert_eq!(resolved[2].expected_attempts_per_chunk(), 10.0);
    }

    #[test]
    fn a_rarity_filter_can_be_scaled_past_one_attempt_per_chunk() {
        // 1-in-9 chunks, twenty-seven times as often, is three attempts in
        // every chunk. Getting this wrong by keeping the rarity form and
        // dividing 9 by 27 would give "one chunk in zero", which is where a
        // naive implementation divides by zero or silently stops scaling.
        let config = config_with(
            "diamond",
            OreOverride {
                frequency: Some(27.0),
                ..Default::default()
            },
        );
        let resolved = resolve(&fixture()[1], &config);
        assert_eq!(resolved.attempts_per_chunk, 3);
        assert!(resolved.extra_attempt_chance < 1e-12);
    }

    #[test]
    fn a_fractional_result_becomes_a_probability_rather_than_being_rounded_away() {
        // Cutting an ore to a twentieth of what it was has to leave a twentieth
        // of it, not none of it and not all of it.
        let config = config_with(
            "iron",
            OreOverride {
                frequency: Some(0.05),
                ..Default::default()
            },
        );
        let resolved = resolve(&fixture()[2], &config);
        assert_eq!(resolved.attempts_per_chunk, 0);
        assert!((resolved.extra_attempt_chance - 0.5).abs() < 1e-12);
        assert!(resolved.generate, "a rare ore still generates");
    }

    #[test]
    fn zero_frequency_and_disabled_both_stop_the_ore_generating() {
        for over in [
            OreOverride {
                frequency: Some(0.0),
                ..Default::default()
            },
            OreOverride {
                enabled: false,
                ..Default::default()
            },
        ] {
            let resolved = resolve(&fixture()[0], &config_with("diamond", over));
            assert!(!resolved.generate);
            assert_eq!(resolved.expected_attempts_per_chunk(), 0.0);
        }
    }

    #[test]
    fn vein_size_scales_independently_of_frequency() {
        let config = config_with(
            "diamond",
            OreOverride {
                frequency: Some(0.5),
                vein_size: Some(2.0),
                ..Default::default()
            },
        );
        let resolved = resolve(&fixture()[0], &config);
        assert_eq!(resolved.vein_size, 16);
        assert_eq!(resolved.expected_attempts_per_chunk(), 3.5);
    }

    #[test]
    fn an_impossible_vein_size_is_clamped_and_said_out_loud() {
        let config = config_with(
            "diamond",
            OreOverride {
                vein_size: Some(8.0),
                ..Default::default()
            },
        );
        let (resolved, notes) = resolve_reporting(&fixture()[1], &config);
        assert_eq!(resolved.vein_size, MAX_VEIN_SIZE);
        assert!(
            matches!(
                notes.as_slice(),
                [Note::VeinSizeClamped { requested: 96, .. }]
            ),
            "{notes:?}"
        );
    }

    #[test]
    fn a_height_override_replaces_only_the_bound_it_sets() {
        let config = config_with(
            "iron",
            OreOverride {
                max_y: Some(200),
                ..Default::default()
            },
        );
        let resolved = resolve(&fixture()[2], &config);
        assert_eq!(resolved.height, HeightRange::new(-24, 200));
    }

    #[test]
    fn a_height_range_that_misses_the_ore_entirely_is_reported() {
        // `min_y = 100` is a perfectly valid number and validation passes it.
        // Only here, with the world's data in hand, is it knowable that diamond
        // has nothing above y=16 to raise.
        let config = config_with(
            "diamond",
            OreOverride {
                min_y: Some(100),
                ..Default::default()
            },
        );
        let (resolved, notes) = resolve_reporting(&fixture()[0], &config);
        assert!(!resolved.generate);
        assert!(
            matches!(notes.as_slice(), [Note::HeightRangeEmpty { .. }]),
            "{notes:?}"
        );
    }

    #[test]
    fn the_default_frequency_reaches_ores_with_no_entry_of_their_own() {
        let config = OresConfig {
            default_frequency: 4.0,
            ..Default::default()
        };
        let (resolved, _) = resolve_all(&fixture(), &config);
        assert_eq!(resolved[2].expected_attempts_per_chunk(), 40.0);
    }

    #[test]
    fn resolution_does_not_depend_on_the_order_placements_arrive_in() {
        // Cheap to assert and worth asserting: the moment resolution carries
        // state between placements, ore density becomes chunk-order dependent
        // and the world stops being reproducible from its seed.
        let config = OresConfig {
            default_frequency: 2.5,
            ..Default::default()
        };
        let forward = resolve_all(&fixture(), &config).0;
        let mut reversed_input = fixture();
        reversed_input.reverse();
        let mut backward = resolve_all(&reversed_input, &config).0;
        backward.reverse();
        assert_eq!(forward, backward);
    }

    // What these tests do not catch, per the rule in `Testing.md`:
    //
    // - Nothing here places a block. Every assertion is about the numbers
    //   handed to the ore feature, and a `Resolved` that is right with a
    //   generator that ignored it would pass every test above. The feature that
    //   consumes them exists now, and the tests that dig a chunk up and count
    //   the cells live beside it in `crate::feature` — including the one that
    //   proves the default path does not rewrite the chain at all.
    // - `extra_attempt_chance` is asserted as a probability, never as an
    //   outcome. Whether it is drawn from the chunk's own decoration stream —
    //   which is what makes a world reproducible from its seed — is a property
    //   of `crate::feature::Modifier::Attempts` and is asserted there.
    // - The baselines *here* are invented, so nothing in this module's tests
    //   depends on vanilla's figures being right. The identity property is
    //   asserted against the extracted vanilla table as well, in
    //   `crate::vanilla_ores` — but that only says the resolver leaves vanilla's
    //   numbers alone, not that those numbers are what vanilla generates.
    //   Whether Dust's real ore placements match vanilla's is still something
    //   only the Phase 6 seed-for-seed differential can say.
}
