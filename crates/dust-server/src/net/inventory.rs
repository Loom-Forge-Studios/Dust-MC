//! What a player is carrying.
//!
//! This replaces `net::hotbar`, which was nine slots and a selection and said
//! in its own first paragraph that it was not an inventory. What that module
//! got right is kept whole — vanilla's slot numbering as a named constant, an
//! unknown item id treated as emptiness rather than as a disconnect, an
//! out-of-range selection left alone rather than wrapped — and the thirty-seven
//! slots it named as missing are here.
//!
//! # The forty-six slots, and why all of them
//!
//! A player's own container is `0..=45` in vanilla's numbering and every packet
//! that touches it uses those numbers:
//!
//! ```text
//!  0        crafting output
//!  1..=4    crafting grid
//!  5..=8    armour: head, chest, legs, feet
//!  9..=35   main inventory
//! 36..=44   hotbar
//! 45        offhand
//! ```
//!
//! The five crafting slots are the 2x2 grid a player carries with them, and
//! slot 0 is what it makes. The recipes are the operator's own files, read at
//! boot by `registries::recipes` and matched by [`dust_sim::crafting`]; an
//! [`Inventory`] built without them behaves exactly as this container did
//! before crafting existed, with an output slot that never fills.
//!
//! # Counts, and where the number comes from
//!
//! A stack is an item and a count, and the count is bounded by
//! [`Item::max_stack_size`] — 64 for dirt, 16 for an ender pearl, 1 for a
//! bucket. **Nothing here writes 64.** That number is Minecraft's, it is
//! per-item, and it already arrives from the operator's own jar: the item
//! component table `cargo xtask extract` generates carries
//! `minecraft:max_stack_size` for all 1,333 items, and the extractor refuses a
//! table where any of them is not an integer in `1..=99`. So a stack of
//! sixty-four buckets is refused here for the same reason vanilla refuses it,
//! from the same number, and a version that changed a stack size changes this
//! with no edit.
//!
//! # What is stored
//!
//! A [`Stack`] is an [`Item`], a `u8` and a component patch. The first two are
//! four bytes; the third is one `Option<Arc<[u8]>>` that is `None` for the
//! overwhelming majority of stacks. The whole container is a fixed array, so
//! reading a slot is an index and writing one is a store, and nothing on the
//! read path allocates — which matters because `held()` is read on every
//! right-click and the container is written on every click a player makes.
//!
//! The components are the whole of what makes one diamond sword different from
//! another: its name, its enchantments, how worn it is, what is inside it.
//! Dust does not model any of that and does not need to — see
//! [`dust_protocol::components`] — it walks a component to find where it ends
//! and then keeps, compares and returns the bytes exactly as they arrived.
//!
//! **Two stacks merge only if their components are equal**, and that rule
//! reaches every mode: a left click, a right click, a shift-click, a
//! double-click and a drag all ask [`Stack::stacks_with`] rather than comparing
//! items. Getting it wrong in one direction duplicates a player's property and
//! in the other destroys it. A real 1.21.1 server was asked: a stack named Bob
//! put down on a plain stack of the same block **swaps**, and it is the same
//! here.
//!
//! # What a click does
//!
//! [`Inventory::click`] is `Click Container`'s seven modes replayed over this
//! state. It is a real specification and it is followed rather than guessed at:
//! left and right click, shift-click, the number keys and F, creative clone,
//! Q and control-Q, the three drags, and double-click-to-collect.
//!
//! # What a slot will accept
//!
//! Forty-four of the forty-six slots take any item. The crafting output takes
//! none, and the four armour slots take only what is worn *in that slot* —
//! which is why [`worn_in`] exists and why it is built out of Mojang's item
//! tags rather than written down. That one rule reaches five of the seven
//! modes: a left click, a right click, a shift-click, a number key and a drag
//! all consult it, and the drag consults it when a slot *joins* the drag
//! rather than at the end, because the share each slot receives is divided by
//! how many slots joined.
//!
//! All of it was measured. `tools/bot/clicks.js` replays eighty-two clicks
//! against this server and against a real 1.21.1 server and diffs the two
//! recordings; the armour, offhand and crafting-grid clicks are the last
//! twenty-five of them, and they are what said this paragraph was wrong before
//! it was written. Decision record 0016 has the counts.
//!
//! Dropping is real and the item is *gone*: there are no item entities in the
//! world yet, so Q destroys rather than throws. Stated here because a player
//! finds that out by losing something.

use std::sync::{Arc, OnceLock};

use dust_protocol::components::ComponentPatch;
use dust_protocol::types::Slot;
use dust_registry::placement::ItemBlocks;
use dust_registry::tags::{self, TagRegistry};
use dust_registry::Item;
use dust_sim::crafting::Recipes;

/// How many slots a player's own container has. Vanilla's `0..=45`.
pub const SLOTS: usize = 46;

/// The crafting output.
///
/// Not a normal slot in either direction. Nothing may be *put* here — vanilla
/// refuses a creative write and a click alike — and what is taken *out* is
/// paid for with the grid: see [`Inventory::take_result`].
pub const CRAFTING_OUTPUT: usize = 0;

/// The 2x2 crafting grid, `1..=4`.
pub const CRAFTING_START: usize = 1;
/// One past the crafting grid, so that `CRAFTING_START..CRAFTING_END` is a
/// range rather than an arithmetic exercise at each call site.
pub const CRAFTING_END: usize = 5;

/// How wide the player's own grid is. Slots `1..=4` are a 2x2 read row-major,
/// which is the order `CraftingContainer` numbers them and therefore the order
/// a pattern must be laid against.
pub const CRAFTING_WIDTH: usize = 2;

/// Armour, `5..=8`: head, chest, legs, feet.
pub const ARMOUR_START: usize = 5;
/// One past the armour.
pub const ARMOUR_END: usize = 9;

/// The four armour slots by name, because `ARMOUR_START + 2` at a call site is
/// a place to be wrong by one and the mistake looks like a client bug.
pub const ARMOUR_HEAD: usize = 5;
/// The chest slot, which is also where an elytra goes.
pub const ARMOUR_CHEST: usize = 6;
/// The leggings slot.
pub const ARMOUR_LEGS: usize = 7;
/// The boots slot.
pub const ARMOUR_FEET: usize = 8;

/// The main inventory, `9..=35`.
pub const MAIN_START: usize = 9;
/// One past the main inventory, which is also where the hotbar begins.
pub const MAIN_END: usize = 36;

/// Where the hotbar sits in the player's container, which is what
/// `set_creative_mode_slot` and every click numbers its slots by.
///
/// A named range rather than a subtraction at the call site: a slot index off
/// by nine is a player holding the wrong thing, which looks exactly like a
/// client bug.
pub const HOTBAR_START: usize = 36;
/// One past the hotbar, which is also the offhand.
pub const HOTBAR_END: usize = 45;

/// The offhand, slot 45.
pub const OFFHAND: usize = 45;

/// How many hotbar slots there are. Vanilla's `Inventory.SELECTION_SIZE`.
pub const HOTBAR_SLOTS: usize = 9;

/// The crafting table's own 3x3, `46..=54` in this container's *storage*.
///
/// Not slots any window numbers: a crafting table's grid belongs to the table
/// menu, which numbers it `1..=9`, and the player's own window cannot see it
/// at all. Storing it here rather than in a menu of its own is what lets one
/// implementation of the seven click modes serve both windows — see
/// [`Window`].
pub const TABLE_GRID_START: usize = 46;
/// One past the table's grid.
pub const TABLE_GRID_END: usize = 55;
/// The table's output, which is [`CRAFTING_OUTPUT`]'s opposite number.
pub const TABLE_OUTPUT: usize = 55;
/// How wide a crafting table's grid is.
pub const TABLE_WIDTH: usize = 3;
/// A furnace's three slots, `56..=58` in this container's *storage*.
///
/// **A mirror, not the furnace.** The furnace's items live in the world, in
/// `net::furnaces`, because they go on existing when every player logs out and
/// they go on smelting while nobody is watching. What is here is one session's
/// copy of them, refreshed from the world at the top of every click and
/// written back at the bottom of it, under the same lock — see
/// [`Inventory::with_furnace`].
///
/// Keeping them here rather than in a menu of their own is the same argument
/// [`TABLE_GRID_START`] makes: it is what lets one implementation of the seven
/// click modes serve a third window, and the modes are at 101 of 101 against a
/// real server because there is one of them.
pub const FURNACE_START: usize = 56;
/// The furnace's input, the slot the fire cooks.
pub const FURNACE_INPUT: usize = 56;
/// The furnace's fuel.
pub const FURNACE_FUEL: usize = 57;
/// The furnace's output. Takes nothing and gives what the fire made.
pub const FURNACE_OUTPUT: usize = 58;
/// One past the furnace's slots.
pub const FURNACE_END: usize = 59;

/// A stonecutter's two slots, `59..=60` in this container's *storage*.
///
/// The player's, not a block's — unlike a furnace's three. A stonecutter holds
/// nothing when nobody is standing at it: vanilla's `StonecutterMenu.removed`
/// empties the input back into the player, and so does
/// [`Inventory::closed`].
pub const CUT_START: usize = 59;
/// The stonecutter's input, the block being cut.
pub const CUT_INPUT: usize = 59;
/// The stonecutter's output: whichever of the buttons is pressed.
pub const CUT_OUTPUT: usize = 60;
/// One past the stonecutter's slots.
pub const CUT_END: usize = 61;

/// A smithing table's four slots, `61..=64` in this container's *storage*.
///
/// Template, base, addition, result — vanilla's own order, left to right on
/// the screen, and the order is load-bearing: the same three items in
/// different slots are not a recipe, which is measured against a real 1.21.1
/// server in `tools/bot/benches.js`.
pub const SMITH_START: usize = 61;
/// The smithing table's template slot.
pub const SMITH_TEMPLATE: usize = 61;
/// The smithing table's base slot: the thing being upgraded.
pub const SMITH_BASE: usize = 62;
/// The smithing table's addition slot: the netherite.
pub const SMITH_ADDITION: usize = 63;
/// The smithing table's result.
pub const SMITH_OUTPUT: usize = 64;
/// One past the smithing table's slots.
pub const SMITH_END: usize = 65;

/// Every slot this container stores: the player's forty-six, the ten a
/// crafting table adds, the three a furnace does, a stonecutter's two and a
/// smithing table's four.
///
/// **Sixty-five, which is over a `u64`.** [`Changed`] is a `u128` for that
/// reason. The alternative was to overlay the three benches on one region,
/// since a player can have only one screen open — six slots and about a
/// hundred bytes a player saved. It was not taken: an aliased slot is a
/// leftover from the last screen appearing in the next one, which is an item
/// duplication bug with no symptom until somebody notices free diamonds, and
/// a wider register on a value that is returned once per click costs nothing
/// measurable.
pub const STORAGE: usize = 65;

/// The slot number a click outside the window carries.
pub const OUTSIDE: i16 = -999;

/// How many slots `minecraft:set_equipment` can carry for a player: the main
/// hand, the offhand, and the four armour pieces.
///
/// The protocol has a seventh, `Body`, which is a horse's barding and a wolf's
/// armour. A player never has one, so a player's equipment array never carries
/// it and no packet ever names it.
pub const EQUIPMENT_SLOTS: usize = 6;

/// What everybody except its owner can see of an inventory, indexed by the
/// slot numbers `minecraft:set_equipment` uses.
///
/// Those numbers are the protocol's, not this container's, which is why the
/// boots come before the helmet and the hand comes before either. Indexing by
/// the wire's own numbering means the diff below is the packet's payload with
/// no second table in between.
pub type Equipment = [Option<Stack>; EQUIPMENT_SLOTS];

/// The wire slot number of the main hand.
pub const EQUIP_MAIN_HAND: u8 = 0;
/// The wire slot number of the offhand.
pub const EQUIP_OFF_HAND: u8 = 1;
/// The wire slot number of the boots.
pub const EQUIP_BOOTS: u8 = 2;
/// The wire slot number of the leggings.
pub const EQUIP_LEGGINGS: u8 = 3;
/// The wire slot number of the chestplate.
pub const EQUIP_CHESTPLATE: u8 = 4;
/// The wire slot number of the helmet.
pub const EQUIP_HELMET: u8 = 5;

/// Which container slot each equipment slot reads, in wire order.
///
/// A flat six-entry table rather than a `match`, because it is walked whole on
/// every inventory change and the compiler can unroll six indexed reads.
const EQUIPMENT_SOURCE: [usize; EQUIPMENT_SLOTS] = [
    // The main hand is not a fixed slot: it is whichever hotbar slot is
    // selected, so this entry is a placeholder the reader replaces.
    usize::MAX,
    OFFHAND,
    ARMOUR_FEET,
    ARMOUR_LEGS,
    ARMOUR_CHEST,
    ARMOUR_HEAD,
];

/// One equipment slot and what is now in it, ready to become a wire entry.
pub type EquipmentChange = (u8, Option<Stack>);

/// The `button` a swap click uses to mean the offhand rather than a hotbar
/// slot. Vanilla's `Inventory.SLOT_OFFHAND`, and it is 40 rather than 45
/// because a swap's button numbers the *hotbar* and offhand is bolted onto the
/// end of that numbering.
const SWAP_OFFHAND_BUTTON: i8 = 40;

/// Where an item is *worn*, as a slot number in this container's numbering, or
/// `None` for the overwhelming majority of items that are worn nowhere.
///
/// This is the table [`Inventory::click`]'s armour rules are missing without,
/// and it is the reason the header used to say shift-click does not equip.
/// Java answers the same question with `Mob.getEquipmentSlotForItem`, which
/// walks a class hierarchy — `ArmorItem` knows its own type, `ShieldItem` is
/// hard-wired to the offhand — and a class hierarchy is not in any report. The
/// item report does not help either: on 1.21.1 every armour piece's
/// `minecraft:attribute_modifiers` is an **empty list**, so the report can say
/// how much damage a helmet absorbs nowhere and which slot it goes in nowhere.
/// The `minecraft:equippable` component that would answer this outright is
/// 1.21.2 and later.
///
/// What does answer it, on this version, is Mojang's own item tags, which
/// arrive through the same extraction as everything else:
///
/// | tag | slot |
/// |---|---|
/// | `minecraft:head_armor` | head |
/// | `minecraft:chest_armor` | chest |
/// | `minecraft:leg_armor` | legs |
/// | `minecraft:foot_armor` | feet |
/// | `minecraft:skulls` | head |
///
/// That is 32 of the 34 items a player can wear. The last two —
/// `minecraft:elytra` on the chest and `minecraft:carved_pumpkin` on the head —
/// are in no tag that names a slot, so they are named here, as is
/// `minecraft:shield`, which goes in the offhand.
///
/// **Names written down are names that can go stale, so they are guarded
/// rather than trusted.** `minecraft:enchantable/equippable` is vanilla's own
/// list of everything that is worn: the four armour tags, the skulls, the
/// elytra and the carved pumpkin, 34 items in all.
/// `every_wearable_item_has_a_slot_to_be_worn_in` walks that tag and fails on
/// any member this table places nowhere — so a version that adds a wearable
/// stops the build on the row where it happened, rather than shipping an item
/// a player cannot put on. The shield is not in that tag, because a shield is
/// held rather than worn, and is checked by name on its own.
///
/// # Cost
///
/// One byte per item, 1,333 of them, built once on the first click of the
/// server's life and read as an array index afterwards. The alternative — a
/// tag lookup per click — is five binary searches over a 514-row table on a
/// path a player hits several times a second.
fn worn_in(item: Item) -> Option<usize> {
    static WORN: OnceLock<Box<[u8]>> = OnceLock::new();
    let table = WORN.get_or_init(build_worn_table);
    // `CRAFTING_OUTPUT` is slot 0 and nothing is worn there, so zero is free to
    // mean "worn nowhere" and the table needs no `Option` per row.
    match table.get(item.protocol_id() as usize).copied() {
        None | Some(0) => None,
        Some(slot) => Some(slot as usize),
    }
}

/// The tags that name a slot, and which slot each names.
const WORN_BY_TAG: [(&str, usize); 5] = [
    ("minecraft:head_armor", ARMOUR_START),
    ("minecraft:chest_armor", ARMOUR_START + 1),
    ("minecraft:leg_armor", ARMOUR_START + 2),
    ("minecraft:foot_armor", ARMOUR_START + 3),
    ("minecraft:skulls", ARMOUR_START),
];

/// The three 1.21.1 leaves no tag for. See [`worn_in`].
const WORN_BY_NAME: [(&str, usize); 3] = [
    ("minecraft:elytra", ARMOUR_START + 1),
    ("minecraft:carved_pumpkin", ARMOUR_START),
    ("minecraft:shield", OFFHAND),
];

fn build_worn_table() -> Box<[u8]> {
    let mut table = vec![0u8; Item::registry().entry_count()];
    let mut put = |name: &str, slot: usize| {
        if let Some(item) = Item::from_name(name) {
            table[item.protocol_id() as usize] = slot as u8;
        }
    };
    for (tag, slot) in WORN_BY_TAG {
        let Some(def) = tags::from_id(TagRegistry::Item, tag) else {
            continue;
        };
        for member in def.members {
            put(member, slot);
        }
    }
    for (name, slot) in WORN_BY_NAME {
        put(name, slot);
    }
    table.into_boxed_slice()
}

/// The most of `item` one slot will hold.
///
/// Vanilla's `Slot.getMaxStackSize(ItemStack)`, which is the item's own maximum
/// everywhere in this container except the four armour slots, where `ArmorSlot`
/// returns 1. That is not a formality: `minecraft:player_head` stacks to 64 and
/// is worn on the head, so a player left-clicking a stack of sixty-four heads
/// onto the helmet slot puts **one** there and keeps sixty-three on the cursor.
/// A container that used the item's number would swallow the stack.
fn slot_limit(index: usize, item: Item) -> u8 {
    if (ARMOUR_START..ARMOUR_END).contains(&index) {
        1
    } else {
        item.max_stack_size()
    }
}

