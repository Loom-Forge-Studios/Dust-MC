//! The recipes beside the operator's data, and what reading them found.
//!
//! `<[data] path>/<namespace>/recipe/<name>.json`, and the item tags under
//! `<namespace>/tags/item/` an ingredient may name. The same directory
//! [`super::drops`] reads loot tables from and for the same reason: a recipe is
//! data pack content, the operator's `--server` data generator already wrote
//! it, and **no new file, no new extraction step and nothing of Mojang's is
//! committed**. Decision record 0022 made this argument for loot; decision
//! record 0031 makes it again for crafting.
//!
//! # Tags come from the data pack, not from this build
//!
//! `dust_registry::tags` holds vanilla's own tag table and it would answer
//! `#minecraft:planks` correctly today. It is not what this reads. A recipe's
//! tags are the one place a data pack's *additions* matter most — a pack that
//! adds a wood adds it to `#minecraft:planks` and expects a crafting table to
//! notice — and a server that resolved the tag out of its own compiled copy
//! would make that pack's planks uncraftable while its recipes loaded fine.
//! Reading the operator's own directory means there is one answer rather than
//! two that can disagree.
//!
//! Tag references resolve: `#minecraft:logs` names three other tags on 1.21.1
//! and nothing dangles. A cycle terminates and contributes what it can rather
//! than hanging the boot.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dust_registry::Item;
use dust_sim::cooking::{Cooking, Fire, FIRES};
use dust_sim::crafting::{ItemTags, Recipes, Refusal};
use dust_sim::cutting::Cutting;
use dust_sim::smithing::Smithing;

/// Where the recipes live inside one namespace.
const RECIPES_UNDER: &str = "recipe";
/// Where the item tags live inside one namespace.
const TAGS_UNDER: &str = "tags/item";

/// What reading the recipes found, for the line the server prints at boot.
#[derive(Debug, Default)]
pub struct Report {
    /// Namespaces that had a `recipe` directory at all.
    pub namespaces: Vec<String>,
    /// Files offered.
    pub files: u32,
    /// Recipes that compiled into something a grid can make.
    pub compiled: usize,
    /// Recipes one of the four fires can cook.
    pub cooked: usize,
    /// How many (fire, item) pairs the cooking lookup holds, larger than
    /// [`Report::cooked`] wherever an ingredient is a tag or a list.
    pub cooked_pairs: usize,
    /// A pair a later file wanted and an earlier one already held.
    pub cooked_collisions: usize,
    /// Recipes a stonecutter can cut.
    pub cut: usize,
    /// How many (input, cut) pairs the stonecutter lookup holds.
    pub cut_pairs: usize,
    /// Recipes a smithing table can make.
    pub smithed: usize,
    /// `smithing_trim` files, which are a component this server does not
    /// author. Counted by name rather than folded into [`Report::not_a_grid`]:
    /// eighteen recipes at a bench a player *can* open is a different fact
    /// from a recipe for a bench that does not exist.
    pub trims: usize,
    /// Files whose `type` no compiler here claims. Counted apart because they
    /// are not defects; they are recipes this server does not run yet.
    pub not_a_grid: u32,
    /// The `crafting_special_*` markers, which are Java classes rather than
    /// described recipes. A firework, a dyed leather cap, a copied map.
    pub special: u32,
    /// Files this compiler refused, with why, capped so a broken pack cannot
    /// fill the log.
    pub errors: Vec<String>,
    /// Files refused for any reason other than the two counted above.
    pub refused: u32,
    /// Item tags read, after resolution.
    pub tags: usize,
    /// How many (item, recipe) pairs the lookup index holds.
    pub index_len: usize,
    /// How many item slots the ingredient pool holds.
    pub choice_len: usize,
}

/// How many failing files are named before the rest are counted.
const NAMED_ERRORS: usize = 5;

impl Report {
    /// The one line the boot log prints.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "{} recipe file(s) in {}, {} craftable in a grid, {} cooked at a fire; \
             {} cut at a stonecutter, {} made at a smithing table; \
             {} not run here, {} are code rather than data, \
             {} refused; \
             {} item tag(s); index {} pair(s), {} ingredient slot(s); \
             cooking {} pair(s), cutting {} pair(s){}",
            self.files,
            self.namespaces.join(", "),
            self.compiled,
            self.cooked,
            self.cut,
            self.smithed,
            self.not_a_grid,
            self.special,
            self.refused,
            self.tags,
            self.index_len,
            self.choice_len,
            self.cooked_pairs,
            self.cut_pairs,
            if self.cooked_collisions == 0 {
                String::new()
            } else {
                format!(", {} claimed twice", self.cooked_collisions)
            },
        );
        for error in &self.errors {
            line.push_str("\n  ");
            line.push_str(error);
        }
        line
    }
}

