//! A stonecutter: one input, many outputs, and the player picks.
//!
//! # Why this is neither [`crafting`](crate::crafting) nor [`cooking`](crate::cooking)
//!
//! A grid recipe is matched and a cooking recipe is looked up, and both answer
//! with *one* result. A stonecutting recipe answers with a **list**: one block
//! of andesite is six different things and the player says which by pressing a
//! button. So the table here is keyed by input like [`cooking`](crate::cooking)'s
//! is, and the value is a run rather than a single entry.
//!
//! # The order is the whole of the difficulty
//!
//! The packet that comes back from the client names a recipe by **its index in
//! a list the client built**, and the client builds it by filtering every
//! stonecutting recipe it was told about and sorting the survivors. Neither
//! side states that order on the wire. If the server sorts differently the
//! player presses "stairs" and is handed a wall — silently, and with both
//! sides believing they agree.
//!
//! Vanilla sorts by the result item's *description id*, which is
//! `block.minecraft.<name>` for an item that places a block and
//! `item.minecraft.<name>` for one that does not. That is what
//! [`Cut::sort_key`] reproduces, and decision record 0037 has the measurement:
//! six inputs, 84 recipes, every button pressed against a real 1.21.1 server.
//!
//! # What it costs
//!
//! One `u32` per item for the head of the run, one `u16` for its length, and
//! six bytes per recipe: about 8 kB on 1.21.1's 1,333 items and 250 recipes.
//! A lookup is two loads and a slice. That matters because it is asked every
//! time the input slot changes, which is every click a player makes in the
//! screen.

use dust_registry::{Block, Item};

use crate::crafting::{one_into, result_stack, ItemTags, Refusal};

/// One thing an input can be cut into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    result: Item,
    count: u8,
}

impl Cut {
    /// The item that comes out, and how many.
    #[must_use]
    pub fn result(&self) -> (Item, u8) {
        (self.result, self.count)
    }

    /// The string vanilla orders the buttons by: the result item's description
    /// id.
    ///
    /// **Not the item's name.** The two agree for every one of the 167
    /// distinct results in 1.21.1's own data, because all of them place a
    /// block — but they are different strings and a data pack whose
    /// stonecutter made an item rather than a block would order differently in
    /// the client than a name comparison does here. Reproducing the real key
    /// costs one registry lookup at boot and nothing afterwards.
    #[must_use]
    pub fn sort_key(&self) -> String {
        let name = self.result.name();
        let path = name.split_once(':').map_or(name, |(_, path)| path);
        let namespace = name.split_once(':').map_or("minecraft", |(space, _)| space);
        let kind = if Block::from_name(name).is_some() {
            "block"
        } else {
            "item"
        };
        format!("{kind}.{namespace}.{path}")
    }
}

/// Where one input's cuts live in [`Cutting::cuts`].
#[derive(Debug, Clone, Copy, Default)]
struct Run {
    at: u32,
    len: u16,
}

/// Everything a stonecutter can do, keyed by what goes in.
///
/// Built in two phases like [`Recipes`](crate::crafting::Recipes): [`add`] on
/// every file, then [`index`] once, which is where the sort happens. A table
/// that sorted on every insertion would sort 250 times to end up in the same
/// place.
///
/// [`add`]: Cutting::add
/// [`index`]: Cutting::index
#[derive(Debug, Default)]
pub struct Cutting {
    /// Filled by `add`, drained by `index`. `(input id, cut)`.
    pending: Vec<(u16, Cut)>,
    /// One run per item, after `index`.
    runs: Box<[Run]>,
    /// Every cut, grouped by input and sorted within the group.
    cuts: Vec<Cut>,
    files: usize,
}