/// Whether a click may put `item` in this slot — vanilla's `Slot.mayPlace`.
///
/// Three answers, and the middle one is the whole point of [`worn_in`]: the
/// crafting output takes nothing, an armour slot takes only what is worn in
/// *that* slot, and everything else — the offhand included — takes anything.
/// The offhand really is unrestricted: a real server accepts a stack of nine
/// cobblestone into slot 45, which is measured in `tools/bot/clicks.js` and is
/// not a guess about what looks sensible.
fn may_place(
    index: usize,
    item: Item,
    fuel: Option<&ItemBlocks>,
    smithing: Option<&dust_sim::smithing::Smithing>,
) -> bool {
    if index == CRAFTING_OUTPUT
        || index == TABLE_OUTPUT
        || index == FURNACE_OUTPUT
        || index == CUT_OUTPUT
        || index == SMITH_OUTPUT
    {
        return false;
    }
    // `SmithingMenu.createInputSlotDefinitions`: each of the three slots
    // accepts only what *some loaded recipe* names for that position, which is
    // why a diamond will not go in the template slot on a real server.
    //
    // A server with no recipes takes anything, for the reason the fuel slot
    // does: an empty table has no opinion, and three slots that refused
    // everything would be a bench a player cannot even load.
    if (SMITH_TEMPLATE..SMITH_OUTPUT).contains(&index) {
        let Some(smithing) = smithing.filter(|table| !table.is_empty()) else {
            return true;
        };
        return smithing.iter().any(|one| match index {
            SMITH_TEMPLATE => one.template_items().any(|allowed| allowed == item),
            SMITH_BASE => one.base_items().any(|allowed| allowed == item),
            _ => one.addition_items().any(|allowed| allowed == item),
        });
    }
    if (ARMOUR_START..ARMOUR_END).contains(&index) {
        return worn_in(item) == Some(index);
    }
    // `FurnaceFuelSlot.mayPlace`: something that burns, or a bucket. The
    // bucket is there because a lava bucket burns and leaves an empty one
    // behind, and a player must be able to take that empty one back out and
    // put a full one in without the slot arguing.
    //
    // A server whose table has no `burn` column does **not** refuse
    // everything: it has no opinion, and a fuel slot that took nothing would
    // be a furnace that cannot be lit at all. See `ItemBlocks::has_burn` —
    // "the table does not know" is not "this item does not burn".
    if index == FURNACE_FUEL {
        let Some(fuel) = fuel.filter(|table| table.has_burn()) else {
            return true;
        };
        return fuel.burn(item).is_some() || item.name() == "minecraft:bucket";
    }
    true
}

/// Where one pass of a shift-click may send a stack, in order, with whether
/// each is filled from the far end.
///
/// Two at most, because vanilla's longest arm is two `moveItemStackTo` calls
/// and a fixed array costs no allocation on a path that runs per click.
type Destinations = [Option<(std::ops::Range<usize>, bool)>; 2];

/// Whether a click may write this storage slot at all.
///
/// The two crafting outputs are not among them: a click there takes the result
/// of a recipe and pays for it out of the grid, which is
/// [`Inventory::pickup_result`] and not a write.
fn writable(index: usize) -> bool {
    index != CRAFTING_OUTPUT
        && index != TABLE_OUTPUT
        && index != CUT_OUTPUT
        && index != SMITH_OUTPUT
        && index < STORAGE
}

/// Whether this slot is one of a furnace's three.
///
/// The question a save asks, and a close: those three belong to a block, and a
/// player who walks away from a furnace must not take its contents with them.
#[must_use]
pub fn is_furnace_slot(index: usize) -> bool {
    (FURNACE_START..FURNACE_END).contains(&index)
}

/// One stack: an item, how many of it, and what makes it that one.
///
/// The count is never zero — an empty slot is `None`, not a stack of nothing —
/// and never above the item's own maximum. Both are invariants of every
/// constructor and every mutation here, which is what lets the rest of this
/// module do arithmetic without re-checking.
///
/// The third field is the stack's data components: its name, its enchantments,
/// how worn it is, what is inside it. It is one `Option<Arc<[u8]>>` — `None`
/// for the overwhelming majority of stacks, which allocate nothing — and it is
/// what [`stacks_with`] compares, because **two stacks merge only if their
/// components are equal**. Getting that comparison wrong in one direction
/// duplicates items and in the other destroys them; see
/// [`dust_protocol::components`] for why it is byte equality and which of the
/// two directions that can fail in.
///
/// [`stacks_with`]: Stack::stacks_with
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    pub item: Item,
    pub count: u8,
    pub components: ComponentPatch,
}

impl Stack {
    /// A stack of `count`, clamped to what the item allows and to at least one.
    #[must_use]
    pub fn new(item: Item, count: u8) -> Self {
        Self::with_components(item, count, ComponentPatch::EMPTY)
    }

    /// The same, carrying components.
    #[must_use]
    pub fn with_components(item: Item, count: u8, components: ComponentPatch) -> Self {
        Self {
            item,
            count: count.clamp(1, item.max_stack_size()),
            components,
        }
    }

    /// Whether these two stacks are the same thing, and may therefore pour into
    /// one another.
    ///
    /// The item **and** the components. A stack of sixteen arrows and a stack
    /// of sixteen arrows named "Bob" are two different things in Minecraft and
    /// two different things here; merging them would take Bob's name off
    /// sixteen arrows and give it to none, which the player sees as the server
    /// tidying their inventory by destroying part of it.
    #[must_use]
    pub fn stacks_with(&self, other: &Self) -> bool {
        self.item == other.item && self.components == other.components
    }

    /// A copy of this stack with a different count, components and all.
    #[must_use]
    fn of(&self, count: u8) -> Self {
        Self {
            item: self.item,
            count,
            components: self.components.clone(),
        }
    }

    /// Whether the stack is at the item's own maximum. Not the same question
    /// as whether the *slot* it is in is full — see [`slot_limit`] — and the
    /// two callers left are the double-click gather, which is about the cursor
    /// and about stacks it will not break open.
    fn is_full(&self) -> bool {
        self.count >= self.item.max_stack_size()
    }
}

/// The slots of one player's container.
pub type Slots = [Option<Stack>; SLOTS];

/// How many slots a furnace's window numbers: three, then the player's
/// twenty-seven and nine.
pub const FURNACE_SLOT_COUNT: usize = 39;

/// How many slots a stonecutter's window numbers: input, result, then the
/// player's thirty-six.
pub const CUT_SLOT_COUNT: usize = 38;

/// How many slots a smithing table's window numbers: template, base, addition,
/// result, then the player's thirty-six.
pub const SMITH_SLOT_COUNT: usize = 40;

/// Which window a click names, and therefore what its slot numbers mean.
///
/// A window is a *numbering*, not a container. Both of these are views onto
/// the same [`Inventory`], which is why the seven click modes are written once
/// — they work in storage indices, and the only thing a window changes is
/// which storage index a wire slot number reaches and where a shift-click
/// sends it. A second implementation of the modes for the crafting table would
/// be two readers of one set of rules, and the pair would drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The player's own container, `0..=45`, which is always open.
    Player,
    /// A crafting table: `0` the result, `1..=9` the grid, `10..=36` the
    /// player's main inventory and `37..=45` their hotbar. No armour and no
    /// offhand — a crafting table cannot see either.
    Table,
    /// A furnace, a blast furnace or a smoker: `0` the input, `1` the fuel,
    /// `2` the result, `3..=29` the player's main inventory and `30..=38`
    /// their hotbar. **Thirty-nine slots, not forty-six** — the first window
    /// here that is not the same size as the others, which is why
    /// [`Window::slot_count`] exists at all.
    ///
    /// The three slots in front belong to the block, not the player. They are
    /// mirrored into [`FURNACE_START`] for the duration of a click; see there.
    Furnace,
    /// A stonecutter: `0` the input, `1` the result, `2..=28` the player's
    /// main inventory and `29..=37` their hotbar.
    ///
    /// The result is not a slot a player puts anything in and not a slot the
    /// server fills on its own: it is whichever of the buttons was last
    /// pressed, which is why this window has a *selection* and no other does.
    Stonecutter,
    /// A smithing table: `0` template, `1` base, `2` addition, `3` the result,
    /// `4..=30` the player's main inventory and `31..=39` their hotbar.
    Smithing,
}

impl Window {
    /// How many slots this window numbers.
    ///
    /// Forty-six for the player's own and a crafting table, which are not the
    /// same forty-six; thirty-nine for a furnace, which has three slots in
    /// front of the player's thirty-six instead of ten. A container sent at
    /// the wrong length is a screen whose last row is somewhere else.
    #[must_use]
    pub fn slot_count(self) -> usize {
        match self {
            Self::Player | Self::Table => SLOTS,
            Self::Furnace => FURNACE_SLOT_COUNT,
            Self::Stonecutter => CUT_SLOT_COUNT,
            Self::Smithing => SMITH_SLOT_COUNT,
        }
    }

    /// The storage index a wire slot number reaches, or `None` for a number
    /// this window does not have.
    #[must_use]
    pub fn storage(self, slot: usize) -> Option<usize> {
        match self {
            Self::Player => (slot < SLOTS).then_some(slot),
            Self::Table => Some(match slot {
                0 => TABLE_OUTPUT,
                1..=9 => TABLE_GRID_START + slot - 1,
                10..=36 => MAIN_START + slot - 10,
                37..=45 => HOTBAR_START + slot - 37,
                _ => return None,
            }),
            Self::Furnace => Some(match slot {
                0 => FURNACE_INPUT,
                1 => FURNACE_FUEL,
                2 => FURNACE_OUTPUT,
                3..=29 => MAIN_START + slot - 3,
                30..=38 => HOTBAR_START + slot - 30,
                _ => return None,
            }),
            Self::Stonecutter => Some(match slot {
                0 => CUT_INPUT,
                1 => CUT_OUTPUT,
                2..=28 => MAIN_START + slot - 2,
                29..=37 => HOTBAR_START + slot - 29,
                _ => return None,
            }),
            Self::Smithing => Some(match slot {
                0 => SMITH_TEMPLATE,
                1 => SMITH_BASE,
                2 => SMITH_ADDITION,
                3 => SMITH_OUTPUT,
                4..=30 => MAIN_START + slot - 4,
                31..=39 => HOTBAR_START + slot - 31,
                _ => return None,
            }),
        }
    }

    /// The wire slot number a storage index appears at, or `None` for a slot
    /// this window cannot see — the armour and the offhand, to a table.
    #[must_use]
    pub fn wire(self, storage: usize) -> Option<usize> {
        match self {
            Self::Player => (storage < SLOTS).then_some(storage),
            Self::Table => Some(match storage {
                TABLE_OUTPUT => 0,
                TABLE_GRID_START..=54 => storage - TABLE_GRID_START + 1,
                MAIN_START..=35 => storage - MAIN_START + 10,
                HOTBAR_START..=44 => storage - HOTBAR_START + 37,
                _ => return None,
            }),
            Self::Furnace => Some(match storage {
                FURNACE_INPUT => 0,
                FURNACE_FUEL => 1,
                FURNACE_OUTPUT => 2,
                MAIN_START..=35 => storage - MAIN_START + 3,
                HOTBAR_START..=44 => storage - HOTBAR_START + 30,
                _ => return None,
            }),
            Self::Stonecutter => Some(match storage {
                CUT_INPUT => 0,
                CUT_OUTPUT => 1,
                MAIN_START..=35 => storage - MAIN_START + 2,
                HOTBAR_START..=44 => storage - HOTBAR_START + 29,
                _ => return None,
            }),
            Self::Smithing => Some(match storage {
                SMITH_TEMPLATE => 0,
                SMITH_BASE => 1,
                SMITH_ADDITION => 2,
                SMITH_OUTPUT => 3,
                MAIN_START..=35 => storage - MAIN_START + 4,
                HOTBAR_START..=44 => storage - HOTBAR_START + 31,
                _ => return None,
            }),
        }
    }

    /// The output slot this window's grid fills, and the grid that fills it —
    /// or `None` for a window with no grid.
    ///
    /// An `Option` and not a sentinel pair. A furnace has an output slot and
    /// it is emphatically not a crafting output: nothing about the three items
    /// in front of a player says what comes out, the fire does, and a window
    /// that answered here would have its result recomputed to *nothing* on the
    /// first click that moved the input.
    fn crafting(self) -> Option<(usize, std::ops::Range<usize>, usize)> {
        match self {
            Self::Player => Some((
                CRAFTING_OUTPUT,
                CRAFTING_START..CRAFTING_END,
                CRAFTING_WIDTH,
            )),
            Self::Table => Some((TABLE_OUTPUT, TABLE_GRID_START..TABLE_GRID_END, TABLE_WIDTH)),
            Self::Furnace | Self::Stonecutter | Self::Smithing => None,
        }
    }

    /// The slot a *recipe* fills, which a click on has to pay for out of the
    /// grid. `None` for a window with no grid.
    ///
    /// **A furnace's output is not one of these**, and it looked like one for
    /// long enough to be measured: a shift-click on it went down the crafting
    /// path, which loops "craft again until the inputs run out" and pays out
    /// of a grid a furnace does not have, so it took nothing and the ingots
    /// stayed where they were. A furnace's output is an ordinary slot that
    /// refuses everything put into it — the fire filled it, and it is already
    /// paid for.
    fn crafting_output(self) -> Option<usize> {
        match self {
            Self::Player => Some(CRAFTING_OUTPUT),
            Self::Table => Some(TABLE_OUTPUT),
            // Both benches, and a furnace not. The difference is who paid: a
            // stonecutter's result and a smithing table's result are pictures
            // of what the slots in front of the player *would* make and are
            // spent out of those slots when they are taken, exactly as a
            // grid's is. A furnace's ingot is already made, out of coal that
            // is already burnt, and taking it costs nothing.
            Self::Stonecutter => Some(CUT_OUTPUT),
            Self::Smithing => Some(SMITH_OUTPUT),
            Self::Furnace => None,
        }
    }

    /// The output slot, whatever filled it.
    fn output(self) -> usize {
        match self {
            Self::Player => CRAFTING_OUTPUT,
            Self::Table => TABLE_OUTPUT,
            Self::Furnace => FURNACE_OUTPUT,
            Self::Stonecutter => CUT_OUTPUT,
            Self::Smithing => SMITH_OUTPUT,
        }
    }
}

/// Which slots a click moved, as a bitmask.
///
/// Fifty-six slots fit in a `u64` with room to spare, so "what changed" is a
/// register rather than a `Vec`. That is not a micro-optimisation for its own
/// sake: this is returned from every click, and a click that allocated to
/// report that one slot moved would allocate once per click per player.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changed {
    slots: u128,
    cursor: bool,
}

impl Changed {
    fn mark(&mut self, slot: usize) {
        debug_assert!(slot < STORAGE);
        self.slots |= 1u128 << slot;
    }

    fn mark_cursor(&mut self) {
        self.cursor = true;
    }

    /// Whether this slot moved.
    #[must_use]
    pub fn has(self, slot: usize) -> bool {
        slot < STORAGE && self.slots & (1u128 << slot) != 0
    }

    /// Whether the cursor moved.
    #[must_use]
    pub fn cursor(self) -> bool {
        self.cursor
    }

    /// Whether nothing at all moved.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.slots == 0 && !self.cursor
    }

    /// Both of these changes, as one.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        Self {
            slots: self.slots | other.slots,
            cursor: self.cursor || other.cursor,
        }
    }

    /// The slots that moved, in ascending order.
    pub fn iter(self) -> impl Iterator<Item = usize> {
        (0..STORAGE).filter(move |slot| self.has(*slot))
    }
}

/// A drag in progress: the mouse is down and slots are being collected.
///
/// Vanilla calls this "quick craft" and it is a three-packet handshake — start,
/// each slot, end — which means a client that disconnects mid-drag, or one
/// sending the steps out of order, leaves state behind. Anything unexpected
/// resets it rather than being interpreted, which is vanilla's own rule and the
/// only safe one: a half-remembered drag applied to a later click is items
/// appearing where nobody put them.
#[derive(Debug, Clone, Copy, Default)]
struct Drag {
    active: bool,
    /// 0 left (split evenly), 1 right (one each), 2 middle (a full stack each,
    /// creative only).
    kind: u8,
    /// The slots collected so far, as a bitmask, for the same reason
    /// [`Changed`] is one — and the same width, because a smithing table's
    /// result is storage slot 64 and `1u64 << 64` is not a shift, it is a
    /// panic.
    slots: u128,
    count: u8,
}

impl Drag {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn add(&mut self, slot: usize) {
        if self.slots & (1u128 << slot) == 0 {
            self.slots |= 1u128 << slot;
            self.count += 1;
        }
    }
}

/// Everything one player is carrying.
#[derive(Debug, Clone)]
pub struct Inventory {
    slots: [Option<Stack>; STORAGE],
    /// What the player has picked up with the mouse. Not a slot: it belongs to
    /// the click protocol rather than the container, and it is sent in its own
    /// field of every packet that carries the container.
    cursor: Option<Stack>,
    /// Which hotbar slot is in hand, `0..9`.
    selected: usize,
    drag: Drag,
    /// The sequence number the client quotes back on a click. The server's
    /// alone: a click carrying a stale one was made against a window that has
    /// since moved.
    state_id: i32,
    /// What this player's 2x2 grid can make. Shared with every other session:
    /// it is read-only after boot and one `Arc` clone per login is the whole
    /// per-player cost.
    ///
    /// `None` is a server with no `[data] path`, and it is not an error — it
    /// is the server this was before crafting, with a grid that stores and an
    /// output that never fills. A recipe table that was absent must not be
    /// read as "no recipe matched", because the two would look the same to
    /// every caller and one of them is a misconfiguration.
    recipes: Option<Arc<Recipes>>,
    /// What the four fires cook, for the one question a shift-click asks: does
    /// the open furnace's fire cook this? Shared, like the recipes.
    cooking: Option<Arc<dust_sim::cooking::Cooking>>,
    /// Which fire the open furnace is, if one is open. Set on the open and
    /// cleared on the close, because a blast furnace and a smoker read
    /// different tables and a shift-click has to ask the right one.
    fire: Option<dust_sim::cooking::Fire>,
    /// The item table, for the one question a slot asks of it: does this burn?
    ///
    /// Shared with every other session for the same reason the recipes are.
    /// `None` is a server with no `[data] path`, and a fuel slot then takes
    /// anything rather than nothing — see [`may_place`].
    fuel: Option<Arc<ItemBlocks>>,
    /// What a stonecutter cuts, in the order its buttons are drawn. Shared,
    /// like the recipes.
    cutting: Option<Arc<dust_sim::cutting::Cutting>>,
    /// What a smithing table upgrades. Shared, and read by [`may_place`] as
    /// well as by the result: the three input slots each accept only what some
    /// recipe names for that position.
    smithing: Option<Arc<dust_sim::smithing::Smithing>>,
    /// Which button of the open stonecutter is pressed, or `None` for none.
    ///
    /// **A position in a list, not a recipe.** Vanilla's own
    /// `StonecutterMenu.selectedRecipeIndex` is the same thing and it is the
    /// number the client sends; keeping the recipe instead would be a
    /// different value that happens to agree until the input changes.
    cut_choice: Option<usize>,
    /// What was in the stonecutter's input when the list was last built.
    ///
    /// The selection survives the *count* changing and not the *item*
    /// changing, which is `StonecutterMenu.slotsChanged` exactly — and it is
    /// what lets a player take eight slabs out in a row without pressing the
    /// button again.
    cut_input: Option<Item>,
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            cursor: None,
            selected: 0,
            drag: Drag::default(),
            state_id: 0,
            recipes: None,
            cooking: None,
            fire: None,
            fuel: None,
            cutting: None,
            smithing: None,
            cut_choice: None,
            cut_input: None,
        }
    }
}

impl Inventory {
    /// An inventory holding what was saved.
    #[must_use]
    pub fn restored(slots: Slots, selected: u8) -> Self {
        let mut inventory = Self {
            selected: usize::from(selected).min(HOTBAR_SLOTS - 1),
            ..Self::default()
        };
        for (index, stack) in slots.into_iter().enumerate() {
            inventory.slots[index] = stack;
        }
        inventory
    }

