//! A smithing table: three items in, one out, and the third is netherite.
//!
//! # Why this is a scan and everything beside it is an index
//!
//! [`crafting`](crate::crafting), [`cooking`](crate::cooking) and
//! [`cutting`](crate::cutting) all build a lookup keyed by item, because each
//! of them is asked hundreds of times a second by a world full of blocks or by
//! a player dragging a stack across a grid. This one is asked when a player
//! puts an item into one of three slots, and 1.21.1 has **nine** recipes to
//! compare it against. A per-item index over 1,333 items to save eight
//! comparisons on a mouse click would cost 5 kB to buy nothing.
//!
//! # What is here and what is not
//!
//! `minecraft:smithing_transform` — the nine netherite upgrades — is here.
//! `minecraft:smithing_trim`, the eighteen armour trims, is **not**: a trim's
//! result is the base item carrying a `minecraft:trim` component this server
//! would have to author, and Dust does not write components, it carries the
//! ones that arrive. Eighteen files stay unreachable and the recipe report
//! says so rather than counting them as done.
//!
//! # The result is the base, transmuted
//!
//! Vanilla's `SmithingTransformRecipe.assemble` calls `transmuteCopy`, which
//! keeps **the base stack's components** and changes only which item it is. A
//! player upgrading an enchanted, named, half-worn diamond chestplate gets an
//! enchanted, named, half-worn netherite one. Building the result out of the
//! recipe's own item alone would silently strip every enchantment a player had
//! spent levels on, which is the most expensive thing this file could get
//! wrong — see [`Transform::result`] for what a caller must do with it.

use dust_registry::Item;

use crate::crafting::{one_into, result_stack, ItemTags, Refusal};

/// One `smithing_transform`: what three slots must hold, and what they become.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transform {
    template: Vec<u16>,
    base: Vec<u16>,
    addition: Vec<u16>,
    result: Item,
    count: u8,
}

impl Transform {
    /// Whether these three items are this recipe.
    ///
    /// Every slot must be filled. A smithing table with two of the three is
    /// not a partial match, it is not a match.
    #[must_use]
    pub fn matches(&self, template: Item, base: Item, addition: Item) -> bool {
        contains(&self.template, template)
            && contains(&self.base, base)
            && contains(&self.addition, addition)
    }

    /// The item that comes out, and how many.
    ///
    /// **This is the item only.** The stack a player receives is the *base*
    /// stack with this item substituted, keeping its components; see this
    /// module's header. A caller that built a fresh stack out of this would
    /// hand back an unenchanted one.
    #[must_use]
    pub fn result(&self) -> (Item, u8) {
        (self.result, self.count)
    }
}

fn contains(accepts: &[u16], item: Item) -> bool {
    accepts.contains(&(item.protocol_id() as u16))
}

/// Every smithing recipe this server runs.
#[derive(Debug, Default)]
pub struct Smithing {
    transforms: Vec<Transform>,
    /// How many `smithing_trim` files were seen and not compiled. Counted
    /// rather than silently refused: eighteen recipes a player can see in
    /// their recipe book and cannot make is a fact the boot line should state.
    trims: usize,
}

impl Smithing {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Compile one recipe file, if it is a smithing recipe this server runs.
    ///
    /// Returns [`Refusal::NotAGrid`] for anything else — including
    /// `smithing_trim`, which is counted first so the loader's "unreachable"
    /// line can name it.
    ///
    /// # Errors
    ///
    /// [`Refusal`], naming what about the file could not be read.
    pub fn add(&mut self, value: &serde_json::Value, tags: &ItemTags) -> Result<(), Refusal> {
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or(Refusal::NoType)?;
        if kind == "minecraft:smithing_trim" {
            self.trims += 1;
            return Err(Refusal::NotAGrid(kind.to_owned()));
        }
        if kind != "minecraft:smithing_transform" {
            return Err(Refusal::NotAGrid(kind.to_owned()));
        }
        let (result, count) = result_stack(value)?;
        let template = slot_of(value, "template", tags)?;
        let base = slot_of(value, "base", tags)?;
        let addition = slot_of(value, "addition", tags)?;
        self.transforms.push(Transform {
            template,
            base,
            addition,
            result,
            count,
        });
        Ok(())
    }

    /// The first recipe these three items are, or `None`.
    ///
    /// First and not best: vanilla takes `list.getFirst()` out of its own
    /// matches, and 1.21.1's nine transforms are disjoint on the base item, so
    /// there is never a second one to choose between.
    #[must_use]
    pub fn find(&self, template: Item, base: Item, addition: Item) -> Option<&Transform> {
        self.transforms
            .iter()
            .find(|one| one.matches(template, base, addition))
    }

    /// How many recipes compiled.
    #[must_use]
    pub fn len(&self) -> usize {
        self.transforms.len()
    }

    /// Whether none did.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.transforms.is_empty()
    }

    /// How many armour-trim files were seen and left unmade.
    #[must_use]
    pub fn trims(&self) -> usize {
        self.trims
    }

    /// Every recipe, for the loader that has to declare them to a client.
    pub fn iter(&self) -> impl Iterator<Item = &Transform> {
        self.transforms.iter()
    }
}

