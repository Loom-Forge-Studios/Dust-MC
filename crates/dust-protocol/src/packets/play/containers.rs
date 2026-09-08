//! Field types for containers, inventories and the recipes that fill them.
//!
//! # The Slot seam runs through everything here
//!
//! Every item-carrying field in this module bottoms out in [`crate::types::Slot`],
//! which decodes an envelope and refuses component-bearing stacks by name. That
//! decision is inherited, not relitigated: a stack whose added components are
//! present cannot be stepped over without knowing every component's layout,
//! and guessing loses the position of every byte after it. What these types add
//! on top is the *structure around* the stacks — which slot, how many, what
//! terminator ends the list — so that when the wall falls, only `Slot` itself
//! changes.

use crate::nbt::Nbt;
use crate::text::Component;
use crate::types::{Decode, Encode, Identifier, ProtocolString, Slot, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{var_int_enum, wire_struct, ProtocolVersion};

// ---------------------------------------------------------------------------
// Equipment: entries until the high bit says stop
// ---------------------------------------------------------------------------

var_int_enum! {
    /// Where on an entity a piece of equipment sits.
    ///
    /// Travels inside a byte whose top bit is the more-items flag, so the
    /// discriminant lives in the low seven bits and this enum's VarInt codec
    /// is deliberately unused — see [`EquipmentEntries`] for the packing.
    pub enum EquipmentSlot {
        MainHand = 0,
        OffHand = 1,
        Boots = 2,
        Leggings = 3,
        Chestplate = 4,
        Helmet = 5,
        Body = 6,
    }
}

/// One entry of a [`EquipmentEntries`] list, before the terminator logic.
#[derive(Debug, Clone, PartialEq)]
pub struct EquipmentEntry {
    pub slot: EquipmentSlot,
    pub item: Slot,
}

/// An entity's changed equipment: entries until one without the continuation
/// bit, exactly like metadata's terminator but spelled in reverse.
///
/// The list must have at least one entry — a bare "no equipment" is not a
/// message worth a packet — so encoding an empty list is refused rather than
/// written as a frame the peer reads as garbage.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EquipmentEntries(pub Vec<EquipmentEntry>);

const CONTINUES: u8 = 0x80;

impl Decode for EquipmentEntries {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let mut entries = Vec::new();
        loop {
            let raw = input.read_u8()?;
            let slot = EquipmentSlot::from_discriminant(i32::from(raw & !CONTINUES)).ok_or(
                DecodeError::UnknownVariant {
                    name: "EquipmentSlot",
                    value: i32::from(raw & !CONTINUES),
                },
            )?;
            entries.push(EquipmentEntry {
                slot,
                item: Slot::decode(input, version)?,
            });
            if raw & CONTINUES == 0 {
                return Ok(Self(entries));
            }
        }
    }
}

impl Encode for EquipmentEntries {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        let Some((last, rest)) = self.0.split_last() else {
            return Err(EncodeError::Unsupported {
                field: "equipment entries",
                why: "an equipment update carries at least one entry",
            });
        };
        for entry in rest {
            out.write_u8(entry.slot.discriminant() as u8 | CONTINUES);
            entry.item.encode(out, version)?;
        }
        out.write_u8(last.slot.discriminant() as u8);
        last.item.encode(out, version)
    }
}

// ---------------------------------------------------------------------------
// Container clicks: what the client believes changed
// ---------------------------------------------------------------------------

wire_struct! {
    /// One slot the client reports having changed, and what it now holds.
    pub struct ChangedSlot {
        number: i16,
        item: Slot,
    }
}

// ---------------------------------------------------------------------------
// Villager trades: an item cost that is almost, but not quite, a Slot
// ---------------------------------------------------------------------------

/// The price side of a villager trade: item id, count, and inline components.
///
/// Not a [`Slot`] because there is no removals list here — only additions —
/// and not decodable beyond the header for the reason every component-bearing
/// value shares. The refusal names the count that lied rather than reading
/// payloads whose length nobody knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeItem {
    pub item_id: VarInt,
    pub count: VarInt,
}

impl Decode for TradeItem {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let item_id = VarInt::decode(input, _version)?;
        let count = VarInt::decode(input, _version)?;
        let components = input.read_var_int()?;
        if components != 0 {
            return Err(DecodeError::Unsupported {
                field: "trade item components",
                why: "a structured component carries no length, so an unknown one cannot be \
                      skipped without losing the position of every field after it",
            });
        }
        Ok(Self { item_id, count })
    }
}