    /// The same container, able to craft.
    ///
    /// Called once per login. The recipes outlive every session, so this is an
    /// `Arc` clone and not a copy of 887 recipes.
    /// The same container, able to tell fuel from everything else.
    ///
    /// Called once per login beside [`Inventory::crafting_with`], and an `Arc`
    /// clone for the same reason.
    #[must_use]
    pub fn burning_with(
        mut self,
        fuel: Arc<ItemBlocks>,
        cooking: Arc<dust_sim::cooking::Cooking>,
    ) -> Self {
        self.fuel = Some(fuel);
        self.cooking = Some(cooking);
        self
    }

    /// The two benches' tables, shared with every other session.
    #[must_use]
    pub fn at_benches(
        mut self,
        cutting: Arc<dust_sim::cutting::Cutting>,
        smithing: Arc<dust_sim::smithing::Smithing>,
    ) -> Self {
        self.cutting = Some(cutting);
        self.smithing = Some(smithing);
        self
    }

    /// Press one of the stonecutter's buttons, and say what moved.
    ///
    /// An index outside the list is **ignored, keeping the last selection** —
    /// vanilla's `StonecutterMenu.clickMenuButton` guards with
    /// `isValidRecipeIndex` and does nothing when it fails, which is measured:
    /// pressing buttons 6 through 23 with andesite in a real 1.21.1 server
    /// leaves button 5's polished andesite stairs in the result slot rather
    /// than emptying it.
    pub fn choose_cut(&mut self, index: i32) -> Changed {
        let mut changed = Changed::default();
        let Ok(index) = usize::try_from(index) else {
            return changed;
        };
        if index >= self.cut_list().len() {
            return changed;
        }
        self.cut_choice = Some(index);
        self.refresh_cut(&mut changed);
        changed
    }

    /// Which button is pressed, for the property the screen draws its
    /// highlight from. `-1` is vanilla's own "none".
    #[must_use]
    pub fn cut_choice(&self) -> i32 {
        self.cut_choice.map_or(-1, |index| index as i32)
    }

    /// What the stonecutter's input can be cut into, in button order.
    fn cut_list(&self) -> &[dust_sim::cutting::Cut] {
        let Some(cutting) = self.cutting.as_ref() else {
            return &[];
        };
        let Some(input) = self.slots[CUT_INPUT].as_ref() else {
            return &[];
        };
        cutting.cuts_of(input.item)
    }

    /// Which fire the open furnace is, if one is open.
    #[must_use]
    pub fn fire(&self) -> Option<dust_sim::cooking::Fire> {
        self.fire
    }

    /// Which fire the open furnace is. `None` closes one.
    pub fn at_fire(&mut self, fire: Option<dust_sim::cooking::Fire>) {
        self.fire = fire;
    }

    /// Copy a furnace's three slots in, marking what a watching client would
    /// see move.
    ///
    /// Called at the **top** of every click on a furnace window, under the
    /// furnace world's lock. The mirror is stale between clicks — the fire
    /// goes on working — and refreshing it here is what stops a click acting
    /// on a picture of a furnace that has since produced an ingot. A click
    /// that then overwrote the slots from a stale mirror would delete it.
    pub fn mirror_furnace(&mut self, slots: &[Option<Stack>]) -> Changed {
        let mut changed = Changed::default();
        for (offset, stack) in slots.iter().take(FURNACE_END - FURNACE_START).enumerate() {
            let index = FURNACE_START + offset;
            if self.slots[index] != *stack {
                self.slots[index] = stack.clone();
                changed.mark(index);
            }
        }
        changed
    }

    /// The three slots as the mirror now holds them, to be written back.
    #[must_use]
    pub fn furnace_slots(&self) -> [Option<Stack>; 3] {
        [
            self.slots[FURNACE_INPUT].clone(),
            self.slots[FURNACE_FUEL].clone(),
            self.slots[FURNACE_OUTPUT].clone(),
        ]
    }

    /// Forget the mirror, so nothing of one furnace is carried to the next.
    ///
    /// Not a cosmetic tidy: the mirror slots are storage indices like any
    /// other, and a shift-click aimed at `MAIN_START..HOTBAR_END` cannot reach
    /// them — but [`Inventory::give`] walks a range and a future one might.
    /// Leaving a stack of somebody else's iron in a slot no window can see is
    /// the kind of thing that turns up as a duplication bug a year later.
    pub fn clear_furnace_mirror(&mut self) {
        for index in FURNACE_START..FURNACE_END {
            self.slots[index] = None;
        }
        self.fire = None;
    }

    #[must_use]
    pub fn crafting_with(mut self, recipes: Arc<Recipes>) -> Self {
        self.recipes = Some(recipes);
        // A restored container can have something already in its grid: a
        // player who logged out mid-craft comes back to the output they left,
        // rather than to an empty slot that fills only once they touch it.
        self.refresh_output(Window::Player, &mut Changed::default());
        self
    }

    /// The player's own forty-six slots, in vanilla's numbering. Borrowed,
    /// never copied: this is read to build the join packet and to write the
    /// save.
    ///
    /// The ten a crafting table adds are deliberately not among them. They are
    /// the table's, not the player's, and a save that wrote them would restore
    /// a player holding the contents of a block they were standing at.
    #[must_use]
    pub fn slots(&self) -> &[Option<Stack>] {
        &self.slots[..SLOTS]
    }

    /// The same forty-six as an array, for the save record that holds one —
    /// **with anything in an open crafting table's grid folded back in.**
    ///
    /// A player who is dropped mid-craft never gets a close packet, and the
    /// nine slots of a table's grid are not theirs to save under their own
    /// name. Folding is the only reading of "what this player owns" that
    /// cannot lose an item: the fold is a projection and is never written back
    /// here, so a player who *does* close the window has the same items moved
    /// into the same slots and recorded again, with nothing doubled.
    ///
    /// The fold costs a clone of the container and only happens when the grid
    /// holds something, which is a few seconds of a session at most.
    #[must_use]
    pub fn saved(&self) -> Slots {
        if self.slots[TABLE_GRID_START..TABLE_GRID_END]
            .iter()
            .all(Option::is_none)
        {
            return std::array::from_fn(|index| self.slots[index].clone());
        }
        let mut folded = self.clone();
        folded.closed(Window::Table);
        std::array::from_fn(|index| folded.slots[index].clone())
    }

    /// What one slot holds. Borrowed: a stack carries its components and a
    /// caller that only wants to look at one should not touch their refcount.
    #[must_use]
    pub fn slot(&self, index: usize) -> Option<&Stack> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    /// What is on the cursor.
    #[must_use]
    pub fn cursor(&self) -> Option<&Stack> {
        self.cursor.as_ref()
    }

    /// Which hotbar slot is in hand, `0..9`.
    #[must_use]
    pub fn selected(&self) -> u8 {
        self.selected as u8
    }

    /// The item in the selected hotbar slot, if there is one.
    #[must_use]
    pub fn held(&self) -> Option<Item> {
        self.held_stack().map(|stack| stack.item)
    }

    /// The whole stack in the selected hotbar slot, components and all.
    ///
    /// Borrowed, for the reason [`Inventory::slot`] is: what a break asks of
    /// this is whether the tool is enchanted, and that answer lives in an
    /// `Arc` nobody should touch the refcount of to read one byte.
    #[must_use]
    pub fn held_stack(&self) -> Option<&Stack> {
        self.slots[HOTBAR_START + self.selected].as_ref()
    }

    /// What everybody else can see of this inventory.
    ///
    /// Six clones of an `Option<Stack>`, which is an item, a count and a
    /// refcount bump on the component bytes — cheap enough to take on every
    /// change to the container and compare, which is what makes this the one
    /// place that has to know a helmet is worn on the head.
    #[must_use]
    pub fn equipment(&self) -> Equipment {
        std::array::from_fn(|wire_slot| {
            let index = if wire_slot == EQUIP_MAIN_HAND as usize {
                HOTBAR_START + self.selected
            } else {
                EQUIPMENT_SOURCE[wire_slot]
            };
            self.slots[index].clone()
        })
    }

    /// The sequence number to stamp on the next sync.
    #[must_use]
    pub fn state_id(&self) -> i32 {
        self.state_id
    }

    /// Advance and return the sequence number for a sync about to be sent.
    pub fn next_state_id(&mut self) -> i32 {
        self.state_id = self.state_id.wrapping_add(1);
        self.state_id
    }

    /// Switch to a hotbar slot.
    ///
    /// Returns whether the index named one. An out-of-range slot leaves the
    /// selection alone rather than wrapping: a client that sent 9 has said
    /// something this server does not understand, and picking slot 0 for it
    /// would be inventing an answer.
    pub fn select(&mut self, slot: i16) -> bool {
        let Ok(index) = usize::try_from(slot) else {
            return false;
        };
        if index >= HOTBAR_SLOTS {
            return false;
        }
        self.selected = index;
        true
    }

    /// A creative client writing a slot directly.
    ///
    /// Returns `Ok(slot)` for a write this server took, `Err(slot)` for one it
    /// refused and the client must be told about, and `Ok(None)` for a write
    /// that names no slot at all.
    ///
    /// Vanilla's own three rules, in vanilla's order:
    ///
    /// - **-1 drops the stack.** It is how a creative client throws something
    ///   out of the menu. There are no item entities, so it is destroyed.
    /// - **1..=45 is a write.** Slot 0 is the crafting output and vanilla
    ///   refuses a write to it, because a client that could write the output
    ///   of a recipe could conjure the result of any recipe.
    /// - **A count above the item's maximum is refused.** This is where
    ///   [`Item::max_stack_size`] earns its place: sixty-four buckets in one
    ///   slot is not something a client should be able to ask for, and the
    ///   number that says so is per-item and Minecraft's.
    pub fn set_creative(&mut self, slot: i16, item: &Slot) -> Result<Changed, usize> {
        let mut changed = Changed::default();
        if slot == -1 {
            // Thrown out of the creative menu. Nothing to report: the client
            // has already forgotten it.
            return Ok(changed);
        }
        let Ok(index) = usize::try_from(slot) else {
            return Ok(changed);
        };
        if !(CRAFTING_START..SLOTS).contains(&index) {
            return Ok(changed);
        }
        match decode(item) {
            Decoded::Empty => self.slots[index] = None,
            Decoded::Stack(stack) => self.slots[index] = Some(stack),
            // Refused, and the slot is left as it was. The client believes it
            // put something there, so the caller has to say otherwise.
            Decoded::TooMany | Decoded::UnknownItem => return Err(index),
        }
        changed.mark(index);
        // A creative write lands in the grid as readily as anywhere else, and
        // the output has to follow it. The client wrote the slot it named and
        // already draws that one; slot 0 it did not write, so the caller sends
        // that one back.
        if (CRAFTING_START..CRAFTING_END).contains(&index) {
            self.refresh_output(Window::Player, &mut changed);
        }
        Ok(changed)
    }

    /// Replay a `Click Container` over this state.
    ///
    /// `slot` is vanilla's number, or [`OUTSIDE`] for a click on the world
    /// behind the window. Returns what moved, so the caller can send back the
    /// slots the client is now wrong about rather than the whole container.
    ///
    /// A mode or button combination this does not understand changes nothing
    /// and reports nothing changed. That is deliberate and it is the safe
    /// direction: the caller re-syncs on a click it did not understand, which
    /// costs a packet, where guessing costs the player an item.
    pub fn click(&mut self, window: Window, mode: ClickMode, slot: i16, button: i8) -> Changed {
        let mut changed = Changed::default();
        // Any click that is not the next step of a drag ends the drag. Vanilla
        // does the same, and the reason is that the drag's three packets are
        // not atomic: a click arriving between them means the player did
        // something else, and finishing the drag afterwards would apply it to
        // a container they have already changed.
        if mode != ClickMode::QuickCraft && self.drag.active {
            self.drag.reset();
        }
        // Every mode below works in *storage* indices; the window is what
        // turns a wire number into one. A number this window does not have
        // reaches nothing and changes nothing.
        let named = usize::try_from(slot)
            .ok()
            .and_then(|slot| window.storage(slot));
        match mode {
            ClickMode::Pickup => self.pickup(window, slot, named, button, &mut changed),
            ClickMode::QuickMove => self.quick_move(window, named, &mut changed),
            ClickMode::Swap => self.swap(window, named, button, &mut changed),
            ClickMode::Clone => self.clone_slot(named, &mut changed),
            ClickMode::Throw => self.throw(window, named, button, &mut changed),
            ClickMode::QuickCraft => self.quick_craft(window, named, button, &mut changed),
            ClickMode::PickupAll => self.pickup_all(window, button, &mut changed),
        }
        // The output is a function of the grid, so it is recomputed whenever
        // the grid moved and never otherwise. A player arranging ingredients
        // makes one lookup per click that touches a grid slot, and none at all
        // for the other forty-one slots — which is the difference between an
        // index and a scan of every recipe on every click.
        //
        // Both grids are asked, not just the open window's: the player's own
        // 2x2 keeps whatever was in it while a table is open, and a click that
        // somehow moved it has to leave its output right.
        for window in [Window::Player, Window::Table] {
            let Some((_, grid, _)) = window.crafting() else {
                continue;
            };
            if grid.clone().any(|slot| changed.has(slot)) {
                self.refresh_output(window, &mut changed);
            }
        }
        // The two benches, on the same rule and for the same reason: their
        // results are functions of the slots in front of the player, and a
        // click that moved one of those slots has to leave the result right.
        if changed.has(CUT_INPUT) {
            self.refresh_cut(&mut changed);
        }
        if (SMITH_TEMPLATE..SMITH_OUTPUT).any(|slot| changed.has(slot)) {
            self.refresh_smithed(&mut changed);
        }
        changed
    }

    /// Put in slot 0 whatever the grid now makes, and say if that moved.
    ///
    /// A container with no recipes leaves the slot alone rather than emptying
    /// it: it has no opinion, and clearing a slot it cannot fill would be a
    /// server with no data path deleting whatever a save had put there.
    fn refresh_output(&mut self, window: Window, changed: &mut Changed) {
        match window {
            Window::Stonecutter => return self.refresh_cut(changed),
            Window::Smithing => return self.refresh_smithed(changed),
            _ => {}
        }
        let Some(recipes) = self.recipes.as_ref() else {
            return;
        };
        let Some((output, grid, width)) = window.crafting() else {
            return;
        };
        // Nine at most, and a 2x2 uses the first four. A fixed array rather
        // than a `Vec`, because this runs on every click that moves a grid
        // slot and a lookup that allocated would allocate per keystroke.
        let mut cells = [None; TABLE_WIDTH * TABLE_WIDTH];
        let mut filled = 0;
        for slot in grid {
            cells[filled] = self.slots[slot].as_ref().map(|stack| stack.item);
            filled += 1;
        }
        let height = filled / width;
        let made = recipes.find(width, height, &cells[..filled]).map(|recipe| {
            let (item, count) = recipe.result();
            Stack::new(item, count)
        });
        if self.slots[output] != made {
            self.slots[output] = made;
            changed.mark(output);
        }
    }

    /// Put in the stonecutter's result slot whatever button is pressed makes.
    ///
    /// Vanilla's `StonecutterMenu.slotsChanged` plus `setupResultSlot`, and
    /// the split between them is the rule that matters: the *list* is rebuilt
    /// and the selection cleared only when the input's **item** changes, not
    /// when its count does. Taking a slab spends one of the input, which
    /// changes the count and not the item, so the button stays pressed and the
    /// next slab appears — which is what makes a stonecutter usable at all.
    fn refresh_cut(&mut self, changed: &mut Changed) {
        let now = self.slots[CUT_INPUT].as_ref().map(|stack| stack.item);
        if now != self.cut_input {
            self.cut_input = now;
            self.cut_choice = None;
        }
        let made = self
            .cut_choice
            .and_then(|index| self.cut_list().get(index).copied())
            .map(|cut| {
                let (item, count) = cut.result();
                Stack::new(item, count)
            });
        if self.slots[CUT_OUTPUT] != made {
            self.slots[CUT_OUTPUT] = made;
            changed.mark(CUT_OUTPUT);
        }
    }

    /// Put in the smithing table's result slot whatever the three inputs make.
    ///
    /// **The result carries the base's components.** Vanilla's
    /// `SmithingTransformRecipe.assemble` calls `transmuteCopy`, which keeps
    /// the base stack's name, enchantments and damage and changes only which
    /// item it is. Building a fresh stack out of the recipe's own item would
    /// silently strip every enchantment a player spent levels on, and it would
    /// look right in every test that only compared item ids.
    fn refresh_smithed(&mut self, changed: &mut Changed) {
        let made = self.smithed();
        if self.slots[SMITH_OUTPUT] != made {
            self.slots[SMITH_OUTPUT] = made;
            changed.mark(SMITH_OUTPUT);
        }
    }

    fn smithed(&self) -> Option<Stack> {
        let smithing = self.smithing.as_ref()?;
        let base = self.slots[SMITH_BASE].as_ref()?;
        let found = smithing.find(
            self.slots[SMITH_TEMPLATE].as_ref()?.item,
            base.item,
            self.slots[SMITH_ADDITION].as_ref()?.item,
        )?;
        let (item, count) = found.result();
        Some(Stack::with_components(item, count, base.components.clone()))
    }

    /// Pay for one craft: one item out of every occupied grid slot, and
    /// whatever those items leave behind put back.
    ///
    /// Called only once the result has already been handed to the player, so
    /// there is no path on which the grid is spent and nothing comes back —
    /// which is the one failure crafting must not have. See decision record
    /// 0031.
    fn take_result(&mut self, window: Window, changed: &mut Changed) {
        // A bench has no grid and still has to be paid for. `StonecutterMenu`
        // removes one from the input and `SmithingMenu` one from each of the
        // three; neither leaves a remainder behind, so neither goes near the
        // bucket rule below.
        let spend: &[usize] = match window {
            Window::Stonecutter => &[CUT_INPUT],
            Window::Smithing => &[SMITH_TEMPLATE, SMITH_BASE, SMITH_ADDITION],
            _ => &[],
        };
        if !spend.is_empty() {
            for &index in spend {
                let Some(mut stack) = self.slots[index].clone() else {
                    continue;
                };
                stack.count -= 1;
                self.slots[index] = (stack.count > 0).then_some(stack);
                changed.mark(index);
            }
            self.refresh_output(window, changed);
            return;
        }
        let Some((_, grid, _)) = window.crafting() else {
            return;
        };
        for index in grid {
            let Some(mut stack) = self.slots[index].clone() else {
                continue;
            };
            let left = dust_sim::crafting::remainder(stack.item);
            stack.count -= 1;
            self.slots[index] = (stack.count > 0).then_some(stack.clone());
            changed.mark(index);
            // A bucket comes back. Minecraft puts it in the slot the milk
            // bucket came out of when that slot is now empty, merges it when
            // the slot holds the same thing, and otherwise gives it to the
            // player — and giving it to the player is the direction that
            // cannot lose it.
            let Some(left) = left.map(|item| Stack::new(item, 1)) else {
                continue;
            };
            match self.slots[index].clone() {
                None => self.slots[index] = Some(left),
                Some(mut there) if there.stacks_with(&left) && !there.is_full() => {
                    there.count += 1;
                    self.slots[index] = Some(there);
                }
                Some(_) => self.give(left, changed),
            }
        }
        self.refresh_output(window, changed);
    }