/// Read every recipe beside a data directory.
///
/// `root` is `[data] path` — the directory holding `minecraft/`.
///
/// A file that will not compile does **not** stop the server, for the reason
/// [`super::drops::beside`] gives: one unreadable recipe is one thing a player
/// cannot make and a named line in the log, where refusing to boot over it is
/// a server an operator cannot run.
pub fn beside(root: impl AsRef<Path>) -> (Recipes, Cooking, Cutting, Smithing, Report) {
    let root = root.as_ref();
    let mut recipes = Recipes::default();
    let mut cooking = Cooking::new();
    let mut cutting = Cutting::new();
    let mut smithing = Smithing::new();
    let mut report = Report::default();

    let tags = item_tags(root);
    report.tags = tags.len();

    let Ok(namespaces) = std::fs::read_dir(root) else {
        return (recipes, cooking, cutting, smithing, report);
    };
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    for entry in namespaces.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let directory = entry.path().join(RECIPES_UNDER);
        if directory.is_dir() {
            roots.push((name, directory));
        }
    }
    // Sorted so two machines with the same data print the same line.
    roots.sort();

    for (namespace, directory) in roots {
        report.namespaces.push(namespace.clone());
        let mut files = Vec::new();
        walk(&directory, &directory, &mut files);
        files.sort();
        for (stem, path) in files {
            report.files += 1;
            let id = format!("{namespace}:{stem}");
            let Ok(text) = std::fs::read_to_string(&path) else {
                report.refused += 1;
                note(&mut report, &id, "could not be read");
                continue;
            };
            let value: serde_json::Value = match serde_json::from_str(&text) {
                Ok(value) => value,
                Err(error) => {
                    report.refused += 1;
                    note(&mut report, &id, &error.to_string());
                    continue;
                }
            };
            // The grid first, then the four fires, then the stonecutter,
            // then the smithing table. Every compiler answers `NotAGrid` for
            // a file that is not theirs, so a file is only counted as one
            // this server does not run when **all four** have said so — a
            // file counted against the first compiler that shrugged at it
            // would count every smelting recipe as unreachable on the day the
            // furnace started reading them.
            let refusal = match recipes.add(&id, &value, &tags) {
                Ok(()) => continue,
                Err(refusal) => refusal,
            };
            let refusal = match refusal {
                Refusal::NotAGrid(_) => match cooking.add(&value, &tags) {
                    Ok(()) => continue,
                    Err(second) => second,
                },
                first => first,
            };
            let refusal = match refusal {
                Refusal::NotAGrid(_) => match cutting.add(&value, &tags) {
                    Ok(()) => continue,
                    Err(third) => third,
                },
                first => first,
            };
            let refusal = match refusal {
                Refusal::NotAGrid(_) => match smithing.add(&value, &tags) {
                    Ok(()) => continue,
                    Err(fourth) => fourth,
                },
                first => first,
            };
            match refusal {
                Refusal::NotAGrid(_) => report.not_a_grid += 1,
                Refusal::Special(_) => report.special += 1,
                other => {
                    report.refused += 1;
                    note(&mut report, &id, &other.to_string());
                }
            }
        }
    }

    recipes.index();
    cutting.index();
    report.compiled = recipes.len();
    report.index_len = recipes.index_len();
    report.choice_len = recipes.choice_len();
    report.cooked = cooking.len();
    report.cooked_pairs = cooking.pairs();
    report.cooked_collisions = cooking.collisions();
    report.cut = cutting.len();
    report.cut_pairs = cutting.pairs();
    report.smithed = smithing.len();
    report.trims = smithing.trims();
    (recipes, cooking, cutting, smithing, report)
}

/// How many pairs each fire cooks, for the boot line and for a check that one
/// of them is not quietly empty.
pub fn per_fire(cooking: &Cooking) -> Vec<(Fire, usize)> {
    FIRES
        .into_iter()
        .map(|fire| (fire, cooking.pairs_in(fire)))
        .collect()
}

fn note(report: &mut Report, id: &str, why: &str) {
    if report.errors.len() < NAMED_ERRORS {
        report.errors.push(format!("{id}: {why}"));
    }
}