impl Encode for TradeItem {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.item_id.encode(out, version)?;
        self.count.encode(out, version)?;
        out.write_var_int(0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Recipes: a typed envelope over the recipe registry's own layouts
// ---------------------------------------------------------------------------

var_int_enum! {
    /// Which tab of the crafting book a recipe appears under.
    pub enum CraftingBookCategory {
        Building = 0,
        Redstone = 1,
        Equipment = 2,
        Misc = 3,
    }
}

var_int_enum! {
    /// Which tab of a furnace-family book a recipe appears under.
    ///
    /// A different table from [`CraftingBookCategory`] because furnaces group
    /// by what they cook, not by what they build. Two enums where one sparse
    /// union would do, because the two travel in different packets' contexts
    /// and conflating them lets `Building` appear where only food belongs.
    pub enum CookingBookCategory {
        Food = 0,
        Blocks = 1,
        Misc = 2,
    }
}

wire_struct! {
    /// One ingredient slot of a recipe: any of these stacks will do.
    ///
    /// Each stack should carry a count of one; the *recipe* decides quantities.
    pub struct Ingredient {
        items: Vec<Slot>,
    }
}

wire_struct! {
    /// A shaped recipe's grid, its result and whether it toasts.
    pub struct CraftingShapedData {
        group: ProtocolString,
        category: CraftingBookCategory,
        width: VarInt,
        height: VarInt,
        /// Exactly `width * height` ingredients, indexed x + y * width.
        ingredients: Vec<Ingredient>,
        result: Slot,
        show_notification: bool,
    }
}

wire_struct! {
    /// A shapeless recipe's pile of ingredients and its result.
    pub struct CraftingShapelessData {
        group: ProtocolString,
        category: CraftingBookCategory,
        ingredients: Vec<Ingredient>,
        result: Slot,
    }
}

wire_struct! {
    /// A furnace-family recipe: one ingredient, one result, heat and time.
    pub struct CookingData {
        group: ProtocolString,
        category: CookingBookCategory,
        ingredient: Ingredient,
        result: Slot,
        experience: f32,
        cooking_time: VarInt,
    }
}

wire_struct! {
    /// A stonecutter recipe: one ingredient cut into one result.
    pub struct StonecuttingData {
        group: ProtocolString,
        ingredient: Ingredient,
        result: Slot,
    }
}

wire_struct! {
    /// Netherite upgrades: template plus base plus addition becomes result.
    ///
    /// **No `group`**, unlike every other recipe here. `SmithingTransformRecipe`'s
    /// stream codec is four fields and the group is not one of them — it is a
    /// recipe-book grouping that the smithing serialiser never carried. The
    /// field was here until the first packet was sent to a real client, which
    /// read the recipe id that followed as a stack's components and gave up on
    /// the connection; a round trip against this crate's own decoder agreed
    /// with itself for the whole time it was wrong.
    pub struct SmithingTransformData {
        template: Ingredient,
        base: Ingredient,
        addition: Ingredient,
        result: Slot,
    }
}

wire_struct! {
    /// Armor trims: template plus base plus addition, with the result derived.
    ///
    /// No `group`, for the reason [`SmithingTransformData`] gives.
    pub struct SmithingTrimData {
        template: Ingredient,
        base: Ingredient,
        addition: Ingredient,
    }
}

/// One declared recipe: its registry id, its type, and the type's own layout.
///
/// The type id leads the data and picks which struct follows, so this is
/// another value-dependent tail — held together as one type because a recipe
/// is not decodable at all unless both halves agree about which half comes
/// next. Unknown type ids are refused rather than skipped for the usual reason:
/// their data has no length, and skipping one loses the rest of the packet.
///
/// The special "crafting_special_*" family (firework stars, map cloning, armor
/// dyeing and friends) carries only a book category — its ingredients are
/// computed client-side — which is why those ids share one arm.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub id: Identifier,
    pub kind: RecipeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecipeKind {
    CraftingShaped(CraftingShapedData),
    CraftingShapeless(CraftingShapelessData),
    /// `crafting_special_*` and `crafting_decorated_pot`: the id is kept
    /// because twelve of them share this one layout.
    Special {
        type_id: i32,
        category: CraftingBookCategory,
    },
    Cooking {
        type_id: i32,
        data: CookingData,
    },
    Stonecutting(StonecuttingData),
    SmithingTransform(SmithingTransformData),
    SmithingTrim(SmithingTrimData),
}

impl Recipe {
    const SHAPED: i32 = 0;
    const SHAPELESS: i32 = 1;
    /// The special crafting ids: eleven classic ones, then the decorated pot
    /// parked at 22 after the cooking family took 15 through 21.
    const SPECIAL_IDS: [i32; 12] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 22];
    const SMELTING: i32 = 15;
    const BLASTING: i32 = 16;
    const SMOKING: i32 = 17;
    const CAMPFIRE_COOKING: i32 = 18;
    const STONECUTTING: i32 = 19;
    const SMITHING_TRANSFORM: i32 = 20;
    const SMITHING_TRIM: i32 = 21;
}

impl Decode for Recipe {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let id = Identifier::decode(input, version)?;
        let type_id = input.read_var_int()?;
        let kind = if type_id == Self::SHAPED {
            RecipeKind::CraftingShaped(CraftingShapedData::decode(input, version)?)
        } else if type_id == Self::SHAPELESS {
            RecipeKind::CraftingShapeless(CraftingShapelessData::decode(input, version)?)
        } else if Self::SPECIAL_IDS.contains(&type_id) {
            RecipeKind::Special {
                type_id,
                category: CraftingBookCategory::decode(input, version)?,
            }
        } else if matches!(
            type_id,
            Self::SMELTING | Self::BLASTING | Self::SMOKING | Self::CAMPFIRE_COOKING
        ) {
            RecipeKind::Cooking {
                type_id,
                data: CookingData::decode(input, version)?,
            }
        } else if type_id == Self::STONECUTTING {
            RecipeKind::Stonecutting(StonecuttingData::decode(input, version)?)
        } else if type_id == Self::SMITHING_TRANSFORM {
            RecipeKind::SmithingTransform(SmithingTransformData::decode(input, version)?)
        } else if type_id == Self::SMITHING_TRIM {
            RecipeKind::SmithingTrim(SmithingTrimData::decode(input, version)?)
        } else {
            return Err(DecodeError::UnknownVariant {
                name: "recipe type",
                value: type_id,
            });
        };
        Ok(Self { id, kind })
    }
}