    /// What a player's window close does to what they were holding.
    ///
    /// Vanilla throws the cursor and the crafting grid on the floor. There is
    /// no floor to throw onto here, so both are put back into the inventory
    /// where they fit — which is better for the player than deleting them and
    /// is the only difference from vanilla in this file. What does not fit is
    /// lost, and there is nowhere else for it to go.
    pub fn closed(&mut self, window: Window) -> Changed {
        let mut changed = Changed::default();
        if let Some(stack) = self.cursor.take() {
            changed.mark_cursor();
            self.give(stack, &mut changed);
        }
        // A furnace's three slots are not the player's and are not given
        // back. They belong to the block, they go on smelting after the
        // screen shuts, and a close that emptied them into the player's
        // pockets would be a furnace that could never be left alone.
        //
        // A stonecutter's and a smithing table's *are* the player's, and both
        // are given back: `StonecutterMenu.removed` and
        // `ItemCombinerMenu.removed` both call `clearContainer`, so a player
        // who shuts the screen on a netherite ingot keeps it. Leaving them in
        // the block would be an item lost to a mis-click.
        let (output, grid) = match window {
            Window::Stonecutter => (CUT_OUTPUT, CUT_INPUT..CUT_OUTPUT),
            Window::Smithing => (SMITH_OUTPUT, SMITH_TEMPLATE..SMITH_OUTPUT),
            _ => match window.crafting() {
                Some((output, grid, _)) => (output, grid),
                None => return changed,
            },
        };
        if matches!(window, Window::Stonecutter) {
            self.cut_choice = None;
            self.cut_input = None;
        }
        for index in grid {
            if let Some(stack) = self.slots[index].take() {
                changed.mark(index);
                self.give(stack, &mut changed);
            }
        }
        // The output is what the grid makes, and the grid is now empty. It is
        // cleared rather than given to the player: it was never crafted, it
        // was only ever a picture of what *would* be, and handing it over
        // would be a free item for every player who opened their inventory,
        // put a log in and closed it again.
        if self.slots[output].take().is_some() {
            changed.mark(output);
        }
        changed
    }

    /// Take a stack up off the ground.
    ///
    /// The same placement [`Inventory::closed`] uses, and it is vanilla's
    /// `Inventory.add`: the slot in hand first, then the offhand, then the
    /// rest of the hotbar, then the main inventory — and an empty slot is
    /// taken from the hotbar before the inventory.
    ///
    /// Measured, and it was wrong before. This container filled the main
    /// inventory first, on the reasoning that a player does not want their
    /// hand replaced; a real 1.21.1 server closing a window with a log in the
    /// crafting grid puts that log in the **first hotbar slot**, and
    /// `tools/bot/crafting.js` reports the row. Priority 1: where a picked-up
    /// stack lands is forty hours of muscle memory and a server that puts it
    /// somewhere else is a server that feels wrong without being able to say
    /// why.
    ///
    /// Returns what did **not** fit. A full inventory is a real state and the
    /// item stays on the ground for it; deleting the overflow would be a
    /// player watching their pickaxe vanish because their pockets were full.
    pub fn collect(&mut self, stack: Stack) -> (Changed, Option<Stack>) {
        let mut changed = Changed::default();
        let mut left = stack;
        self.place(&mut left, &mut changed);
        (changed, (left.count > 0).then_some(left))
    }

    /// Put a stack where a player would be given one. Whatever does not fit is
    /// dropped, which is the only caller-visible loss and only happens with a
    /// full inventory.
    fn give(&mut self, stack: Stack, changed: &mut Changed) {
        let mut left = stack;
        self.place(&mut left, changed);
    }

    /// Vanilla's `Inventory.add`, which is two searches and not one range.
    ///
    /// `getSlotWithRemainingSpace` looks for a partial stack of the same thing
    /// in the slot in hand, then the offhand, then the hotbar, then the main
    /// inventory. `getFreeSlot` then looks for an empty one, and it scans only
    /// the thirty-six — **the offhand is never chosen for an empty slot**,
    /// which is why the two searches are written out rather than sharing one
    /// order.
    fn place(&mut self, stack: &mut Stack, changed: &mut Changed) {
        if stack.item.max_stack_size() > 1 {
            let selected = HOTBAR_START + self.selected;
            let order = [selected, OFFHAND]
                .into_iter()
                .chain(HOTBAR_START..HOTBAR_END)
                .chain(MAIN_START..MAIN_END);
            for index in order {
                if stack.count == 0 {
                    return;
                }
                let Some(mut there) = self.slots[index].clone() else {
                    continue;
                };
                let limit = slot_limit(index, stack.item);
                if !there.stacks_with(stack) || there.count >= limit {
                    continue;
                }
                let moved = stack.count.min(limit - there.count);
                there.count += moved;
                stack.count -= moved;
                self.slots[index] = Some(there);
                changed.mark(index);
            }
        }
        if stack.count == 0 {
            return;
        }
        for index in (HOTBAR_START..HOTBAR_END).chain(MAIN_START..MAIN_END) {
            if self.slots[index].is_some() {
                continue;
            }
            let moved = stack.count.min(slot_limit(index, stack.item));
            self.slots[index] = Some(stack.of(moved));
            stack.count -= moved;
            changed.mark(index);
            return;
        }
    }

    // -- the seven modes ---------------------------------------------------

    fn pickup(
        &mut self,
        window: Window,
        slot: i16,
        named: Option<usize>,
        button: i8,
        changed: &mut Changed,
    ) {
        if slot == OUTSIDE {
            // Clicked the world behind the window with something on the
            // cursor. Left drops it all, right drops one.
            match (self.cursor.clone(), button) {
                (Some(_), 0) => {
                    self.cursor = None;
                    changed.mark_cursor();
                }
                (Some(mut held), 1) => {
                    held.count -= 1;
                    self.cursor = (held.count > 0).then_some(held);
                    changed.mark_cursor();
                }
                _ => {}
            }
            return;
        }
        let Some(index) = named else {
            return;
        };
        if Some(index) == window.crafting_output() {
            self.pickup_result(window, button, changed);
            return;
        }
        if !writable(index) {
            return;
        }
        match button {
            0 => self.pickup_left(index, changed),
            1 => self.pickup_right(index, changed),
            _ => {}
        }
    }

    /// A click on the crafting output.
    ///
    /// Nothing can be put here, so the only two things a click can do is take
    /// the result onto an empty cursor or pour it onto a cursor already
    /// holding the same thing. Both take the **whole** result and pay for it
    /// once.
    ///
    /// Left and right do the same thing, and that is a decision rather than an
    /// omission. Vanilla's `AbstractContainerMenu.doClick` computes
    /// `(count + 1) / 2` for a right click before it reaches the result slot,
    /// and the ingredients are spent by `ResultSlot.onTake` whatever that
    /// number came out as — so a right click on four planks made from one log
    /// hands the player two planks and destroys the other two. This server
    /// hands over four. Priority 1: a click that silently deletes half a craft
    /// is the single worst thing an inventory can do to a player, and no
    /// player has ever right-clicked the output *wanting* half.
    fn pickup_result(&mut self, window: Window, button: i8, changed: &mut Changed) {
        if !(0..=1).contains(&button) {
            return;
        }
        let Some(made) = self.slots[window.output()].clone() else {
            return;
        };
        match self.cursor.clone() {
            None => self.cursor = Some(made),
            Some(mut held) => {
                if !held.stacks_with(&made) || held.count + made.count > held.item.max_stack_size()
                {
                    // No room for the whole result. Nothing happens rather
                    // than part of it happening: a craft that spent the grid
                    // and delivered less than it made would be the loss this
                    // whole path exists to avoid.
                    return;
                }
                held.count += made.count;
                self.cursor = Some(held);
            }
        }
        changed.mark_cursor();
        self.take_result(window, changed);
    }

    fn pickup_left(&mut self, index: usize, changed: &mut Changed) {
        let limit = self
            .cursor
            .as_ref()
            .map_or(u8::MAX, |held| slot_limit(index, held.item));
        match (self.cursor.clone(), self.slots[index].clone()) {
            // Hand empty, slot full: take it all.
            (None, Some(stack)) => {
                self.cursor = Some(stack);
                self.slots[index] = None;
            }
            // Hand full, slot empty: put down as much as the slot will hold.
            // A slot that will not take this item at all does nothing, which is
            // what a real server does with cobblestone aimed at a helmet slot.
            (Some(mut held), None) => {
                if !may_place(
                    index,
                    held.item,
                    self.fuel.as_deref(),
                    self.smithing.as_deref(),
                ) {
                    return;
                }
                let moved = held.count.min(limit);
                held.count -= moved;
                self.slots[index] = Some(held.of(moved));
                self.cursor = (held.count > 0).then_some(held);
            }
            // Both full, same item: pour the hand into the slot up to what the
            // slot will hold and keep the rest.
            (Some(mut held), Some(mut there))
                if held.stacks_with(&there)
                    && there.count < limit
                    && may_place(
                        index,
                        held.item,
                        self.fuel.as_deref(),
                        self.smithing.as_deref(),
                    ) =>
            {
                let moved = held.count.min(limit - there.count);
                there.count += moved;
                held.count -= moved;
                self.slots[index] = Some(there);
                self.cursor = (held.count > 0).then_some(held);
            }
            // Both full, different items — or the same item with no room.
            // Swap, if the slot will take what is on the cursor and the whole
            // of it fits.
            (Some(held), Some(there)) => {
                if !may_place(
                    index,
                    held.item,
                    self.fuel.as_deref(),
                    self.smithing.as_deref(),
                ) || held.count > limit
                {
                    return;
                }
                self.slots[index] = Some(held);
                self.cursor = Some(there);
            }
            (None, None) => return,
        }
        changed.mark(index);
        changed.mark_cursor();
    }

    fn pickup_right(&mut self, index: usize, changed: &mut Changed) {
        match (self.cursor.clone(), self.slots[index].clone()) {
            // Hand empty: take half, rounded up. Vanilla rounds the *taken*
            // half up, so a right-click on three leaves one behind.
            (None, Some(mut there)) => {
                let taken = there.count.div_ceil(2);
                self.cursor = Some(there.of(taken));
                there.count -= taken;
                self.slots[index] = (there.count > 0).then_some(there);
            }
            // Hand full, slot empty or the same item with room: put one down.
            (Some(mut held), None) => {
                if !may_place(
                    index,
                    held.item,
                    self.fuel.as_deref(),
                    self.smithing.as_deref(),
                ) {
                    return;
                }
                held.count -= 1;
                self.slots[index] = Some(held.of(1));
                self.cursor = (held.count > 0).then_some(held);
            }
            (Some(mut held), Some(mut there))
                if held.stacks_with(&there)
                    && there.count < slot_limit(index, held.item)
                    && may_place(
                        index,
                        held.item,
                        self.fuel.as_deref(),
                        self.smithing.as_deref(),
                    ) =>
            {
                held.count -= 1;
                there.count += 1;
                self.slots[index] = Some(there);
                self.cursor = (held.count > 0).then_some(held);
            }
            (Some(held), Some(there)) => {
                if !may_place(
                    index,
                    held.item,
                    self.fuel.as_deref(),
                    self.smithing.as_deref(),
                ) || held.count > slot_limit(index, held.item)
                {
                    return;
                }
                self.slots[index] = Some(held);
                self.cursor = Some(there);
            }
            (None, None) => return,
        }
        changed.mark(index);
        changed.mark_cursor();
    }

    /// Shift-click: send the stack where a real client sends it.
    ///
    /// Vanilla's `AbstractContainerMenu.clicked` does not call
    /// `quickMoveStack` once. It calls it **in a loop**, until a call moves
    /// nothing or the slot no longer holds the same item, and that loop is not
    /// a detail — it is the only reason shift-clicking a stack of nine player
    /// heads works. The first pass sees an empty head slot and moves one head
    /// there, because an armour slot holds one. The second pass sees the head
    /// slot occupied, takes a different arm entirely, and sends the other eight
    /// to the hotbar. A single pass leaves eight heads sitting in the slot the
    /// player shift-clicked, which is what this did until a real server was
    /// asked.
    fn quick_move(&mut self, window: Window, named: Option<usize>, changed: &mut Changed) {
        let Some(index) = named else {
            return;
        };
        if Some(index) == window.crafting_output() {
            self.quick_move_result(window, changed);
            return;
        }
        loop {
            let Some(mut stack) = self.slots[index].clone() else {
                return;
            };
            let destinations = self.quick_move_destination(window, index, stack.item);
            self.slots[index] = None;
            let before = stack.count;
            // Vanilla writes this as `if (!moveItemStackTo(a)) moveItemStackTo(b)`,
            // and the `!` is the rule: the second destination is tried only
            // when the first took *nothing*, not when it took some. A
            // shift-clicked stack that half fits into a crafting grid does not
            // spill its other half into the hotbar.
            for destination in destinations.into_iter().flatten() {
                self.move_into(destination.0, &mut stack, changed, destination.1);
                if stack.count != before {
                    break;
                }
            }
            if stack.count == before {
                // Nowhere for any of it to go. Vanilla leaves the slot alone
                // and so does this: a shift-click that moves nothing must not
                // report a change, or the client redraws a slot that did not
                // move.
                self.slots[index] = Some(stack);
                return;
            }
            changed.mark(index);
            if stack.count == 0 {
                return;
            }
            self.slots[index] = Some(stack);
        }
    }

    /// Shift-click on the crafting output: craft until the inputs run out.
    ///
    /// This is the same loop [`quick_move`] runs and the same reason it is a
    /// loop, but the destination is not what changes between passes — the
    /// *source* is. Each pass spends the grid, the grid makes the result
    /// again, and the pass after that finds a full output slot. A stack of
    /// sixty-four logs shift-clicked once is sixty-four crafts and 256 planks,
    /// which is the single most-used interaction in the game.
    ///
    /// It stops on the first pass whose result does not fit **whole**, and
    /// that pass is not performed. Vanilla's `moveItemStackTo` returns true
    /// when it moved *any* of the stack, spends the grid anyway, and the
    /// remainder is destroyed; a player shift-clicking with two free slots
    /// left watches ingredients turn into nothing. Priority 1: stopping one
    /// craft early leaves the ingredients in the grid where the player can see
    /// them.
    ///
    /// The bound is the grid, not a constant: every pass removes one item from
    /// every occupied grid slot, so a 2x2 holding four full stacks is at most
    /// sixty-four passes and there is no way to write a recipe that does not
    /// shrink its own inputs.
    ///
    /// [`quick_move`]: Inventory::quick_move
    fn quick_move_result(&mut self, window: Window, changed: &mut Changed) {
        loop {
            let Some(made) = self.slots[window.output()].clone() else {
                return;
            };
            // Tried against a copy first. `move_to` mutates what it is given
            // and leaves behind what did not fit, and there is no way to put
            // that back into the inventory it came from without knowing which
            // slots it touched — so the question "does all of it fit" is asked
            // before anything moves.
            // **The one place Dust does not do what Minecraft does, and it is
            // a choice — see `docs/decisions/0041-a-craft-that-only-half-fits.md`.**
            // Vanilla moves what fits, spends the grid and lets the remainder
            // fall off the stack frame: eight planks in, one stick out, one
            // stick destroyed. Dust refuses the pass instead. Nothing is
            // created and nothing is lost, and the state that caused it — a
            // full inventory — is on the screen the player is looking at,
            // which the silent loss is not.
            if !self.room_for(&made) {
                return;
            }
            let mut stack = made;
            // Reversed, which is `InventoryMenu.quickMoveStack`'s own
            // `moveItemStackTo(stack, 9, 45, true)` for slot 0 and only for
            // slot 0: a crafted stack fills the hotbar from the right before
            // it touches the main inventory, and a player who has crafted
            // planks expects them under their hand.
            self.move_to_reversed(MAIN_START..HOTBAR_END, &mut stack, changed);
            debug_assert_eq!(stack.count, 0, "room_for said the whole stack fits");
            self.take_result(window, changed);
        }
    }

    /// Whether `MAIN_START..HOTBAR_END` can take the whole of this stack.
    ///
    /// Counts rather than moves: partial stacks of the same item take what
    /// they have room for, and empty slots take a stack each.
    fn room_for(&self, stack: &Stack) -> bool {
        let mut left = u32::from(stack.count);
        for index in MAIN_START..HOTBAR_END {
            let limit = u32::from(slot_limit(index, stack.item));
            left = left.saturating_sub(match self.slots[index].as_ref() {
                None => limit,
                Some(there) if there.stacks_with(stack) => {
                    limit.saturating_sub(u32::from(there.count))
                }
                Some(_) => 0,
            });
            if left == 0 {
                return true;
            }
        }
        false
    }

    /// Where one pass of a shift-click sends what is in this slot.
    ///
    /// Vanilla's `InventoryMenu.quickMoveStack`, arm for arm and in its order,
    /// because the order *is* the rule: the equipment arms sit between the
    /// container's two halves, so a helmet in the main inventory goes to the
    /// head — but a helmet already **in** an armour slot comes off, and a
    /// helmet with a helmet already on the head goes to the hotbar like any
    /// other item.
    ///
    /// - the crafting output, the crafting grid and the armour empty into the
    ///   inventory as a whole,
    /// - an item that is worn, whose slot is empty, is put on,
    /// - the main inventory goes to the hotbar,
    /// - the hotbar goes to the main inventory,
    /// - anything else — the offhand — goes to the inventory as a whole.
    fn quick_move_destination(&self, window: Window, index: usize, item: Item) -> Destinations {
        match window {
            Window::Player => [Some((self.player_destination(index, item), false)), None],
            // `CraftingMenu.quickMoveStack`, arm for arm. The grid is tried
            // *first* for anything coming out of the player's half, which is
            // the arm a player feels: shift-clicking planks with a table open
            // lays them into the grid rather than shuffling them between the
            // hotbar and the inventory.
            Window::Table => {
                if (TABLE_GRID_START..TABLE_GRID_END).contains(&index) {
                    [Some((MAIN_START..HOTBAR_END, false)), None]
                } else if (MAIN_START..MAIN_END).contains(&index) {
                    [
                        Some((TABLE_GRID_START..TABLE_GRID_END, false)),
                        Some((HOTBAR_START..HOTBAR_END, false)),
                    ]
                } else {
                    [
                        Some((TABLE_GRID_START..TABLE_GRID_END, false)),
                        Some((MAIN_START..MAIN_END, false)),
                    ]
                }
            }
            // `AbstractFurnaceMenu.quickMoveStack`, arm for arm, and the
            // order is the one a player feels. Out of any of the three
            // furnace slots, into the player's half. Out of the player's
            // half: the input if the fire cooks it, else the fuel slot if it
            // burns, and only if it is neither does the stack shuffle between
            // the hotbar and the inventory.
            //
            // Which way round matters for one common item: a **log** both
            // smelts (to charcoal) and burns. Vanilla tries the input first,
            // so shift-clicking logs at a furnace fills the input, and a
            // player who wanted them as fuel drags them. Trying fuel first
            // would put every log a player owns under the fire.
            Window::Furnace => {
                if is_furnace_slot(index) {
                    return [Some((MAIN_START..HOTBAR_END, false)), None];
                }
                let item = self.slots[index].as_ref().map(|stack| stack.item);
                let smeltable = item.is_some_and(|item| self.cooks(item));
                let burns = item.is_some_and(|item| self.burns(item));
                if smeltable {
                    return [Some((FURNACE_INPUT..FURNACE_INPUT + 1, false)), None];
                }
                if burns {
                    return [Some((FURNACE_FUEL..FURNACE_FUEL + 1, false)), None];
                }
                if (MAIN_START..MAIN_END).contains(&index) {
                    [Some((HOTBAR_START..HOTBAR_END, false)), None]
                } else {
                    [Some((MAIN_START..MAIN_END, false)), None]
                }
            }
            // `StonecutterMenu.quickMoveStack`, arm for arm. Out of either of
            // the bench's two slots, into the player's half; out of the
            // player's half, into the input **if the stonecutter cuts it**,
            // and only otherwise between the hotbar and the inventory.
            Window::Stonecutter => {
                if (CUT_START..CUT_END).contains(&index) {
                    return [Some((MAIN_START..HOTBAR_END, false)), None];
                }
                if self.cut_list_for(item).is_empty() {
                    if (MAIN_START..MAIN_END).contains(&index) {
                        return [Some((HOTBAR_START..HOTBAR_END, false)), None];
                    }
                    return [Some((MAIN_START..MAIN_END, false)), None];
                }
                [Some((CUT_INPUT..CUT_OUTPUT, false)), None]
            }
            // `ItemCombinerMenu.quickMoveStack` with `SmithingMenu`'s own
            // `getSlotToQuickMoveTo`: the **first** of the three input slots
            // that will take this item, and the player's half if none will.
            Window::Smithing => {
                if (SMITH_START..SMITH_END).contains(&index) {
                    return [Some((MAIN_START..HOTBAR_END, false)), None];
                }
                let to = (SMITH_TEMPLATE..SMITH_OUTPUT).find(|&slot| {
                    may_place(slot, item, self.fuel.as_deref(), self.smithing.as_deref())
                });
                if let Some(to) = to {
                    return [Some((to..SMITH_OUTPUT, false)), None];
                }
                if (MAIN_START..MAIN_END).contains(&index) {
                    [Some((HOTBAR_START..HOTBAR_END, false)), None]
                } else {
                    [Some((MAIN_START..MAIN_END, false)), None]
                }
            }
        }
    }