/// Every `.json` under a directory, as (path relative to the root without the
/// extension, full path). Recipes and tags both nest one level on 1.21.1.
fn walk(root: &Path, directory: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let mut stem = relative.with_extension("").to_string_lossy().into_owned();
        // A namespaced id always uses forward slashes, whatever the platform's
        // separator is.
        if std::path::MAIN_SEPARATOR != '/' {
            stem = stem.replace(std::path::MAIN_SEPARATOR, "/");
        }
        out.push((stem, path));
    }
}

/// One tag's file, before references are followed. `replace` is applied as
/// the file is read rather than kept, so what is here is the merged list.
struct Raw {
    values: Vec<String>,
}

/// Read `tags/item` from every namespace and resolve every reference.
fn item_tags(root: &Path) -> ItemTags {
    let mut raw: BTreeMap<String, Raw> = BTreeMap::new();
    let Ok(namespaces) = std::fs::read_dir(root) else {
        return ItemTags::new();
    };
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    for entry in namespaces.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let directory = entry.path().join(TAGS_UNDER);
        if directory.is_dir() {
            roots.push((name, directory));
        }
    }
    roots.sort();
    for (namespace, directory) in roots {
        let mut files = Vec::new();
        walk(&directory, &directory, &mut files);
        files.sort();
        for (stem, path) in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let replace = value
                .get("replace")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let mut values = Vec::new();
            if let Some(list) = value.get("values").and_then(serde_json::Value::as_array) {
                for entry in list {
                    // A member is a string, or an object carrying the same
                    // string under `id` with a `required` flag beside it. Both
                    // spellings name the same member; the flag only says
                    // whether a missing target is an error, and a missing
                    // target contributes nothing either way here.
                    let name = match entry {
                        serde_json::Value::String(name) => Some(name.as_str()),
                        serde_json::Value::Object(_) => {
                            entry.get("id").and_then(serde_json::Value::as_str)
                        }
                        _ => None,
                    };
                    if let Some(name) = name {
                        values.push(name.to_owned());
                    }
                }
            }
            let id = format!("{namespace}:{stem}");
            // **Tags merge, everything else overrides** — the rule
            // `dust_data`'s header states and the reason it is the one
            // exception: two packs adding a wood to `#planks` are not
            // disagreeing about anything. `"replace": true` is how a pack says
            // it meant to throw the earlier list away.
            match raw.entry(id) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(Raw { values });
                }
                std::collections::btree_map::Entry::Occupied(mut slot) => {
                    if replace {
                        slot.insert(Raw { values });
                    } else {
                        slot.get_mut().values.extend(values);
                    }
                }
            }
        }
    }

    let mut resolved = ItemTags::new();
    let names: Vec<String> = raw.keys().cloned().collect();
    for name in names {
        let mut seen = BTreeSet::new();
        let mut items = BTreeSet::new();
        resolve(&raw, &name, &mut seen, &mut items);
        resolved.insert(name, items.into_iter().collect());
    }
    resolved
}

/// Follow one tag's members, and the tags they name, into items.
///
/// `seen` makes a cycle terminate: a tag that names itself, directly or
/// through others, contributes what it can and stops. A data pack can write
/// one and a boot that hung on it would be indistinguishable from a hang.
fn resolve(
    raw: &BTreeMap<String, Raw>,
    name: &str,
    seen: &mut BTreeSet<String>,
    items: &mut BTreeSet<Item>,
) {
    if !seen.insert(name.to_owned()) {
        return;
    }
    let Some(tag) = raw.get(name) else {
        return;
    };
    for value in &tag.values {
        match value.strip_prefix('#') {
            Some(reference) => resolve(raw, &namespaced(reference), seen, items),
            None => {
                if let Some(item) = Item::from_name(&namespaced(value)) {
                    items.insert(item);
                }
            }
        }
    }
}