impl Encode for Recipe {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.id.encode(out, version)?;
        match &self.kind {
            RecipeKind::CraftingShaped(data) => {
                out.write_var_int(Recipe::SHAPED);
                data.encode(out, version)
            }
            RecipeKind::CraftingShapeless(data) => {
                out.write_var_int(Recipe::SHAPELESS);
                data.encode(out, version)
            }
            RecipeKind::Special { type_id, category } => {
                out.write_var_int(*type_id);
                category.encode(out, version)
            }
            RecipeKind::Cooking { type_id, data } => {
                out.write_var_int(*type_id);
                data.encode(out, version)
            }
            RecipeKind::Stonecutting(data) => {
                out.write_var_int(Recipe::STONECUTTING);
                data.encode(out, version)
            }
            RecipeKind::SmithingTransform(data) => {
                out.write_var_int(Recipe::SMITHING_TRANSFORM);
                data.encode(out, version)
            }
            RecipeKind::SmithingTrim(data) => {
                out.write_var_int(Recipe::SMITHING_TRIM);
                data.encode(out, version)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Scoreboards: how a score number is dressed up
// ---------------------------------------------------------------------------

/// How the client renders a score's number, or replaces it entirely.
///
/// Absent means "use the objective's format"; this type is the per-score or
/// per-objective override itself, so there is no absent case here — presence
/// is decided by the boolean that precedes it in the packet.
#[derive(Debug, Clone, PartialEq)]
pub enum NumberFormat {
    /// Show nothing at all.
    Blank,
    /// Style the number with text-component styling fields.
    Styled(Nbt),
    /// Replace the number with fixed text.
    Fixed(Component),
}

impl Decode for NumberFormat {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        match input.read_var_int()? {
            0 => Ok(Self::Blank),
            1 => Nbt::decode(input, version).map(Self::Styled),
            2 => Component::decode(input, version).map(Self::Fixed),
            other => Err(DecodeError::UnknownVariant {
                name: "NumberFormat",
                value: other,
            }),
        }
    }
}

impl Encode for NumberFormat {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match self {
            Self::Blank => out.write_var_int(0),
            Self::Styled(nbt) => {
                out.write_var_int(1);
                return nbt.encode(out, version);
            }
            Self::Fixed(component) => {
                out.write_var_int(2);
                return component.encode(out, version);
            }
        }
        Ok(())
    }
}

wire_struct! {
    /// One changed statistic: which category, which counter, what it says now.
    ///
    /// Both ids are raw registry ids — resolving `minecraft:mined` id 42 to a
    /// block is the registry crate's job, and this layer would only bake a
    /// version's numbering into a type.
    pub struct StatisticEntry {
        category: VarInt,
        statistic: VarInt,
        value: VarInt,
    }
}

var_int_enum! {
    /// Which kind of slot interaction a click reports.
    ///
    /// The ids are the protocol's own; the button byte's meaning changes with
    /// this, which is why the two travel together in one packet rather than
    /// being interpreted apart.
    pub enum ClickType {
        Pickup = 0,
        QuickMove = 1,
        Swap = 2,
        Clone = 3,
        Throw = 4,
        QuickCraft = 5,
        PickupAll = 6,
    }
}

/// One villager trade: what goes in, what comes out, and the bookkeeping the
/// client renders around it.
///
/// The inputs use [`TradeItem`] — id, count and inline components — while the
/// output is a full [`Slot`]. The output is where enchantments live, which is
/// why it is the side that can refuse; the refusal inherits Slot's own.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeOffer {
    pub buy_a: TradeItem,
    pub sell: Slot,
    /// The second input of a two-item trade; absent for most trades.
    pub buy_b: Option<TradeItem>,
    /// Whether the trade is currently greyed out.
    pub disabled: bool,
    pub uses: i32,
    pub max_uses: i32,
    pub villager_xp: i32,
    /// Added to the price when reputation or demand adjusts it; zero or
    /// negative.
    pub special_price: i32,
    /// How strongly reputation and demand move the price. Low (0.05) or
    /// high (0.2) in vanilla data.
    pub price_multiplier: f32,
    pub demand: i32,
}