    /// What a stonecutter would cut this item into, without it being in the
    /// slot yet. The question a shift-click asks.
    fn cut_list_for(&self, item: Item) -> &[dust_sim::cutting::Cut] {
        self.cutting
            .as_ref()
            .map_or(&[][..], |cutting| cutting.cuts_of(item))
    }

    /// Whether the open furnace's fire cooks this item.
    ///
    /// `false` on a server with no recipes, which is the same answer it gives
    /// for an item nothing cooks — and here the two really are the same
    /// answer, because a shift-click has to go *somewhere* and a server with
    /// no data has no reason to prefer the input.
    fn cooks(&self, item: Item) -> bool {
        self.cooking
            .as_ref()
            .zip(self.fire)
            .is_some_and(|(cooking, fire)| cooking.find(fire, item).is_some())
    }

    /// Whether this item burns. See [`may_place`] for what a table with no
    /// `burn` column means.
    fn burns(&self, item: Item) -> bool {
        self.fuel
            .as_ref()
            .is_some_and(|table| table.burn(item).is_some())
    }

    /// `InventoryMenu.quickMoveStack`, which is the arm the player's own
    /// window uses and the only one with an opinion about armour.
    fn player_destination(&self, index: usize, item: Item) -> std::ops::Range<usize> {
        if index < ARMOUR_END {
            return MAIN_START..HOTBAR_END;
        }
        if let Some(to) = worn_in(item).filter(|&to| self.slots[to].is_none()) {
            return to..to + 1;
        }
        if (MAIN_START..MAIN_END).contains(&index) {
            HOTBAR_START..HOTBAR_END
        } else if (HOTBAR_START..HOTBAR_END).contains(&index) {
            MAIN_START..MAIN_END
        } else {
            MAIN_START..HOTBAR_END
        }
    }

    /// A number key or F: swap this slot with a hotbar slot or the offhand.
    ///
    /// The named slot has an opinion and the hotbar slot does not, so only one
    /// direction is checked: pressing 1 over the helmet slot with cobblestone
    /// in hotbar slot 0 does nothing at all, and pressing it with a helmet
    /// there swaps. A real server does exactly that, and a server that swapped
    /// anyway is a player wearing a block.
    fn swap(&mut self, window: Window, named: Option<usize>, button: i8, changed: &mut Changed) {
        // The button numbers the *player's* hotbar however a table renumbers
        // everything else, because it is a key press and not a slot: pressing
        // 1 with a crafting table open still means the first hotbar slot.
        let other = if button == SWAP_OFFHAND_BUTTON {
            // Which a crafting table cannot reach at all: F over a table menu
            // swaps with the offhand on a real server because the offhand is
            // still the player's, and the menu's own numbering never names it.
            OFFHAND
        } else if (0..HOTBAR_SLOTS as i8).contains(&button) {
            HOTBAR_START + button as usize
        } else {
            return;
        };
        // A number key over the crafting output takes the result, and only
        // into a slot that is empty — there is nothing to swap it *with*,
        // because nothing may be put into the output. Vanilla's `doClick`
        // reaches the same place by asking `mayPlace` of the hotbar's stack
        // and finding it false.
        let Some(index) = named else {
            return;
        };
        if Some(index) == window.crafting_output() {
            let Some(made) = self.slots[index].clone() else {
                return;
            };
            if self.slots[other].is_some() {
                return;
            }
            self.slots[other] = Some(made);
            changed.mark(other);
            self.take_result(window, changed);
            return;
        }
        if !writable(index) {
            return;
        }
        if other == index {
            return;
        }
        let Some(mut coming) = self.slots[other].clone() else {
            // Nothing coming in: this is a take, and every slot here may be
            // taken from.
            if self.slots[index].is_some() {
                self.slots.swap(index, other);
                changed.mark(index);
                changed.mark(other);
            }
            return;
        };
        if !may_place(
            index,
            coming.item,
            self.fuel.as_deref(),
            self.smithing.as_deref(),
        ) {
            return;
        }
        let limit = slot_limit(index, coming.item);
        if coming.count <= limit {
            self.slots.swap(index, other);
            changed.mark(index);
            changed.mark(other);
            return;
        }
        // More than the slot holds — a stack of skulls aimed at the head. The
        // slot takes what it holds, the rest stays in the hotbar, and whatever
        // was in the slot goes back into the inventory rather than being
        // deleted to make room.
        let going = self.slots[index].take();
        coming.count -= limit;
        self.slots[index] = Some(coming.of(limit));
        self.slots[other] = Some(coming);
        changed.mark(index);
        changed.mark(other);
        if let Some(going) = going {
            self.give(going, changed);
        }
    }

    /// Creative middle-click: a full stack of whatever is there.
    ///
    /// Every player on this server is in creative, which is the condition
    /// vanilla gates this on. The count is the item's maximum and not 64 —
    /// middle-clicking a bucket gives one bucket.
    fn clone_slot(&mut self, named: Option<usize>, changed: &mut Changed) {
        let Some(index) = named.filter(|index| writable(*index)) else {
            return;
        };
        if self.cursor.is_some() {
            return;
        }
        let Some(there) = self.slots[index].as_ref() else {
            return;
        };
        // Vanilla's middle-click is `ItemStack.copy()`, which copies the
        // components too: middle-clicking a named pickaxe gives a named
        // pickaxe, not a plain one.
        self.cursor = Some(there.of(there.item.max_stack_size()));
        changed.mark_cursor();
    }

    /// Q and control-Q. The item is destroyed: see this module's header.
    fn throw(&mut self, window: Window, named: Option<usize>, button: i8, changed: &mut Changed) {
        if self.cursor.is_some() {
            // Vanilla ignores a throw while something is on the cursor — that
            // gesture is the outside-click drop instead.
            return;
        }
        // Q over the crafting output crafts once and throws what it made away.
        // Measured, not guessed: a real 1.21.1 server takes a log out of the
        // grid for both Q and control-Q, one craft each, and this container
        // matches it. What reaches the floor differs between the two buttons
        // on a real server and reaches no floor at all here, which is what
        // every other Q in this file already does.
        let Some(index) = named else {
            return;
        };
        if Some(index) == window.crafting_output() && (0..=1).contains(&button) {
            if self.slots[index].is_none() {
                return;
            }
            self.slots[index] = None;
            changed.mark(index);
            self.take_result(window, changed);
            return;
        }
        if !writable(index) {
            return;
        }
        let Some(mut there) = self.slots[index].clone() else {
            return;
        };
        match button {
            0 => {
                there.count -= 1;
                self.slots[index] = (there.count > 0).then_some(there);
            }
            1 => self.slots[index] = None,
            _ => return,
        }
        changed.mark(index);
    }

    /// The three-packet drag.
    ///
    /// `button` encodes both which drag and which step: `kind * 4 + step`,
    /// where step 0 starts, 1 adds a slot and 2 ends. Anything out of order
    /// resets, which is vanilla's rule and the one that keeps a dropped packet
    /// from turning into items nobody placed.
    fn quick_craft(
        &mut self,
        _window: Window,
        named: Option<usize>,
        button: i8,
        changed: &mut Changed,
    ) {
        let (kind, step) = (button / 4, button % 4);
        if !(0..=2).contains(&kind) || !(0..=2).contains(&step) {
            self.drag.reset();
            return;
        }
        let kind = kind as u8;
        match step {
            0 => {
                self.drag.reset();
                self.drag.active = true;
                self.drag.kind = kind;
            }
            1 => {
                if !self.drag.active || self.drag.kind != kind {
                    self.drag.reset();
                    return;
                }
                let Some(index) = named.filter(|index| writable(*index)) else {
                    return;
                };
                // A slot only joins the drag if the cursor's item could go
                // there: the slot will take that item, and it is empty or the
                // same item with room. Vanilla checks both at *this* step and
                // not at the end, which is load-bearing — the share each slot
                // gets is `count / slots.len()`, so a slot that is filtered on
                // the way in makes the others' share larger. Dragging twenty-one
                // cobblestone across the chest slot and one ordinary slot puts
                // all twenty-one in the ordinary slot on a real server, not ten.
                let Some(held) = self.cursor.as_ref() else {
                    self.drag.reset();
                    return;
                };
                let fits = may_place(
                    index,
                    held.item,
                    self.fuel.as_deref(),
                    self.smithing.as_deref(),
                ) && match self.slots[index].as_ref() {
                    None => true,
                    Some(there) => {
                        there.stacks_with(held) && there.count < slot_limit(index, held.item)
                    }
                };
                if fits {
                    self.drag.add(index);
                }
            }
            2 => {
                if !self.drag.active || self.drag.kind != kind {
                    self.drag.reset();
                    return;
                }
                self.finish_drag(kind, changed);
                self.drag.reset();
            }
            _ => unreachable!("step is 0..=2"),
        }
    }

    fn finish_drag(&mut self, kind: u8, changed: &mut Changed) {
        let Some(held) = self.cursor.clone() else {
            return;
        };
        if self.drag.count == 0 {
            return;
        }
        // Left drag splits what is on the cursor evenly and keeps the
        // remainder; right drag puts one in each; middle drag is creative and
        // fills each slot without spending anything.
        let share = match kind {
            0 => held.count / self.drag.count,
            1 => 1,
            _ => held.item.max_stack_size(),
        };
        if share == 0 {
            return;
        }
        let mut left = held.count;
        for index in 0..SLOTS {
            if self.drag.slots & (1u128 << index) == 0 {
                continue;
            }
            let existing = self.slots[index].as_ref().map_or(0, |s| s.count);
            let want = share.min(slot_limit(index, held.item).saturating_sub(existing));
            if want == 0 {
                continue;
            }
            // A creative middle drag spends nothing, so the cursor's count
            // never limits it.
            let take = if kind == 2 { want } else { want.min(left) };
            if take == 0 {
                break;
            }
            // Every slot in the drag joined it holding either nothing or a
            // stack this one merges with, so writing the cursor's components
            // over the slot's writes the same bytes it already had.
            self.slots[index] = Some(held.of(existing + take));
            changed.mark(index);
            if kind != 2 {
                left -= take;
            }
        }
        if kind != 2 && left != held.count {
            self.cursor = (left > 0).then_some(held.of(left));
            changed.mark_cursor();
        }
    }

    /// Double-click: gather every loose one of this item onto the cursor.
    ///
    /// Two passes, because vanilla makes two: partial stacks first, so that
    /// double-clicking with a half stack tidies the loose ones up instead of
    /// breaking a full stack somewhere else in the inventory.
    fn pickup_all(&mut self, window: Window, button: i8, changed: &mut Changed) {
        let Some(mut held) = self.cursor.clone() else {
            return;
        };
        if held.is_full() {
            return;
        }
        let max = held.item.max_stack_size();
        for pass in 0..2 {
            for step in 0..window.slot_count() {
                // Button 1 is the same gesture from the other end of the
                // container, which is what vanilla's `reverse` flag means —
                // and the end it starts from is the *window's*, so a
                // double-click with a table open gathers across the table's
                // numbering and never touches the armour.
                let slot = if button == 1 {
                    window.slot_count() - 1 - step
                } else {
                    step
                };
                let Some(index) = window.storage(slot) else {
                    continue;
                };
                if !writable(index) {
                    continue;
                }
                let Some(mut there) = self.slots[index].clone() else {
                    continue;
                };
                if !there.stacks_with(&held) {
                    continue;
                }
                if pass == 0 && there.is_full() {
                    continue;
                }
                let moved = there.count.min(max - held.count);
                if moved == 0 {
                    continue;
                }
                held.count += moved;
                there.count -= moved;
                self.slots[index] = (there.count > 0).then_some(there);
                changed.mark(index);
                if held.count >= max {
                    self.cursor = Some(held);
                    changed.mark_cursor();
                    return;
                }
            }
        }
        if !changed.is_empty() {
            self.cursor = Some(held);
            changed.mark_cursor();
        }
    }

    // -- shared moves ------------------------------------------------------

    /// The slot a click may write, if the number names one.
    ///
    /// The crafting output is not one: a click there in vanilla takes the
    /// result of a recipe, and there is no recipe here to have produced it.
    /// Vanilla's `AbstractContainerMenu.moveItemStackTo`, which is what every
    /// shift-click and every put-it-back is made of.
    ///
    /// Two passes and they are not the same pass. The first pours into partial
    /// stacks of the same item, so a shift-clicked stack tops up what is
    /// already there rather than opening a new slot beside it. The second puts
    /// what is left into **one** empty slot and stops — vanilla breaks out of
    /// that loop, and since no slot here holds more than a stack there is never
    /// anything left over to want a second one.
    ///
    /// Both passes ask [`slot_limit`] rather than the item's own maximum, and
    /// the second asks [`may_place`]. Neither matters for a range inside the
    /// inventory; both matter for the one-slot range an armour move uses.
    /// The same, from the far end. Vanilla's `reverse` flag, and slot 0's
    /// shift-click is the one caller that sets it — see [`quick_move_result`].
    ///
    /// [`quick_move_result`]: Inventory::quick_move_result
    fn move_to_reversed(
        &mut self,
        range: std::ops::Range<usize>,
        stack: &mut Stack,
        changed: &mut Changed,
    ) {
        self.move_into(range, stack, changed, true);
    }

    fn move_into(
        &mut self,
        range: std::ops::Range<usize>,
        stack: &mut Stack,
        changed: &mut Changed,
        reverse: bool,
    ) {
        // Arithmetic rather than a reversed iterator, because both halves want
        // the same order and collecting one would be an allocation on every
        // shift-click.
        let (start, end) = (range.start, range.end);
        let at = |step: usize| {
            if reverse {
                end - 1 - step
            } else {
                start + step
            }
        };
        let steps = end.saturating_sub(start);
        if stack.item.max_stack_size() > 1 {
            for index in (0..steps).map(at) {
                if stack.count == 0 {
                    return;
                }
                let Some(mut there) = self.slots[index].clone() else {
                    continue;
                };
                let limit = slot_limit(index, stack.item);
                if !there.stacks_with(stack) || there.count >= limit {
                    continue;
                }
                let moved = stack.count.min(limit - there.count);
                there.count += moved;
                stack.count -= moved;
                self.slots[index] = Some(there);
                changed.mark(index);
            }
        }
        if stack.count == 0 {
            return;
        }
        for index in (0..steps).map(at) {
            if self.slots[index].is_some()
                || !may_place(
                    index,
                    stack.item,
                    self.fuel.as_deref(),
                    self.smithing.as_deref(),
                )
            {
                continue;
            }
            let moved = stack.count.min(slot_limit(index, stack.item));
            self.slots[index] = Some(stack.of(moved));
            stack.count -= moved;
            changed.mark(index);
            return;
        }
    }
}

/// The seven things a click can be.
///
/// A copy of [`dust_protocol::packets::play::containers::ClickType`] rather
/// than a re-export, so that this module can be tested and reasoned about
/// without a packet, and so a mode this server does not implement is a
/// conversion that fails rather than a match arm nobody notices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickMode {
    Pickup,
    QuickMove,
    Swap,
    Clone,
    Throw,
    QuickCraft,
    PickupAll,
}

impl From<dust_protocol::packets::play::containers::ClickType> for ClickMode {
    fn from(kind: dust_protocol::packets::play::containers::ClickType) -> Self {
        use dust_protocol::packets::play::containers::ClickType as T;
        match kind {
            T::Pickup => Self::Pickup,
            T::QuickMove => Self::QuickMove,
            T::Swap => Self::Swap,
            T::Clone => Self::Clone,
            T::Throw => Self::Throw,
            T::QuickCraft => Self::QuickCraft,
            T::PickupAll => Self::PickupAll,
        }
    }
}

/// What a wire [`Slot`] turned out to be.
enum Decoded {
    Empty,
    Stack(Stack),
    /// A count above the item's own maximum. Refused rather than clamped: a
    /// client that asked for sixty-four buckets has to be told it did not get
    /// them, and clamping would leave it believing it did.
    TooMany,
    /// An id this build has no item for. It arrives from a client that may be
    /// modded or may be a version ahead, and dropping the connection over an
    /// item nobody can place would be a disconnect for a right-click.
    UnknownItem,
}

fn decode(slot: &Slot) -> Decoded {
    match slot {
        Slot::Empty => Decoded::Empty,
        Slot::Present { count, item_id, .. } => {
            let Some(item) = u32::try_from(*item_id)
                .ok()
                .and_then(Item::from_protocol_id)
            else {
                return Decoded::UnknownItem;
            };
            let Ok(count) = u8::try_from(*count) else {
                return Decoded::TooMany;
            };
            if count == 0 {
                return Decoded::Empty;
            }
            if count > item.max_stack_size() {
                return Decoded::TooMany;
            }
            let components = match components_of(slot) {
                Some(components) => components,
                None => return Decoded::Empty,
            };
            Decoded::Stack(Stack {
                item,
                count,
                components,
            })
        }
    }
}