impl Transform {
    /// The items this slot accepts, for a declaration to a client.
    pub fn template_items(&self) -> impl Iterator<Item = Item> + '_ {
        self.template
            .iter()
            .filter_map(|id| Item::from_protocol_id(u32::from(*id)))
    }

    /// The items the base slot accepts.
    pub fn base_items(&self) -> impl Iterator<Item = Item> + '_ {
        self.base
            .iter()
            .filter_map(|id| Item::from_protocol_id(u32::from(*id)))
    }

    /// The items the addition slot accepts.
    pub fn addition_items(&self) -> impl Iterator<Item = Item> + '_ {
        self.addition
            .iter()
            .filter_map(|id| Item::from_protocol_id(u32::from(*id)))
    }
}

fn slot_of(
    value: &serde_json::Value,
    key: &'static str,
    tags: &ItemTags,
) -> Result<Vec<u16>, Refusal> {
    let mut accepts = Vec::new();
    match value.get(key) {
        Some(serde_json::Value::Array(list)) => {
            for one in list {
                one_into(one, tags, &mut accepts)?;
            }
        }
        Some(one @ serde_json::Value::Object(_)) => one_into(one, tags, &mut accepts)?,
        _ => {
            return Err(Refusal::Malformed(
                "a smithing slot is not an object or list",
            ))
        }
    }
    if accepts.is_empty() {
        return Err(Refusal::Malformed("a smithing slot accepts nothing"));
    }
    accepts.sort_unstable();
    accepts.dedup();
    Ok(accepts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("the fixture is JSON")
    }

    fn item(name: &str) -> Item {
        Item::from_name(name).expect("the generated item table has it")
    }

    fn upgrade(base: &str, result: &str) -> serde_json::Value {
        json(&format!(
            r#"{{"type":"minecraft:smithing_transform",
                 "template":{{"item":"minecraft:netherite_upgrade_smithing_template"}},
                 "base":{{"item":"{base}"}},
                 "addition":{{"item":"minecraft:netherite_ingot"}},
                 "result":{{"id":"{result}","count":1}}}}"#
        ))
    }

    fn table() -> Smithing {
        let mut smithing = Smithing::new();
        let tags = ItemTags::new();
        smithing
            .add(
                &upgrade(
                    "minecraft:diamond_chestplate",
                    "minecraft:netherite_chestplate",
                ),
                &tags,
            )
            .expect("the fixture compiles");
        smithing
            .add(
                &upgrade("minecraft:diamond_sword", "minecraft:netherite_sword"),
                &tags,
            )
            .expect("the fixture compiles");
        smithing
    }

    #[test]
    fn three_right_items_are_an_upgrade() {
        let smithing = table();
        let found = smithing
            .find(
                item("minecraft:netherite_upgrade_smithing_template"),
                item("minecraft:diamond_chestplate"),
                item("minecraft:netherite_ingot"),
            )
            .expect("the recipe is there");
        assert_eq!(found.result(), (item("minecraft:netherite_chestplate"), 1));
    }

    #[test]
    fn the_same_three_in_the_wrong_slots_are_nothing() {
        // Measured against a real 1.21.1 server: template and addition
        // swapped leaves the result slot empty. A matcher that tested a set
        // rather than three positions would pass this and hand out a free
        // upgrade for any arrangement.
        let smithing = table();
        assert!(smithing
            .find(
                item("minecraft:netherite_ingot"),
                item("minecraft:diamond_chestplate"),
                item("minecraft:netherite_upgrade_smithing_template"),
            )
            .is_none());
    }

    #[test]
    fn a_base_nothing_upgrades_is_nothing() {
        let smithing = table();
        assert!(smithing
            .find(
                item("minecraft:netherite_upgrade_smithing_template"),
                item("minecraft:iron_chestplate"),
                item("minecraft:netherite_ingot"),
            )
            .is_none());
    }

    #[test]
    fn a_trim_is_counted_and_not_compiled() {
        let mut smithing = Smithing::new();
        let tags = ItemTags::new();
        let refusal = smithing
            .add(
                &json(
                    r#"{"type":"minecraft:smithing_trim",
                        "template":{"item":"minecraft:coast_armor_trim_smithing_template"},
                        "base":{"item":"minecraft:iron_chestplate"},
                        "addition":{"item":"minecraft:copper_ingot"}}"#,
                ),
                &tags,
            )
            .expect_err("a trim is not compiled");
        assert_eq!(
            refusal,
            Refusal::NotAGrid("minecraft:smithing_trim".to_owned())
        );
        assert_eq!(smithing.trims(), 1);
        assert_eq!(smithing.len(), 0);
    }

    #[test]
    fn a_grid_recipe_is_not_this_compilers_business() {
        let mut smithing = Smithing::new();
        let tags = ItemTags::new();
        let refusal = smithing
            .add(
                &json(r#"{"type":"minecraft:crafting_shapeless","ingredients":[]}"#),
                &tags,
            )
            .expect_err("a grid recipe is not a smithing one");
        assert_eq!(
            refusal,
            Refusal::NotAGrid("minecraft:crafting_shapeless".to_owned())
        );
        assert_eq!(smithing.trims(), 0);
    }
}