impl Decode for TradeOffer {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let buy_a = TradeItem::decode(input, version)?;
        let sell = Slot::decode(input, version)?;
        let buy_b = Option::<TradeItem>::decode(input, version)?;
        let disabled = input.read_bool()?;
        let uses = input.read_i32()?;
        let max_uses = input.read_i32()?;
        let villager_xp = input.read_i32()?;
        let special_price = input.read_i32()?;
        let price_multiplier = input.read_f32()?;
        let demand = input.read_i32()?;
        Ok(Self {
            buy_a,
            sell,
            buy_b,
            disabled,
            uses,
            max_uses,
            villager_xp,
            special_price,
            price_multiplier,
            demand,
        })
    }
}

impl Encode for TradeOffer {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.buy_a.encode(out, version)?;
        self.sell.encode(out, version)?;
        self.buy_b.encode(out, version)?;
        out.write_bool(self.disabled);
        out.write_i32(self.uses);
        out.write_i32(self.max_uses);
        out.write_i32(self.villager_xp);
        out.write_i32(self.special_price);
        out.write_f32(self.price_multiplier);
        out.write_i32(self.demand);
        Ok(())
    }
}

/// Everything after the window id on a merchant offer sync.
#[derive(Debug, Clone, PartialEq)]
pub struct MerchantOffersBody {
    pub offers: Vec<TradeOffer>,
    /// The trader's progression tier, 1..=5, shown above the trade list.
    pub villager_level: VarInt,
    pub experience: VarInt,
    /// False for the wandering trader, which hides level and restock hints.
    pub regular_villager: bool,
    pub can_restock: bool,
}

impl Decode for MerchantOffersBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            offers: Vec::<TradeOffer>::decode(input, version)?,
            villager_level: VarInt::decode(input, version)?,
            experience: VarInt::decode(input, version)?,
            regular_villager: input.read_bool()?,
            can_restock: input.read_bool()?,
        })
    }
}

impl Encode for MerchantOffersBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.offers.encode(out, version)?;
        self.villager_level.encode(out, version)?;
        self.experience.encode(out, version)?;
        out.write_bool(self.regular_villager);
        out.write_bool(self.can_restock);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The recipe book: which recipes the client may craft
// ---------------------------------------------------------------------------