/// This stack's components, as the wire gave them.
fn components_of(slot: &Slot) -> Option<ComponentPatch> {
    match slot {
        Slot::Empty => Some(ComponentPatch::EMPTY),
        Slot::Present { components, .. } => Some(components.clone()),
    }
}

/// Tell `dust-protocol` how to name a data-component type id.
///
/// The layouts of the fifty-seven component types are protocol knowledge and
/// live in `dust-protocol`; their *numbers* are Minecraft's, they are a
/// position in `minecraft:data_component_type`, and that registry is extracted
/// from the operator's own jar. Writing them down a second time in
/// `dust-protocol` would be a second answer to a question the registry already
/// answers — decision record 0016 declined the same trade for equipment slots —
/// so the lookup is installed here instead, where both halves are visible.
///
/// Idempotent, and cheap enough to call on every server construction: the
/// registry handle is resolved once and the lookup is a function pointer.
pub fn install_component_types() {
    fn name_of(id: i32) -> Option<&'static str> {
        static REGISTRY: OnceLock<Option<dust_registry::Registry>> = OnceLock::new();
        let registry = (*REGISTRY
            .get_or_init(|| dust_registry::Registry::from_name("minecraft:data_component_type")))?;
        registry.entry_name(u32::try_from(id).ok()?)
    }
    dust_protocol::components::install_type_names(name_of);
}

/// A stack as the wire wants it.
///
/// The components are the bytes that arrived, in their canonical order. A
/// `memcpy` and nothing else: they are not re-encoded per send, which matters
/// because this is called once per slot the client is wrong about, on every
/// click, for every player.
#[must_use]
pub fn to_wire(stack: Option<&Stack>) -> Slot {
    match stack {
        None => Slot::Empty,
        Some(stack) => Slot::Present {
            count: i32::from(stack.count),
            item_id: stack.item.protocol_id() as i32,
            components: stack.components.clone(),
        },
    }
}