/// A name with its namespace, defaulting to `minecraft:` the way every
/// Minecraft id does.
fn namespaced(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

/// The recipes a client has to be told about, as one already-encoded packet.
///
/// # Why the client needs any of them
///
/// Dust matches every recipe on the server, so for a crafting table, a furnace
/// and a smithing table the client needs nothing: it is shown what the slots
/// hold and it draws that. A **stonecutter is different**. Its buttons are
/// drawn by the client out of its own recipe list, filtered by whatever is in
/// the input slot, and the packet it sends back names a button by its position
/// in that list. A client that was told no recipes draws no buttons, and the
/// screen is a dead end.
///
/// # And why not all 1,290
///
/// Vanilla declares everything, which is about 100 kB per join on 1.21.1. The
/// only thing that changes for a player is the recipe *book*, which needs a
/// second packet Dust does not send and which is not reachable from anything
/// this server opens. So this declares the 250 stonecutting recipes and the
/// nine smithing transforms — the screens whose contents the client computes —
/// and nothing else. Decision record 0044.
///
/// The smithing nine are in for a reason of their own: the vanilla client
/// recomputes a smithing table's result whenever an input slot changes and
/// **clears its own result slot** when nothing matches. A client with no
/// smithing recipes would blank a result the server had just sent.
///
/// Built once at boot and cloned per join, which is one allocation and a
/// memcpy of a few kilobytes rather than several thousand small ones.
///
/// # Errors
///
/// The encoding fails only if the packet does not exist at this version.
pub fn declaration(
    cutting: &Cutting,
    smithing: &Smithing,
    version: dust_protocol::ProtocolVersion,
) -> Option<dust_net::frame::Frame> {
    use dust_protocol::packets::play::containers::{
        Ingredient, Recipe, RecipeKind, SmithingTransformData, StonecuttingData,
    };
    use dust_protocol::types::{Identifier, ProtocolString, Slot};

    let stack = |item: dust_registry::Item, count: u8| Slot::Present {
        count: i32::from(count),
        item_id: item.protocol_id() as i32,
        components: dust_protocol::components::ComponentPatch::EMPTY,
    };
    let one = |items: Vec<dust_registry::Item>| Ingredient {
        items: items.into_iter().map(|item| stack(item, 1)).collect(),
    };

    let mut recipes = Vec::new();
    // The id is a name the client uses only to tell two recipes apart, and
    // Dust does not keep the file's own. A stable synthetic one is enough and
    // is not Mojang's data: `dust:cut/<n>`.
    for (index, input) in dust_registry::Item::all().enumerate() {
        let cuts = cutting.cuts_of(input);
        if cuts.is_empty() {
            continue;
        }
        for (which, cut) in cuts.iter().enumerate() {
            let (item, count) = cut.result();
            recipes.push(Recipe {
                id: Identifier::parse(&format!("dust:cut/{index}/{which}")).ok()?,
                kind: RecipeKind::Stonecutting(StonecuttingData {
                    group: ProtocolString::new(String::new()).ok()?,
                    ingredient: one(vec![input]),
                    result: stack(item, count),
                }),
            });
        }
    }
    for (index, transform) in smithing.iter().enumerate() {
        let (item, count) = transform.result();
        recipes.push(Recipe {
            id: Identifier::parse(&format!("dust:smith/{index}")).ok()?,
            kind: RecipeKind::SmithingTransform(SmithingTransformData {
                template: one(transform.template_items().collect()),
                base: one(transform.base_items().collect()),
                addition: one(transform.addition_items().collect()),
                result: stack(item, count),
            }),
        });
    }

    // Nothing to declare is not an empty declaration. A client that receives
    // no recipes and a client that receives a list of none are in the same
    // state, so the second is a packet for nobody — and a server with no
    // `[data] path` is the ordinary case, not an unusual one.
    if recipes.is_empty() {
        return None;
    }
    let packet = dust_protocol::packets::play::clientbound::Packet::UpdateRecipes(
        dust_protocol::packets::play::clientbound::UpdateRecipes { recipes },
    );
    let mut body = dust_protocol::wire::Writer::default();
    packet.encode_body(&mut body, version).ok()?;
    Some(dust_net::frame::Frame::new(
        packet.protocol_id(version).ok()? as i32,
        body.into_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tag that names itself resolves rather than hanging, and still
    /// contributes the members it does have.
    #[test]
    fn a_tag_cycle_terminates() {
        let mut raw = BTreeMap::new();
        raw.insert(
            "minecraft:a".to_owned(),
            Raw {
                values: vec!["#minecraft:b".to_owned(), "minecraft:stick".to_owned()],
            },
        );
        raw.insert(
            "minecraft:b".to_owned(),
            Raw {
                values: vec!["#minecraft:a".to_owned()],
            },
        );
        let mut seen = BTreeSet::new();
        let mut items = BTreeSet::new();
        resolve(&raw, "minecraft:a", &mut seen, &mut items);
        assert_eq!(items.len(), 1);
    }

    /// A bare member is `minecraft:`, which is how every Minecraft id reads.
    #[test]
    fn a_bare_name_is_a_minecraft_name() {
        assert_eq!(namespaced("stick"), "minecraft:stick");
        assert_eq!(namespaced("mypack:stick"), "mypack:stick");
    }
}