var_int_enum! {
    /// Which tab group a recipe-book toggle or setting belongs to.
    ///
    /// Not [`CraftingBookCategory`] and not [`CookingBookCategory`]: this
    /// splits by *machine* — crafting, furnace, blast furnace, smoker —
    /// because that is how the book groups its display toggles. Three
    /// four-value tables where one sparse union would do; conflating any two
    /// puts a furnace flag where a food tab belongs.
    pub enum RecipeBookType {
        Crafting = 0,
        Furnace = 1,
        BlastFurnace = 2,
        Smoker = 3,
    }
}

var_int_enum! {
    /// How a recipe-book update changes the client's set.
    ///
    /// `Init` replaces everything and carries the highlighted half; add and
    /// remove patch one list.
    pub enum RecipeBookAction {
        Init = 0,
        Add = 1,
        Remove = 2,
    }
}

/// The eight display booleans the recipe book keeps per tab group, in wire
/// order: open then filtered for crafting, furnace, blast furnace and
/// smoker.
///
/// A struct rather than eight loose fields because the order is load-bearing
/// and a definition with eight `bool`s named only by position is how the
/// furnace flag ends up driving the smoker's icon. They are client-side
/// display state; nothing here decides what may be crafted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecipeBookSettings {
    pub crafting_open: bool,
    pub crafting_filter: bool,
    pub furnace_open: bool,
    pub furnace_filter: bool,
    pub blast_furnace_open: bool,
    pub blast_furnace_filter: bool,
    pub smoker_open: bool,
    pub smoker_filter: bool,
}

impl RecipeBookSettings {
    const FIELDS: usize = 8;

    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let mut flags = [false; Self::FIELDS];
        for flag in &mut flags {
            *flag = input.read_bool()?;
        }
        Ok(Self {
            crafting_open: flags[0],
            crafting_filter: flags[1],
            furnace_open: flags[2],
            furnace_filter: flags[3],
            blast_furnace_open: flags[4],
            blast_furnace_filter: flags[5],
            smoker_open: flags[6],
            smoker_filter: flags[7],
        })
    }

    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_bool(self.crafting_open);
        out.write_bool(self.crafting_filter);
        out.write_bool(self.furnace_open);
        out.write_bool(self.furnace_filter);
        out.write_bool(self.blast_furnace_open);
        out.write_bool(self.blast_furnace_filter);
        out.write_bool(self.smoker_open);
        out.write_bool(self.smoker_filter);
        Ok(())
    }
}

/// Everything after the packet id on a recipe-book sync.
///
/// Two id lists travel together because the book shows two collections: what
/// the player may craft (`changed`) and, on init, what the book highlights
/// as uncrafted (`highlighted`). Add and remove carry an empty highlight
/// list rather than omitting the section — the field count does not move
/// with the action, so a reader cannot be left guessing.
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeBookBody {
    pub action: RecipeBookAction,
    pub settings: RecipeBookSettings,
    pub changed: Vec<Identifier>,
    /// Present only when [`RecipeBookAction::Init`] resets the book.
    pub highlighted: Option<Vec<Identifier>>,
}

impl Decode for RecipeBookBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let action = RecipeBookAction::decode(input, version)?;
        let settings = RecipeBookSettings::decode(input, version)?;
        let changed = Vec::<Identifier>::decode(input, version)?;
        let highlighted = if action == RecipeBookAction::Init {
            Some(Vec::<Identifier>::decode(input, version)?)
        } else {
            None
        };
        Ok(Self {
            action,
            settings,
            changed,
            highlighted,
        })
    }
}

impl Encode for RecipeBookBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.action.encode(out, version)?;
        self.settings.encode(out, version)?;
        self.changed.encode(out, version)?;
        if self.action == RecipeBookAction::Init {
            return match &self.highlighted {
                Some(list) => list.encode(out, version),
                None => Err(EncodeError::Unsupported {
                    field: "recipe book highlights",
                    why: "an init carries the highlighted recipes and none were given",
                }),
            };
        }
        Ok(())
    }
}

impl TradeOffer {
    /// A trade with no second input and no bookkeeping history, for tests
    /// and for building offers from data.
    pub fn simple(buy_a: TradeItem, sell: Slot) -> Self {
        Self {
            buy_a,
            sell,
            buy_b: None,
            disabled: false,
            uses: 0,
            max_uses: 7,
            villager_xp: 2,
            special_price: 0,
            price_multiplier: 0.05,
            demand: 0,
        }
    }
}