/// A stack as a wire [`Slot`], for reading a client's opinion of one back.
#[must_use]
pub fn from_wire(slot: &Slot) -> Option<Stack> {
    match decode(slot) {
        Decoded::Stack(stack) => Some(stack),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A patch that sets one component, with the id the operator's own
    /// registry gave it. Nothing here writes a component number down.
    fn patch(component: &str, payload: &[u8]) -> ComponentPatch {
        install_component_types();
        let id = dust_registry::Registry::from_name("minecraft:data_component_type")
            .and_then(|registry| registry.entry_id(component))
            .expect("the extracted registry has that component type") as i32;
        let mut bytes = Vec::new();
        dust_protocol::varint::write_var_int(1, &mut bytes);
        dust_protocol::varint::write_var_int(0, &mut bytes);
        dust_protocol::varint::write_var_int(id, &mut bytes);
        bytes.extend_from_slice(payload);
        ComponentPatch::from_wire_bytes(&bytes).expect("a patch this build can walk")
    }

    /// `minecraft:damage`, which is one VarInt and is what a used tool carries.
    fn worn(amount: i32) -> ComponentPatch {
        let mut payload = Vec::new();
        dust_protocol::varint::write_var_int(amount, &mut payload);
        patch("minecraft:damage", &payload)
    }

    /// `minecraft:custom_name`, which is one network-NBT value. An empty
    /// compound stands in for the text: this is about identity, not rendering.
    fn named() -> ComponentPatch {
        patch("minecraft:custom_name", &[10, 0])
    }

    fn item(name: &str) -> Item {
        Item::from_name(name).expect("this build has that item")
    }

    fn stone() -> Item {
        item("minecraft:stone")
    }

    fn dirt() -> Item {
        item("minecraft:dirt")
    }

    /// Stack size 1, which is what makes it the interesting case everywhere a
    /// count is arithmetic.
    ///
    /// A *water* bucket and not an empty one: an empty bucket stacks to 16 on
    /// 1.21.1, which is exactly the sort of thing a hand-written table gets
    /// wrong and the generated one does not.
    fn bucket() -> Item {
        item("minecraft:water_bucket")
    }

    fn helmet() -> Item {
        item("minecraft:iron_helmet")
    }

    fn boots() -> Item {
        item("minecraft:iron_boots")
    }

    /// Worn on the head and stacks to 64, which is the only combination in the
    /// game where an armour slot's own limit of one is visible.
    fn head() -> Item {
        item("minecraft:player_head")
    }

    fn wire(item: Item, count: i32) -> Slot {
        Slot::Present {
            count,
            item_id: item.protocol_id() as i32,
            components: ComponentPatch::EMPTY,
        }
    }

    /// Two recipes, written here rather than read: a shapeless one log into
    /// four planks, and a shaped 2x2 of planks into a crafting table. Neither
    /// is Minecraft's file — they are the two shapes this container has to
    /// handle, spelled in the language the operator's files are written in.
    fn recipes() -> std::sync::Arc<Recipes> {
        let mut recipes = Recipes::default();
        let planks = serde_json::json!({
            "type": "minecraft:crafting_shapeless",
            "ingredients": [{"item": "minecraft:oak_log"}],
            "result": {"id": "minecraft:oak_planks", "count": 4}
        });
        recipes
            .add("test:oak_planks", &planks, &Default::default())
            .expect("compiles");
        let table = serde_json::json!({
            "type": "minecraft:crafting_shaped",
            "pattern": ["##", "##"],
            "key": {"#": {"item": "minecraft:oak_planks"}},
            "result": {"id": "minecraft:crafting_table", "count": 1}
        });
        recipes
            .add("test:crafting_table", &table, &Default::default())
            .expect("compiles");
        // Three wide, so it cannot be made in the 2x2 a player carries. This
        // is the recipe the crafting table exists for.
        let chest = serde_json::json!({
            "type": "minecraft:crafting_shaped",
            "pattern": ["###", "# #", "###"],
            "key": {"#": {"item": "minecraft:oak_planks"}},
            "result": {"id": "minecraft:chest", "count": 1}
        });
        recipes
            .add("test:chest", &chest, &Default::default())
            .expect("compiles");
        recipes.index();
        std::sync::Arc::new(recipes)
    }

    fn crafting(pairs: &[(usize, Item, u8)]) -> Inventory {
        with(pairs).crafting_with(recipes())
    }

    fn log() -> Item {
        item("minecraft:oak_log")
    }

    fn planks() -> Item {
        item("minecraft:oak_planks")
    }

    fn with(pairs: &[(usize, Item, u8)]) -> Inventory {
        let mut inventory = Inventory::default();
        for &(index, item, count) in pairs {
            inventory.slots[index] = Some(Stack::new(item, count));
        }
        inventory
    }

    #[test]
    fn the_stack_sizes_are_minecrafts_and_they_differ() {
        // The whole reason nothing here writes 64. If these were equal this
        // module could hardcode one number and every test below would still
        // pass, which is exactly the trap.
        assert_eq!(stone().max_stack_size(), 64);
        assert_eq!(item("minecraft:ender_pearl").max_stack_size(), 16);
        assert_eq!(item("minecraft:bucket").max_stack_size(), 16);
        assert_eq!(bucket().max_stack_size(), 1);
    }

    #[test]
    fn a_fresh_inventory_holds_nothing() {
        let inventory = Inventory::default();
        assert_eq!(inventory.held(), None);
        assert_eq!(inventory.cursor().cloned(), None);
        assert!(inventory.slots().iter().all(Option::is_none));
    }

    #[test]
    fn a_creative_write_lands_in_the_slot_it_names() {
        // 36 is hotbar slot 0 and 44 is slot 8; 5 is the helmet and 45 the
        // offhand. All four are slots the old hotbar dropped on the floor.
        let mut inventory = Inventory::default();
        assert!(inventory
            .set_creative(36, &wire(stone(), 1))
            .unwrap()
            .has(36));
        assert_eq!(inventory.held(), Some(stone()), "slot 0 is selected");
        assert!(inventory
            .set_creative(44, &wire(dirt(), 5))
            .unwrap()
            .has(44));
        assert!(inventory.set_creative(9, &wire(dirt(), 64)).unwrap().has(9));
        assert!(inventory
            .set_creative(45, &wire(bucket(), 1))
            .unwrap()
            .has(45));
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(64));
        assert_eq!(inventory.slot(45).map(|s| s.item), Some(bucket()));
        assert!(inventory.select(8));
        assert_eq!(inventory.held(), Some(dirt()));
    }

    #[test]
    fn a_count_above_the_items_own_maximum_is_refused_and_the_slot_is_untouched() {
        // The check that would pass with a hardcoded 64 and does not: a bucket
        // stacks to one, so two is already too many.
        let mut inventory = Inventory::default();
        assert_eq!(inventory.set_creative(36, &wire(bucket(), 2)), Err(36));
        assert_eq!(inventory.slot(36).cloned(), None);
        assert_eq!(inventory.set_creative(37, &wire(stone(), 65)), Err(37));
        assert_eq!(inventory.slot(37).cloned(), None);
        // And the ones that are fine stay fine.
        assert!(inventory
            .set_creative(38, &wire(stone(), 64))
            .unwrap()
            .has(38));
        assert!(inventory
            .set_creative(39, &wire(item("minecraft:ender_pearl"), 16))
            .unwrap()
            .has(39));
        assert_eq!(
            inventory.set_creative(40, &wire(item("minecraft:ender_pearl"), 17)),
            Err(40)
        );
    }

    /// The output follows the grid, and it follows it back to empty.
    #[test]
    fn the_output_fills_when_the_grid_makes_something() {
        let mut inventory = crafting(&[(MAIN_START, log(), 1)]);
        assert_eq!(inventory.slot(CRAFTING_OUTPUT), None);
        // Pick the log up and put it in the grid.
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        let changed = inventory.click(Window::Player, ClickMode::Pickup, CRAFTING_START as i16, 0);
        assert!(
            changed.has(CRAFTING_OUTPUT),
            "the output moved with the grid"
        );
        assert_eq!(
            inventory.slot(CRAFTING_OUTPUT).cloned(),
            Some(Stack::new(planks(), 4))
        );
        // Take it back out again and the output empties.
        let changed = inventory.click(Window::Player, ClickMode::Pickup, CRAFTING_START as i16, 0);
        assert!(changed.has(CRAFTING_OUTPUT));
        assert_eq!(inventory.slot(CRAFTING_OUTPUT), None);
    }

    /// A container with no recipe table has no opinion, which is the server
    /// this was before crafting and the server an operator with no `[data]
    /// path` still has.
    #[test]
    fn without_recipes_the_output_never_fills() {
        let mut inventory = with(&[(CRAFTING_START, log(), 1)]);
        inventory.click(Window::Player, ClickMode::Pickup, CRAFTING_START as i16, 1);
        assert_eq!(inventory.slot(CRAFTING_OUTPUT), None);
    }

    /// Taking the result spends the grid exactly once, and both buttons take
    /// the whole of it.
    #[test]
    fn taking_the_result_spends_one_of_each_ingredient() {
        for button in [0, 1] {
            let mut inventory = crafting(&[(CRAFTING_START, log(), 3)]);
            // A click that moves nothing still has to leave the output right,
            // so the grid is refreshed by the constructor.
            assert_eq!(
                inventory.slot(CRAFTING_OUTPUT).cloned(),
                Some(Stack::new(planks(), 4))
            );
            inventory.click(
                Window::Player,
                ClickMode::Pickup,
                CRAFTING_OUTPUT as i16,
                button,
            );
            assert_eq!(inventory.cursor().cloned(), Some(Stack::new(planks(), 4)));
            assert_eq!(
                inventory.slot(CRAFTING_START).cloned(),
                Some(Stack::new(log(), 2)),
                "one log, not three and not none"
            );
            assert_eq!(
                inventory.slot(CRAFTING_OUTPUT).cloned(),
                Some(Stack::new(planks(), 4)),
                "two logs left, so there is still something to take"
            );
        }
    }

    /// Nothing may be put into the output, by any gesture.
    #[test]
    fn the_output_takes_nothing_a_player_puts_there() {
        let mut inventory = crafting(&[(MAIN_START, planks(), 4)]);
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        let changed = inventory.click(Window::Player, ClickMode::Pickup, CRAFTING_OUTPUT as i16, 0);
        assert!(changed.is_empty());
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(planks(), 4)));
        assert_eq!(inventory.slot(CRAFTING_OUTPUT), None);
    }

    /// A cursor already holding the result takes the next craft onto it; one
    /// holding something else, or holding too many, is left alone rather than
    /// having the grid spent underneath it.
    #[test]
    fn the_result_pours_onto_a_cursor_holding_the_same_thing() {
        let mut inventory = crafting(&[(CRAFTING_START, log(), 2), (MAIN_START, planks(), 60)]);
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        inventory.click(Window::Player, ClickMode::Pickup, CRAFTING_OUTPUT as i16, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(64));
        assert_eq!(inventory.slot(CRAFTING_START).map(|s| s.count), Some(1));
        // 64 on the cursor and four more coming: no room for the whole result,
        // so nothing happens and the log is still there.
        let changed = inventory.click(Window::Player, ClickMode::Pickup, CRAFTING_OUTPUT as i16, 0);
        assert!(changed.is_empty());
        assert_eq!(inventory.slot(CRAFTING_START).map(|s| s.count), Some(1));
    }

    /// Shift-clicking the output crafts as many times as the inputs allow.
    /// One pass is the defect this is here to catch.
    #[test]
    fn shift_clicking_the_output_crafts_until_the_grid_runs_out() {
        let mut inventory = crafting(&[(CRAFTING_START, log(), 16)]);
        inventory.click(
            Window::Player,
            ClickMode::QuickMove,
            CRAFTING_OUTPUT as i16,
            0,
        );
        assert_eq!(inventory.slot(CRAFTING_START), None, "every log spent");
        assert_eq!(inventory.slot(CRAFTING_OUTPUT), None);
        let planks: u32 = (MAIN_START..HOTBAR_END)
            .filter_map(|slot| inventory.slot(slot))
            .filter(|stack| stack.item == planks())
            .map(|stack| u32::from(stack.count))
            .sum();
        assert_eq!(planks, 64, "sixteen logs are sixteen crafts of four");
    }

    /// A crafted stack lands in the hotbar before the main inventory, which is
    /// `InventoryMenu.quickMoveStack`'s `moveItemStackTo(stack, 9, 45, true)`
    /// and the only slot in the container that reverses.
    #[test]
    fn a_shift_crafted_stack_fills_the_hotbar_from_the_right() {
        let mut inventory = crafting(&[(CRAFTING_START, log(), 1)]);
        inventory.click(
            Window::Player,
            ClickMode::QuickMove,
            CRAFTING_OUTPUT as i16,
            0,
        );
        assert_eq!(
            inventory.slot(HOTBAR_END - 1).cloned(),
            Some(Stack::new(planks(), 4))
        );
        assert!(inventory.slot(MAIN_START).is_none());
    }

    /// The last craft that does not fit whole is not performed. Vanilla moves
    /// what it can and spends the grid anyway; this leaves the ingredients
    /// where the player can see them.
    #[test]
    fn a_craft_that_does_not_fit_is_not_made() {
        // Every slot but one full of something else, and that one slot holding
        // 61 planks: the first craft fits (61 + 4 > 64 is false at 61 + 3, so
        // fill to exactly 64) and the second does not.
        let mut inventory = crafting(&[(CRAFTING_START, log(), 4)]);
        for slot in MAIN_START..HOTBAR_END {
            inventory.slots[slot] = Some(Stack::new(stone(), 64));
        }
        inventory.slots[HOTBAR_END - 1] = Some(Stack::new(planks(), 60));
        inventory.click(
            Window::Player,
            ClickMode::QuickMove,
            CRAFTING_OUTPUT as i16,
            0,
        );
        assert_eq!(
            inventory.slot(HOTBAR_END - 1).map(|s| s.count),
            Some(64),
            "one craft fitted"
        );
        assert_eq!(
            inventory.slot(CRAFTING_START).map(|s| s.count),
            Some(3),
            "three logs still in the grid, not spent for nothing"
        );
    }

    /// A number key over the output takes the result into an empty hotbar
    /// slot, and does nothing at all when that slot is occupied — there is
    /// nothing to swap with, because nothing may be put into the output.
    #[test]
    fn a_number_key_over_the_output_takes_the_result() {
        let mut inventory = crafting(&[(CRAFTING_START, log(), 2)]);
        inventory.click(Window::Player, ClickMode::Swap, CRAFTING_OUTPUT as i16, 0);
        assert_eq!(
            inventory.slot(HOTBAR_START).cloned(),
            Some(Stack::new(planks(), 4))
        );
        assert_eq!(inventory.slot(CRAFTING_START).map(|s| s.count), Some(1));
        // Slot 0 of the hotbar is full now, so the second press does nothing.
        let changed = inventory.click(Window::Player, ClickMode::Swap, CRAFTING_OUTPUT as i16, 0);
        assert!(changed.is_empty());
        assert_eq!(inventory.slot(CRAFTING_START).map(|s| s.count), Some(1));
    }

    /// Closing the window clears the output rather than handing it over. It
    /// was a picture of what the grid would make, never a crafted item.
    #[test]
    fn closing_the_window_clears_the_output_it_never_made() {
        let mut inventory = crafting(&[(CRAFTING_START, log(), 1)]);
        assert!(inventory.slot(CRAFTING_OUTPUT).is_some());
        inventory.closed(Window::Player);
        assert_eq!(inventory.slot(CRAFTING_OUTPUT), None);
        assert_eq!(
            inventory.slot(HOTBAR_START).cloned(),
            Some(Stack::new(log(), 1)),
            "the log went back into the hand, which is where a real server puts it"
        );
    }

    /// A creative write into the grid moves the output too, and the caller is
    /// told which slot it did not write.
    #[test]
    fn a_creative_write_into_the_grid_fills_the_output() {
        let mut inventory = Inventory::default().crafting_with(recipes());
        let changed = inventory
            .set_creative(CRAFTING_START as i16, &wire(log(), 1))
            .expect("accepted");
        assert!(changed.has(CRAFTING_START));
        assert!(changed.has(CRAFTING_OUTPUT));
        assert_eq!(
            inventory.slot(CRAFTING_OUTPUT).cloned(),
            Some(Stack::new(planks(), 4))
        );
    }

    /// The two windows number the same container differently, and every number
    /// either has resolves back to itself.
    #[test]
    fn a_window_and_its_numbering_round_trip() {
        for window in [Window::Player, Window::Table] {
            for slot in 0..window.slot_count() {
                let index = window.storage(slot).expect("every slot resolves");
                assert_eq!(window.wire(index), Some(slot), "{window:?} slot {slot}");
            }
        }
        // A table's numbering reaches ten slots the player's own cannot see,
        // and cannot see six the player's own has.
        assert_eq!(Window::Table.storage(0), Some(TABLE_OUTPUT));
        assert_eq!(Window::Table.storage(1), Some(TABLE_GRID_START));
        assert_eq!(Window::Table.storage(10), Some(MAIN_START));
        assert_eq!(Window::Table.storage(37), Some(HOTBAR_START));
        assert_eq!(Window::Table.wire(ARMOUR_HEAD), None);
        assert_eq!(Window::Table.wire(OFFHAND), None);
        assert_eq!(Window::Table.wire(CRAFTING_OUTPUT), None);
    }

    /// A three-wide recipe is made in the table and nowhere else.
    #[test]
    fn the_table_makes_what_the_players_own_grid_cannot() {
        let mut inventory = Inventory::default().crafting_with(recipes());
        // Eight planks around an empty middle, in the table's own numbering.
        for slot in [1, 2, 3, 4, 6, 7, 8, 9] {
            let index = Window::Table.storage(slot).expect("a grid slot");
            inventory.slots[index] = Some(Stack::new(planks(), 1));
        }
        let mut changed = Changed::default();
        inventory.refresh_output(Window::Table, &mut changed);
        assert_eq!(
            inventory.slot(TABLE_OUTPUT).map(|stack| stack.item),
            Item::from_name("minecraft:chest")
        );
        // And the player's own 2x2 makes nothing of it.
        assert_eq!(inventory.slot(CRAFTING_OUTPUT), None);
    }

    /// Taking the table's result spends the table's grid, not the player's.
    #[test]
    fn the_table_pays_out_of_its_own_grid() {
        let mut inventory = Inventory::default().crafting_with(recipes());
        inventory.slots[CRAFTING_START] = Some(Stack::new(log(), 4));
        for slot in [1, 2, 3, 4, 6, 7, 8, 9] {
            let index = Window::Table.storage(slot).expect("a grid slot");
            inventory.slots[index] = Some(Stack::new(planks(), 2));
        }
        let mut changed = Changed::default();
        inventory.refresh_output(Window::Player, &mut changed);
        inventory.refresh_output(Window::Table, &mut changed);
        // Slot 0 of the table's numbering is the chest.
        inventory.click(Window::Table, ClickMode::Pickup, 0, 0);
        assert_eq!(
            inventory.cursor().map(|stack| stack.item),
            Item::from_name("minecraft:chest")
        );
        assert_eq!(
            inventory.slot(TABLE_GRID_START).map(|s| s.count),
            Some(1),
            "one plank out of each table slot"
        );
        assert_eq!(
            inventory.slot(CRAFTING_START).map(|s| s.count),
            Some(4),
            "the player's own grid is untouched"
        );
        assert_eq!(
            inventory.slot(CRAFTING_OUTPUT).cloned(),
            Some(Stack::new(planks(), 4)),
            "and its output still says what it makes"
        );
    }

    /// Shift-clicking with a table open lays the stack into the grid first,
    /// which is `CraftingMenu.quickMoveStack`'s own first arm and the one a
    /// player uses constantly.
    #[test]
    fn shift_click_into_an_open_table_fills_its_grid() {
        let mut inventory = Inventory::default().crafting_with(recipes());
        inventory.slots[MAIN_START] = Some(Stack::new(planks(), 3));
        // Menu slot 10 is the first slot of the player's main inventory.
        inventory.click(Window::Table, ClickMode::QuickMove, 10, 0);
        assert_eq!(inventory.slot(MAIN_START), None);
        // The whole stack into one grid slot, not one plank per slot: a
        // crafting grid slot holds a full stack and `moveItemStackTo` fills
        // the first empty one it finds and stops. That is what a real server
        // does with a shift-clicked stack of planks and it is why laying out a
        // recipe is still a per-slot job.
        assert_eq!(
            inventory.slot(TABLE_GRID_START).cloned(),
            Some(Stack::new(planks(), 3))
        );
        assert_eq!(
            (TABLE_GRID_START..TABLE_GRID_END)
                .filter(|slot| inventory.slot(*slot).is_some())
                .count(),
            1
        );
    }

    /// Closing the table hands the grid back and clears its output; the
    /// player's own 2x2 is left where it was, because closing a table is not
    /// closing their inventory.
    #[test]
    fn closing_the_table_gives_the_grid_back() {
        let mut inventory = Inventory::default().crafting_with(recipes());
        inventory.slots[CRAFTING_START] = Some(Stack::new(log(), 1));
        inventory.slots[TABLE_GRID_START] = Some(Stack::new(planks(), 5));
        inventory.closed(Window::Table);
        assert_eq!(inventory.slot(TABLE_GRID_START), None);
        assert_eq!(
            inventory.slot(HOTBAR_START).cloned(),
            Some(Stack::new(planks(), 5))
        );
        assert_eq!(
            inventory.slot(CRAFTING_START).cloned(),
            Some(Stack::new(log(), 1)),
            "the player's own grid belongs to the other window"
        );
    }

    /// A player dropped mid-craft owns what is in the table's grid. The saved
    /// form folds it in, and folding it does not move it.
    #[test]
    fn what_is_saved_includes_an_open_tables_grid() {
        let mut inventory = Inventory::default().crafting_with(recipes());
        inventory.slots[TABLE_GRID_START] = Some(Stack::new(planks(), 7));
        let saved = inventory.saved();
        assert_eq!(saved[HOTBAR_START].clone(), Some(Stack::new(planks(), 7)));
        assert_eq!(
            inventory.slot(TABLE_GRID_START).cloned(),
            Some(Stack::new(planks(), 7)),
            "the fold is a projection and never a move"
        );
        // And the ten a table adds are not among the forty-six either way.
        assert_eq!(saved.len(), SLOTS);
    }

    /// A double-click with a table open gathers across the table's numbering
    /// and never reaches the armour.
    #[test]
    fn a_double_click_in_a_table_cannot_reach_the_armour() {
        let mut inventory = Inventory::default().crafting_with(recipes());
        inventory.slots[ARMOUR_HEAD] = Some(Stack::new(item("minecraft:player_head"), 1));
        inventory.slots[MAIN_START] = Some(Stack::new(item("minecraft:player_head"), 3));
        inventory.slots[MAIN_START + 1] = Some(Stack::new(item("minecraft:player_head"), 3));
        inventory.click(Window::Table, ClickMode::Pickup, 10, 0);
        inventory.click(Window::Table, ClickMode::PickupAll, 10, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(6));
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).map(|s| s.count),
            Some(1),
            "the head on the player's head is behind the screen"
        );
    }

    #[test]
    fn the_crafting_output_is_not_writable_and_neither_is_a_slot_off_the_end() {
        let mut inventory = Inventory::default();
        assert!(inventory
            .set_creative(0, &wire(stone(), 1))
            .unwrap()
            .is_empty());
        assert!(inventory
            .set_creative(46, &wire(stone(), 1))
            .unwrap()
            .is_empty());
        assert!(inventory
            .set_creative(-2, &wire(stone(), 1))
            .unwrap()
            .is_empty());
        // -1 is the creative menu's "throw this away", which is a real
        // instruction and not a refusal.
        assert!(inventory
            .set_creative(-1, &wire(stone(), 1))
            .unwrap()
            .is_empty());
        assert!(inventory.slots().iter().all(Option::is_none));
    }

    #[test]
    fn an_item_this_build_has_never_heard_of_is_refused_rather_than_dropping_the_player() {
        let mut inventory = Inventory::default();
        assert_eq!(
            inventory.set_creative(
                36,
                &Slot::Present {
                    count: 1,
                    item_id: 999_999,
                    components: ComponentPatch::EMPTY,
                }
            ),
            Err(36)
        );
        assert_eq!(inventory.held(), None);
    }

    #[test]
    fn a_selection_outside_the_hotbar_leaves_the_one_in_hand_alone() {
        let mut inventory = Inventory::default();
        assert!(inventory
            .set_creative(36, &wire(stone(), 1))
            .unwrap()
            .has(36));
        assert!(!inventory.select(9));
        assert!(!inventory.select(-1));
        assert_eq!(inventory.held(), Some(stone()));
    }

    #[test]
    fn left_click_takes_a_stack_puts_it_down_merges_and_swaps() {
        let mut inventory = with(&[(9, stone(), 30), (10, stone(), 50), (11, dirt(), 1)]);

        // Take.
        let changed = inventory.click(Window::Player, ClickMode::Pickup, 9, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(30));
        assert_eq!(inventory.slot(9).cloned(), None);
        assert!(changed.has(9) && changed.cursor());

        // Merge: 30 into a stack of 50 leaves 16 in hand and 64 in the slot.
        inventory.click(Window::Player, ClickMode::Pickup, 10, 0);
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(64));
        assert_eq!(inventory.cursor().map(|s| s.count), Some(16));

        // Swap: a different item.
        inventory.click(Window::Player, ClickMode::Pickup, 11, 0);
        assert_eq!(inventory.slot(11).cloned(), Some(Stack::new(stone(), 16)));
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(dirt(), 1)));

        // Put down.
        inventory.click(Window::Player, ClickMode::Pickup, 12, 0);
        assert_eq!(inventory.slot(12).cloned(), Some(Stack::new(dirt(), 1)));
        assert_eq!(inventory.cursor().cloned(), None);
    }

    #[test]
    fn right_click_takes_half_rounded_up_and_puts_one_down() {
        let mut inventory = with(&[(9, stone(), 3)]);
        inventory.click(Window::Player, ClickMode::Pickup, 9, 1);
        assert_eq!(
            inventory.cursor().map(|s| s.count),
            Some(2),
            "half of 3, up"
        );
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(1));

        inventory.click(Window::Player, ClickMode::Pickup, 10, 1);
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(1));
        assert_eq!(inventory.cursor().map(|s| s.count), Some(1));

        inventory.click(Window::Player, ClickMode::Pickup, 10, 1);
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(2));
        assert_eq!(inventory.cursor().cloned(), None);
    }

    #[test]
    fn right_click_on_a_single_leaves_the_slot_empty_rather_than_a_stack_of_nothing() {
        let mut inventory = with(&[(9, bucket(), 1)]);
        inventory.click(Window::Player, ClickMode::Pickup, 9, 1);
        assert_eq!(inventory.slot(9).cloned(), None);
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(bucket(), 1)));
    }

    #[test]
    fn shift_click_sends_the_hotbar_to_the_inventory_and_back() {
        let mut inventory = with(&[(36, stone(), 20)]);
        inventory.click(Window::Player, ClickMode::QuickMove, 36, 0);
        assert_eq!(inventory.slot(36).cloned(), None);
        assert_eq!(inventory.slot(9).cloned(), Some(Stack::new(stone(), 20)));

        inventory.click(Window::Player, ClickMode::QuickMove, 9, 0);
        assert_eq!(inventory.slot(9).cloned(), None);
        assert_eq!(inventory.slot(36).cloned(), Some(Stack::new(stone(), 20)));
    }

    #[test]
    fn shift_click_merges_before_it_takes_an_empty_slot() {
        // 40 in the hotbar and 34 already sitting in slot 9. The merge fills
        // slot 9 to 64 and the ten that do not fit take the next empty slot —
        // vanilla's `moveItemStackTo`, which is a merge pass followed by a
        // fill pass and not one or the other. A server that only filled would
        // put 40 in slot 10 and leave 34 loose.
        let mut inventory = with(&[(36, stone(), 40), (9, stone(), 34)]);
        inventory.click(Window::Player, ClickMode::QuickMove, 36, 0);
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(64));
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(10));
        assert_eq!(inventory.slot(36).cloned(), None);
    }

    #[test]
    fn shift_click_with_nowhere_to_go_changes_nothing() {
        // Every main slot full of a different item, so a hotbar stack has no
        // home. A server that reported a change here would make the client
        // redraw a slot that did not move.
        let mut inventory = Inventory::default();
        for index in MAIN_START..MAIN_END {
            inventory.slots[index] = Some(Stack::new(dirt(), 64));
        }
        inventory.slots[36] = Some(Stack::new(stone(), 5));
        let changed = inventory.click(Window::Player, ClickMode::QuickMove, 36, 0);
        assert!(changed.is_empty());
        assert_eq!(inventory.slot(36).cloned(), Some(Stack::new(stone(), 5)));
    }

    #[test]
    fn a_number_key_swaps_with_that_hotbar_slot_and_f_swaps_with_the_offhand() {
        let mut inventory = with(&[(9, stone(), 4), (38, dirt(), 2)]);
        inventory.click(Window::Player, ClickMode::Swap, 9, 2);
        assert_eq!(inventory.slot(9).cloned(), Some(Stack::new(dirt(), 2)));
        assert_eq!(inventory.slot(38).cloned(), Some(Stack::new(stone(), 4)));

        inventory.click(Window::Player, ClickMode::Swap, 9, SWAP_OFFHAND_BUTTON);
        assert_eq!(inventory.slot(9).cloned(), None);
        assert_eq!(
            inventory.slot(OFFHAND).cloned(),
            Some(Stack::new(dirt(), 2))
        );
    }

    #[test]
    fn middle_click_clones_a_full_stack_of_that_items_own_maximum() {
        let mut inventory = with(&[(9, stone(), 1), (10, bucket(), 1)]);
        inventory.click(Window::Player, ClickMode::Clone, 9, 2);
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(stone(), 64)));
        assert_eq!(
            inventory.slot(9).cloned(),
            Some(Stack::new(stone(), 1)),
            "unchanged"
        );

        // And the number is the item's, not 64.
        let mut inventory = with(&[(10, bucket(), 1)]);
        inventory.click(Window::Player, ClickMode::Clone, 10, 2);
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(bucket(), 1)));
    }

    #[test]
    fn q_drops_one_and_control_q_drops_the_stack() {
        let mut inventory = with(&[(36, stone(), 3)]);
        inventory.click(Window::Player, ClickMode::Throw, 36, 0);
        assert_eq!(inventory.slot(36).map(|s| s.count), Some(2));
        inventory.click(Window::Player, ClickMode::Throw, 36, 1);
        assert_eq!(inventory.slot(36).cloned(), None);
    }

    #[test]
    fn clicking_outside_the_window_drops_what_is_on_the_cursor() {
        let mut inventory = with(&[(9, stone(), 4)]);
        inventory.click(Window::Player, ClickMode::Pickup, 9, 0);
        inventory.click(Window::Player, ClickMode::Pickup, OUTSIDE, 1);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(3));
        inventory.click(Window::Player, ClickMode::Pickup, OUTSIDE, 0);
        assert_eq!(inventory.cursor().cloned(), None);
    }

    #[test]
    fn a_left_drag_splits_evenly_and_keeps_the_remainder() {
        let mut inventory = with(&[(9, stone(), 10)]);
        inventory.click(Window::Player, ClickMode::Pickup, 9, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(10));

        inventory.click(Window::Player, ClickMode::QuickCraft, -999, 0);
        inventory.click(Window::Player, ClickMode::QuickCraft, 10, 1);
        inventory.click(Window::Player, ClickMode::QuickCraft, 11, 1);
        inventory.click(Window::Player, ClickMode::QuickCraft, 12, 1);
        inventory.click(Window::Player, ClickMode::QuickCraft, -999, 2);

        // Three each, one left over.
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(3));
        assert_eq!(inventory.slot(11).map(|s| s.count), Some(3));
        assert_eq!(inventory.slot(12).map(|s| s.count), Some(3));
        assert_eq!(inventory.cursor().map(|s| s.count), Some(1));
    }

    #[test]
    fn a_right_drag_puts_one_in_each() {
        let mut inventory = with(&[(9, stone(), 10)]);
        inventory.click(Window::Player, ClickMode::Pickup, 9, 0);
        inventory.click(Window::Player, ClickMode::QuickCraft, -999, 4);
        inventory.click(Window::Player, ClickMode::QuickCraft, 10, 5);
        inventory.click(Window::Player, ClickMode::QuickCraft, 11, 5);
        inventory.click(Window::Player, ClickMode::QuickCraft, -999, 6);
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(1));
        assert_eq!(inventory.slot(11).map(|s| s.count), Some(1));
        assert_eq!(inventory.cursor().map(|s| s.count), Some(8));
    }

    #[test]
    fn a_drag_interrupted_by_another_click_applies_nothing() {
        let mut inventory = with(&[(9, stone(), 10)]);
        inventory.click(Window::Player, ClickMode::Pickup, 9, 0);
        inventory.click(Window::Player, ClickMode::QuickCraft, -999, 0);
        inventory.click(Window::Player, ClickMode::QuickCraft, 10, 1);
        // A pickup arrives mid-drag. The drag is abandoned, and the end that
        // arrives after it does nothing.
        inventory.click(Window::Player, ClickMode::Pickup, 20, 0);
        inventory.click(Window::Player, ClickMode::QuickCraft, -999, 2);
        assert_eq!(inventory.slot(10).cloned(), None, "the drag never landed");
        assert_eq!(inventory.slot(20).map(|s| s.count), Some(10));
    }

    #[test]
    fn double_click_gathers_the_partial_stacks_before_the_full_ones() {
        // A full stack in slot 9, loose ones in 10 and 11. Picking up 5 and
        // double-clicking should empty the loose slots and leave the full
        // stack alone until they run out.
        let mut inventory = with(&[
            (9, stone(), 64),
            (10, stone(), 7),
            (11, stone(), 3),
            (12, stone(), 5),
        ]);
        inventory.click(Window::Player, ClickMode::Pickup, 12, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(5));
        inventory.click(Window::Player, ClickMode::PickupAll, 12, 0);
        // 5 + 7 + 3 = 15, then 49 taken off the full stack to reach 64.
        assert_eq!(inventory.cursor().map(|s| s.count), Some(64));
        assert_eq!(inventory.slot(10).cloned(), None);
        assert_eq!(inventory.slot(11).cloned(), None);
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(15));
    }

    #[test]
    fn closing_the_window_puts_the_cursor_and_the_grid_back_rather_than_deleting_them() {
        let mut inventory = with(&[(9, stone(), 4), (CRAFTING_START, dirt(), 2)]);
        inventory.click(Window::Player, ClickMode::Pickup, 9, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(4));
        inventory.closed(Window::Player);
        assert_eq!(inventory.cursor().cloned(), None);
        assert_eq!(inventory.slot(CRAFTING_START).cloned(), None);
        // Both landed somewhere in the inventory.
        let total: u32 = inventory
            .slots()
            .iter()
            .flatten()
            .map(|s| u32::from(s.count))
            .sum();
        assert_eq!(total, 6);
    }

    #[test]
    fn a_click_naming_a_slot_that_is_not_one_changes_nothing() {
        let mut inventory = with(&[(9, stone(), 4)]);
        for slot in [-1i16, 46, 1000] {
            assert!(inventory
                .click(Window::Player, ClickMode::Pickup, slot, 0)
                .is_empty());
        }
        // And the crafting output, which is a real slot number and still not
        // one a click may take from.
        assert!(inventory
            .click(Window::Player, ClickMode::Pickup, 0, 0)
            .is_empty());
        assert_eq!(inventory.slot(9).cloned(), Some(Stack::new(stone(), 4)));
    }

    #[test]
    fn the_changed_mask_names_exactly_the_slots_that_moved() {
        let mut inventory = with(&[(9, stone(), 4)]);
        let changed = inventory.click(Window::Player, ClickMode::Swap, 9, 3);
        let moved: Vec<usize> = changed.iter().collect();
        assert_eq!(moved, vec![9, 39]);
        assert!(!changed.cursor());
    }

    #[test]
    fn a_stack_survives_the_wire_and_back() {
        for (item, count) in [(stone(), 64u8), (bucket(), 1), (dirt(), 17)] {
            let stack = Stack::new(item, count);
            assert_eq!(from_wire(&to_wire(Some(&stack))), Some(stack));
        }
        assert_eq!(to_wire(None), Slot::Empty);
        assert_eq!(from_wire(&Slot::Empty), None);
    }

    #[test]
    fn every_wearable_item_has_a_slot_to_be_worn_in() {
        // `minecraft:enchantable/equippable` is vanilla's own list of the 34
        // things a player wears, and this table places 32 of them from four
        // slot-naming tags and the other two by name. A version that adds a
        // wearable — or renames one of those two — fails here rather than
        // shipping an item that cannot be put on.
        let wearable = dust_registry::tags::wire(TagRegistry::Item)
            .expect("the item tags resolve")
            .into_iter()
            .find(|tag| tag.id == "minecraft:enchantable/equippable")
            .expect("1.21.1 has that tag");
        let mut placed_nowhere = Vec::new();
        for id in &wearable.entries {
            let item = Item::from_protocol_id(*id).expect("a tag member is an item");
            if worn_in(item).is_none() {
                placed_nowhere.push(item.name());
            }
        }
        assert!(
            placed_nowhere.is_empty(),
            "these are worn somewhere and this table says nowhere: {placed_nowhere:?}"
        );
        assert_eq!(wearable.entries.len(), 34, "the wearables on 1.21.1");
        // The shield is not worn and so is not in that tag; it is the one
        // offhand answer and nothing else guards it.
        assert_eq!(worn_in(item("minecraft:shield")), Some(OFFHAND));
    }

    #[test]
    fn the_four_armour_slots_take_only_what_is_worn_in_them() {
        assert_eq!(worn_in(helmet()), Some(ARMOUR_HEAD));
        assert_eq!(worn_in(boots()), Some(ARMOUR_FEET));
        assert_eq!(worn_in(item("minecraft:elytra")), Some(ARMOUR_CHEST));
        assert_eq!(worn_in(item("minecraft:carved_pumpkin")), Some(ARMOUR_HEAD));
        assert_eq!(worn_in(stone()), None);

        assert!(may_place(ARMOUR_HEAD, helmet(), None, None));
        assert!(!may_place(ARMOUR_HEAD, boots(), None, None));
        assert!(!may_place(ARMOUR_FEET, helmet(), None, None));
        assert!(!may_place(ARMOUR_HEAD, stone(), None, None));
        // The offhand and the inventory take anything, the output nothing.
        assert!(may_place(OFFHAND, stone(), None, None));
        assert!(may_place(MAIN_START, helmet(), None, None));
        assert!(!may_place(CRAFTING_OUTPUT, stone(), None, None));
    }

    #[test]
    fn a_left_click_cannot_put_a_block_on_a_players_head() {
        let mut inventory = with(&[(MAIN_START, stone(), 9)]);
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        let changed = inventory.click(Window::Player, ClickMode::Pickup, ARMOUR_HEAD as i16, 0);
        assert!(changed.is_empty(), "a refused click changes nothing");
        assert_eq!(inventory.slot(ARMOUR_HEAD).cloned(), None);
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(stone(), 9)));

        // Boots into the helmet slot are refused for the same reason, and the
        // helmet slot takes the helmet.
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        let mut inventory = with(&[(MAIN_START, boots(), 1), (MAIN_START + 1, helmet(), 1)]);
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        assert!(inventory
            .click(Window::Player, ClickMode::Pickup, ARMOUR_HEAD as i16, 0)
            .is_empty());
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        inventory.click(
            Window::Player,
            ClickMode::Pickup,
            (MAIN_START + 1) as i16,
            0,
        );
        inventory.click(Window::Player, ClickMode::Pickup, ARMOUR_HEAD as i16, 0);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(helmet(), 1))
        );
    }

    #[test]
    fn shift_click_puts_armour_on_and_takes_it_off_again() {
        let mut inventory = with(&[(MAIN_START, helmet(), 1)]);
        inventory.click(Window::Player, ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(helmet(), 1))
        );
        assert_eq!(inventory.slot(MAIN_START).cloned(), None);

        // Off again, and into the inventory rather than back onto the head.
        inventory.click(Window::Player, ClickMode::QuickMove, ARMOUR_HEAD as i16, 0);
        assert_eq!(inventory.slot(ARMOUR_HEAD).cloned(), None);
        assert_eq!(
            inventory.slot(MAIN_START).cloned(),
            Some(Stack::new(helmet(), 1))
        );
    }

    #[test]
    fn a_second_helmet_goes_to_the_hotbar_because_the_head_is_taken() {
        let mut inventory = with(&[
            (ARMOUR_HEAD, helmet(), 1),
            (MAIN_START, item("minecraft:golden_helmet"), 1),
        ]);
        inventory.click(Window::Player, ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(helmet(), 1))
        );
        assert_eq!(
            inventory.slot(HOTBAR_START).cloned(),
            Some(Stack::new(item("minecraft:golden_helmet"), 1))
        );
    }

    #[test]
    fn a_shield_shift_clicks_into_the_offhand_unless_it_is_taken() {
        let shield = item("minecraft:shield");
        let mut inventory = with(&[(MAIN_START, shield, 1)]);
        inventory.click(Window::Player, ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(OFFHAND).cloned(),
            Some(Stack::new(shield, 1))
        );

        let mut inventory = with(&[(MAIN_START, shield, 1), (OFFHAND, stone(), 1)]);
        inventory.click(Window::Player, ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(OFFHAND).cloned(),
            Some(Stack::new(stone(), 1))
        );
        assert_eq!(
            inventory.slot(HOTBAR_START).cloned(),
            Some(Stack::new(shield, 1))
        );
    }

    #[test]
    fn the_offhand_and_the_crafting_grid_empty_into_the_inventory() {
        let mut inventory = with(&[(OFFHAND, stone(), 9), (CRAFTING_START, stone(), 4)]);
        inventory.click(Window::Player, ClickMode::QuickMove, OFFHAND as i16, 0);
        assert_eq!(
            inventory.slot(MAIN_START).cloned(),
            Some(Stack::new(stone(), 9))
        );
        inventory.click(
            Window::Player,
            ClickMode::QuickMove,
            CRAFTING_START as i16,
            0,
        );
        assert_eq!(
            inventory.slot(MAIN_START).cloned(),
            Some(Stack::new(stone(), 13))
        );
        assert_eq!(inventory.slot(CRAFTING_START).cloned(), None);
    }

    #[test]
    fn a_number_key_over_an_armour_slot_obeys_the_slot_and_not_the_key() {
        let mut inventory = with(&[
            (ARMOUR_HEAD, helmet(), 1),
            (HOTBAR_START, stone(), 6),
            (HOTBAR_START + 1, item("minecraft:golden_helmet"), 1),
        ]);
        let changed = inventory.click(Window::Player, ClickMode::Swap, ARMOUR_HEAD as i16, 0);
        assert!(changed.is_empty(), "a block cannot be swapped onto a head");
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(helmet(), 1))
        );

        inventory.click(Window::Player, ClickMode::Swap, ARMOUR_HEAD as i16, 1);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(item("minecraft:golden_helmet"), 1))
        );
        assert_eq!(
            inventory.slot(HOTBAR_START + 1).cloned(),
            Some(Stack::new(helmet(), 1))
        );
    }

    #[test]
    fn an_armour_slot_holds_one_of_something_that_stacks_to_sixty_four() {
        // A player head is worn and stacks to 64. `ArmorSlot.getMaxStackSize`
        // is 1, so one goes on and the rest stays on the cursor.
        assert_eq!(head().max_stack_size(), 64);
        let mut inventory = with(&[(MAIN_START, head(), 9)]);
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        inventory.click(Window::Player, ClickMode::Pickup, ARMOUR_HEAD as i16, 0);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(head(), 1))
        );
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(head(), 8)));

        // And a shift-click puts one on the head and then keeps going: the
        // second pass finds the head slot occupied, takes the ordinary arm and
        // sends the other eight to the hotbar. Measured against a real server;
        // a single pass leaves them in the slot that was clicked.
        let mut inventory = with(&[(MAIN_START, head(), 9)]);
        inventory.click(Window::Player, ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(head(), 1))
        );
        assert_eq!(inventory.slot(MAIN_START).cloned(), None);
        assert_eq!(
            inventory.slot(HOTBAR_START).cloned(),
            Some(Stack::new(head(), 8))
        );
    }

    #[test]
    fn a_drag_skips_the_slot_that_will_not_take_it_and_the_rest_share_more() {
        // Twenty-one cobblestone dragged across the chest slot and one
        // ordinary slot. The chest slot never joins the drag, so the share is
        // 21/1 and not 21/2 — measured against a real server.
        let mut inventory = with(&[(MAIN_START, stone(), 21)]);
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        inventory.click(Window::Player, ClickMode::QuickCraft, OUTSIDE, 0);
        inventory.click(
            Window::Player,
            ClickMode::QuickCraft,
            ARMOUR_CHEST as i16,
            1,
        );
        inventory.click(
            Window::Player,
            ClickMode::QuickCraft,
            (MAIN_START + 8) as i16,
            1,
        );
        inventory.click(Window::Player, ClickMode::QuickCraft, OUTSIDE, 2);
        assert_eq!(inventory.slot(ARMOUR_CHEST).cloned(), None);
        assert_eq!(
            inventory.slot(MAIN_START + 8).cloned(),
            Some(Stack::new(stone(), 21))
        );
        assert_eq!(inventory.cursor().cloned(), None);
    }

    #[test]
    fn a_block_goes_in_the_offhand_because_the_offhand_takes_anything() {
        let mut inventory = with(&[(MAIN_START, stone(), 9), (OFFHAND, dirt(), 2)]);
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        inventory.click(Window::Player, ClickMode::Pickup, OFFHAND as i16, 0);
        assert_eq!(
            inventory.slot(OFFHAND).cloned(),
            Some(Stack::new(stone(), 9))
        );
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(dirt(), 2)));
    }
    fn stone_with(components: ComponentPatch, count: u8) -> Stack {
        Stack::with_components(item("minecraft:stone"), count, components)
    }

    #[test]
    fn a_left_click_pours_one_stack_into_another_only_when_the_components_match() {
        // The whole point of this module's change, from the player's side: a
        // named stack poured onto a plain one would take the name off both.
        let mut inventory = Inventory::default();
        inventory.slots[MAIN_START] = Some(stone_with(named(), 16));
        inventory.cursor = Some(stone_with(named(), 16));
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(MAIN_START).map(|s| s.count),
            Some(32),
            "two stacks with the same components must merge"
        );
        assert_eq!(inventory.cursor(), None);

        let mut inventory = Inventory::default();
        inventory.slots[MAIN_START] = Some(stone_with(named(), 16));
        inventory.cursor = Some(stone_with(ComponentPatch::EMPTY, 16));
        inventory.click(Window::Player, ClickMode::Pickup, MAIN_START as i16, 0);
        // A swap, which is what vanilla does with two stacks that are not the
        // same thing. Not a merge, and not a refusal.
        assert_eq!(inventory.slot(MAIN_START).map(|s| s.count), Some(16));
        assert_eq!(
            inventory.slot(MAIN_START).map(|s| s.components.clone()),
            Some(ComponentPatch::EMPTY)
        );
        assert_eq!(
            inventory.cursor().map(|s| s.components.clone()),
            Some(named())
        );
    }

    #[test]
    fn a_shift_click_finds_the_stack_that_matches_and_not_the_one_that_does_not() {
        let mut inventory = Inventory::default();
        // Two partial stacks of stone in the hotbar: one worn, one plain. A
        // shift-clicked worn stack must top up the worn one and leave the
        // plain one alone, even though the plain one comes first.
        inventory.slots[HOTBAR_START] = Some(stone_with(ComponentPatch::EMPTY, 60));
        inventory.slots[HOTBAR_START + 1] = Some(stone_with(worn(3), 60));
        inventory.slots[MAIN_START] = Some(stone_with(worn(3), 8));
        inventory.click(Window::Player, ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(inventory.slot(HOTBAR_START).map(|s| s.count), Some(60));
        assert_eq!(inventory.slot(HOTBAR_START + 1).map(|s| s.count), Some(64));
        // The four that did not fit took an empty slot rather than the plain
        // stack sitting in front of it.
        assert_eq!(inventory.slot(HOTBAR_START + 2).map(|s| s.count), Some(4));
        assert_eq!(inventory.slot(MAIN_START), None);
    }

    #[test]
    fn a_double_click_gathers_the_ones_that_are_the_same_thing() {
        let mut inventory = Inventory {
            cursor: Some(stone_with(worn(3), 1)),
            ..Inventory::default()
        };
        inventory.slots[MAIN_START] = Some(stone_with(worn(3), 10));
        inventory.slots[MAIN_START + 1] = Some(stone_with(ComponentPatch::EMPTY, 10));
        inventory.slots[MAIN_START + 2] = Some(stone_with(worn(9), 10));
        inventory.click(Window::Player, ClickMode::PickupAll, MAIN_START as i16, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(11));
        assert_eq!(inventory.slot(MAIN_START), None);
        assert_eq!(inventory.slot(MAIN_START + 1).map(|s| s.count), Some(10));
        assert_eq!(inventory.slot(MAIN_START + 2).map(|s| s.count), Some(10));
    }

    #[test]
    fn a_drag_skips_a_slot_holding_the_same_item_with_different_components() {
        // The drag decides at the moment a slot joins, and the share is divided
        // by how many joined — so a slot filtered here makes the others' share
        // larger rather than losing its own.
        let mut inventory = Inventory {
            cursor: Some(stone_with(named(), 20)),
            ..Inventory::default()
        };
        inventory.slots[MAIN_START] = Some(stone_with(ComponentPatch::EMPTY, 1));
        inventory.click(Window::Player, ClickMode::QuickCraft, OUTSIDE, 0);
        inventory.click(Window::Player, ClickMode::QuickCraft, MAIN_START as i16, 1);
        inventory.click(
            Window::Player,
            ClickMode::QuickCraft,
            (MAIN_START + 1) as i16,
            1,
        );
        inventory.click(Window::Player, ClickMode::QuickCraft, OUTSIDE, 2);
        assert_eq!(inventory.slot(MAIN_START).map(|s| s.count), Some(1));
        assert_eq!(inventory.slot(MAIN_START + 1).map(|s| s.count), Some(20));
    }

    #[test]
    fn a_middle_click_copies_the_components_the_way_vanilla_copies_the_stack() {
        let mut inventory = Inventory::default();
        inventory.slots[MAIN_START] = Some(stone_with(named(), 1));
        inventory.click(Window::Player, ClickMode::Clone, MAIN_START as i16, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(64));
        assert_eq!(
            inventory.cursor().map(|s| s.components.clone()),
            Some(named())
        );
    }

    #[test]
    fn the_sequence_number_a_client_quotes_is_the_last_one_sent() {
        // Load-bearing for the stale-click check in `net::session`, which
        // compares a click's `state_id` against this. If `state_id` ever
        // returned the *next* number rather than the last one sent, every
        // honest click would look stale and every one of them would be
        // answered with the whole container — which is not a crash, not a
        // wrong item, and not visible in any test that only looks at slots.
        let mut inventory = Inventory::default();
        let sent = inventory.next_state_id();
        assert_eq!(inventory.state_id(), sent);
        let again = inventory.next_state_id();
        assert_eq!(inventory.state_id(), again);
        assert_ne!(again, sent, "and a second update is a different number");
    }

    #[test]
    fn the_equipment_set_reads_the_six_slots_the_wire_numbers() {
        // Each of the six from a *different* item, because a set built with
        // one item in every slot agrees with any permutation of the table —
        // the reason this reads six names and not one.
        let mut inventory = Inventory::default();
        let pieces = [
            (HOTBAR_START, "minecraft:diamond_sword"),
            (OFFHAND, "minecraft:shield"),
            (ARMOUR_FEET, "minecraft:diamond_boots"),
            (ARMOUR_LEGS, "minecraft:diamond_leggings"),
            (ARMOUR_CHEST, "minecraft:diamond_chestplate"),
            (ARMOUR_HEAD, "minecraft:diamond_helmet"),
        ];
        for (slot, name) in pieces {
            inventory.slots[slot] = Some(Stack::new(item(name), 1));
        }

        let worn = inventory.equipment();
        for (wire_slot, (_, name)) in pieces.into_iter().enumerate() {
            assert_eq!(
                worn[wire_slot].as_ref().map(|stack| stack.item),
                Some(item(name)),
                "wire slot {wire_slot} should hold {name}"
            );
        }
    }

    #[test]
    fn the_main_hand_follows_the_hotbar_slot_the_player_selected() {
        // The one equipment slot that is not a fixed container index. A player
        // scrolling from an empty slot to a sword has armed themselves without
        // touching the container, and everybody else has to see it.
        let mut inventory = Inventory::default();
        inventory.slots[HOTBAR_START + 3] = Some(Stack::new(item("minecraft:diamond_sword"), 1));

        assert_eq!(inventory.equipment()[EQUIP_MAIN_HAND as usize], None);
        assert!(inventory.select(3));
        assert_eq!(
            inventory.equipment()[EQUIP_MAIN_HAND as usize]
                .as_ref()
                .map(|stack| stack.item),
            Some(item("minecraft:diamond_sword"))
        );
    }

    #[test]
    fn a_worn_stack_carries_its_components_into_the_equipment_set() {
        // A named sword everybody else sees as a plain one is #54's defect
        // reappearing one layer out.
        install_component_types();
        let mut inventory = Inventory::default();
        inventory.slots[HOTBAR_START] = Some(stone_with(named(), 1));
        assert_eq!(
            inventory.equipment()[EQUIP_MAIN_HAND as usize]
                .as_ref()
                .map(|stack| stack.components.clone()),
            Some(named())
        );
    }

    #[test]
    fn a_named_stack_survives_the_wire_and_comes_back_the_same_stack() {
        let stack = stone_with(named(), 3);
        let wire = to_wire(Some(&stack));
        assert_eq!(from_wire(&wire), Some(stack));
    }

    #[test]
    fn a_creative_write_keeps_the_components_the_client_sent() {
        install_component_types();
        let mut inventory = Inventory::default();
        let sent = Slot::Present {
            count: 1,
            item_id: item("minecraft:stone").protocol_id() as i32,
            components: named(),
        };
        assert!(inventory
            .set_creative(MAIN_START as i16, &sent)
            .unwrap()
            .has(MAIN_START));
        assert_eq!(
            inventory.slot(MAIN_START).map(|s| s.components.clone()),
            Some(named())
        );
    }

    #[test]
    fn the_boot_path_installs_the_component_registry() {
        // Without this the whole feature is inert: `dust-protocol` refuses
        // every component by number, and it would do it quietly, one packet at
        // a time, on a server that looked like it was working.
        install_component_types();
        assert!(dust_protocol::components::type_names_installed());
        assert_eq!(
            dust_protocol::components::type_name(
                dust_registry::Registry::from_name("minecraft:data_component_type")
                    .and_then(|r| r.entry_id("minecraft:custom_name"))
                    .expect("in the registry") as i32
            ),
            Some("minecraft:custom_name")
        );
    }
}