impl Cutting {
    /// An empty table sized for this build's item registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            runs: vec![Run::default(); Item::registry().entry_count()].into_boxed_slice(),
            cuts: Vec::new(),
            files: 0,
        }
    }

    /// Compile one recipe file, if it is a stonecutting recipe.
    ///
    /// Returns [`Refusal::NotAGrid`] for anything that is not one, which is
    /// the answer the other two compilers give for a file that is not theirs,
    /// so a loader can try all of them and count a file only every one refused.
    ///
    /// # Errors
    ///
    /// [`Refusal`], naming what about the file could not be read.
    pub fn add(&mut self, value: &serde_json::Value, tags: &ItemTags) -> Result<(), Refusal> {
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or(Refusal::NoType)?;
        if kind != "minecraft:stonecutting" {
            return Err(Refusal::NotAGrid(kind.to_owned()));
        }
        let (result, count) = result_stack(value)?;
        let mut accepts = Vec::new();
        match value.get("ingredient") {
            Some(serde_json::Value::Array(list)) => {
                for one in list {
                    one_into(one, tags, &mut accepts)?;
                }
            }
            Some(one @ serde_json::Value::Object(_)) => one_into(one, tags, &mut accepts)?,
            _ => return Err(Refusal::Malformed("`ingredient` is not an object or list")),
        }
        if accepts.is_empty() {
            return Err(Refusal::Malformed("an ingredient accepts nothing"));
        }
        self.files += 1;
        for id in accepts {
            self.pending.push((id, Cut { result, count }));
        }
        Ok(())
    }

    /// Group and sort. Call once, after the last [`Cutting::add`].
    ///
    /// The sort is by [`Cut::sort_key`] and it is stable, so two recipes that
    /// make the same item out of the same input keep the order the files were
    /// read in — which is the same tie-break vanilla's own sort has, and there
    /// are no such pairs in 1.21.1's data.
    pub fn index(&mut self) {
        let mut pending = std::mem::take(&mut self.pending);
        // Keyed once and then sorted, rather than formatting the key inside
        // the comparator: a comparison sort asks for the key O(n log n) times
        // and this way asks for it n.
        let mut keyed: Vec<(u16, String, Cut)> = pending
            .drain(..)
            .map(|(id, cut)| (id, cut.sort_key(), cut))
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        self.cuts = Vec::with_capacity(keyed.len());
        let mut runs = vec![Run::default(); self.runs.len()];
        for (id, _, cut) in keyed {
            let Some(run) = runs.get_mut(usize::from(id)) else {
                continue;
            };
            if run.len == 0 {
                run.at = u32::try_from(self.cuts.len()).unwrap_or(u32::MAX);
            }
            run.len = run.len.saturating_add(1);
            self.cuts.push(cut);
        }
        self.runs = runs.into_boxed_slice();
    }

    /// What this item can be cut into, **in the order the buttons are drawn**.
    ///
    /// Empty for an item no stonecutter touches, which is most of them.
    #[must_use]
    pub fn cuts_of(&self, input: Item) -> &[Cut] {
        let Some(run) = self.runs.get(input.protocol_id() as usize) else {
            return &[];
        };
        let at = run.at as usize;
        let end = at + usize::from(run.len);
        self.cuts.get(at..end).unwrap_or(&[])
    }

    /// How many files compiled.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files
    }

    /// Whether none did.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }

    /// How many `(input, cut)` pairs there are. Larger than [`Cutting::len`]
    /// wherever an ingredient is a tag or a list.
    #[must_use]
    pub fn pairs(&self) -> usize {
        self.cuts.len()
    }

    /// How many distinct items a stonecutter accepts.
    #[must_use]
    pub fn inputs(&self) -> usize {
        self.runs.iter().filter(|run| run.len > 0).count()
    }
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

    fn cut(input: &str, result: &str, count: u8) -> serde_json::Value {
        json(&format!(
            r#"{{"type":"minecraft:stonecutting","ingredient":{{"item":"{input}"}},
                 "result":{{"id":"{result}","count":{count}}}}}"#
        ))
    }

    fn table(files: &[serde_json::Value]) -> Cutting {
        let mut cutting = Cutting::new();
        let tags = ItemTags::new();
        for file in files {
            cutting.add(file, &tags).expect("the fixture compiles");
        }
        cutting.index();
        cutting
    }

    #[test]
    fn a_stonecutter_answers_with_a_list_and_a_furnace_recipe_is_not_its_business() {
        let mut cutting = Cutting::new();
        let tags = ItemTags::new();
        let refusal = cutting
            .add(
                &json(
                    r#"{"type":"minecraft:smelting","ingredient":{"item":"minecraft:raw_iron"},
                        "result":{"id":"minecraft:iron_ingot"},"cookingtime":200}"#,
                ),
                &tags,
            )
            .expect_err("a smelting recipe is not a stonecutting one");
        assert_eq!(refusal, Refusal::NotAGrid("minecraft:smelting".to_owned()));
    }

    #[test]
    fn the_buttons_are_in_the_order_vanilla_draws_them() {
        // Deliberately fed in the reverse of the answer: a table that kept
        // insertion order would pass this backwards.
        let cutting = table(&[
            cut(
                "minecraft:andesite",
                "minecraft:polished_andesite_stairs",
                1,
            ),
            cut("minecraft:andesite", "minecraft:andesite_slab", 2),
            cut("minecraft:andesite", "minecraft:polished_andesite", 1),
            cut("minecraft:andesite", "minecraft:andesite_stairs", 1),
        ]);
        let names: Vec<&str> = cutting
            .cuts_of(item("minecraft:andesite"))
            .iter()
            .map(|cut| cut.result.name())
            .collect();
        assert_eq!(
            names,
            [
                "minecraft:andesite_slab",
                "minecraft:andesite_stairs",
                "minecraft:polished_andesite",
                "minecraft:polished_andesite_stairs",
            ]
        );
    }

    #[test]
    fn a_result_that_is_not_a_block_sorts_under_item_and_not_under_its_name() {
        // `brick` sorts before `bricks` by name and after every block by
        // description id, because it is `item.minecraft.brick`. Nothing in
        // vanilla cuts to it; the point is that the key is the description id
        // and not the name.
        let brick = Cut {
            result: item("minecraft:brick"),
            count: 1,
        };
        let bricks = Cut {
            result: item("minecraft:bricks"),
            count: 1,
        };
        assert_eq!(brick.sort_key(), "item.minecraft.brick");
        assert_eq!(bricks.sort_key(), "block.minecraft.bricks");
        assert!(bricks.sort_key() < brick.sort_key());
        assert!(bricks.result.name() > brick.result.name());
    }

    #[test]
    fn the_underscore_sorts_before_a_letter_because_the_comparison_is_the_bytes() {
        // The row that says this is a plain string comparison and not
        // something that reads the words. A real 1.21.1 server puts
        // `stone_brick_wall` on button 3 and `stone_bricks` on button 4,
        // which is `_` (0x5f) before `s` (0x73) and is the opposite of what
        // splitting on underscores would give.
        let cutting = table(&[
            cut("minecraft:stone", "minecraft:stone_bricks", 1),
            cut("minecraft:stone", "minecraft:stone_brick_wall", 1),
            cut("minecraft:stone", "minecraft:stone_slab", 2),
        ]);
        let names: Vec<&str> = cutting
            .cuts_of(item("minecraft:stone"))
            .iter()
            .map(|cut| cut.result.name())
            .collect();
        assert_eq!(
            names,
            [
                "minecraft:stone_brick_wall",
                "minecraft:stone_bricks",
                "minecraft:stone_slab",
            ]
        );
    }

    #[test]
    fn an_item_no_stonecutter_touches_has_no_buttons() {
        let cutting = table(&[cut("minecraft:andesite", "minecraft:andesite_slab", 2)]);
        assert!(cutting.cuts_of(item("minecraft:diamond")).is_empty());
        assert_eq!(cutting.inputs(), 1);
        assert_eq!(cutting.pairs(), 1);
    }

    #[test]
    fn two_inputs_keep_their_own_runs() {
        let cutting = table(&[
            cut("minecraft:stone", "minecraft:stone_stairs", 1),
            cut("minecraft:andesite", "minecraft:andesite_slab", 2),
            cut("minecraft:stone", "minecraft:stone_bricks", 1),
        ]);
        let stone: Vec<&str> = cutting
            .cuts_of(item("minecraft:stone"))
            .iter()
            .map(|cut| cut.result.name())
            .collect();
        assert_eq!(stone, ["minecraft:stone_bricks", "minecraft:stone_stairs"]);
        assert_eq!(cutting.cuts_of(item("minecraft:andesite")).len(), 1);
        assert_eq!(cutting.cuts_of(item("minecraft:andesite"))[0].count, 2);
    }

    #[test]
    fn a_tag_ingredient_gives_every_member_the_same_list() {
        let mut tags = ItemTags::new();
        tags.insert(
            "minecraft:stone_crafting_materials".to_owned(),
            vec![item("minecraft:stone"), item("minecraft:andesite")],
        );
        let mut cutting = Cutting::new();
        cutting
            .add(
                &json(
                    r#"{"type":"minecraft:stonecutting",
                        "ingredient":{"tag":"minecraft:stone_crafting_materials"},
                        "result":{"id":"minecraft:stone_slab","count":2}}"#,
                ),
                &tags,
            )
            .expect("the fixture compiles");
        cutting.index();
        assert_eq!(cutting.len(), 1);
        assert_eq!(cutting.pairs(), 2);
        assert_eq!(cutting.cuts_of(item("minecraft:stone")).len(), 1);
        assert_eq!(cutting.cuts_of(item("minecraft:andesite")).len(), 1);
    }
}
