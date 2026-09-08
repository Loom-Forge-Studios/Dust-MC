//! Play, client to server: what a player does, and how the server learns it.
//!
//! The movement family is four packets that differ only in which fields made
//! it onto the wire — position, position plus rotation, rotation alone, or
//! nothing but the on-ground flag. They are four definitions and not one
//! clever type because the wire has no tag saying which is which: the packet
//! id *is* the tag, and collapsing them would put the id back inside a body,
//! which this crate never does.

use crate::packets::play::advancements::SeenAdvancementsBody;
use crate::packets::play::chat::MessageAcknowledgement;
use crate::packets::play::containers::{ChangedSlot, ClickType, RecipeBookType};
use crate::packets::play::map_item::BookPage;
use crate::packets::play::{Abilities, DifficultyByte, Hand};
use crate::types::{BoundedString, Identifier, ProtocolString, RestOfPacket, Slot, VarInt};
use crate::{packet_group, var_int_enum};

packet_group! {
    state: Play,
    direction: Serverbound,
    versions: ["1.21.1"],

    /// "The teleport you sent me happened." Carries back the id from
    /// the clientbound `player_position`; until this arrives the server treats
    /// the client's position as provisional.
    "minecraft:accept_teleportation" => TeleportConfirm {
        teleport_id: VarInt,
    },

    /// A player chat message, with whatever signing artifacts the client
    /// attached.
    ///
    /// Dust neither verifies nor requires signatures; the acknowledgement
    /// travels so the layout stays exact, and the message content is what
    /// gameplay sees. See [`crate::packets::play::chat`].
    "minecraft:chat" => Chat {
        message: BoundedString<256>,
        timestamp: i64,
        salt: i64,
        signature: Option<crate::packets::play::chat::SignatureBytes>,
        acknowledgement: MessageAcknowledgement,
    },

    /// A command the player typed, unsigned: the whole packet is the text
    /// after the slash, with no leading slash of its own.
    ///
    /// **This one carries no signing artifacts at all, and that is the
    /// version's doing rather than a simplification here.** Before 1.20.5 a
    /// command travelled with a timestamp, a salt and one signature per
    /// signable argument; 1.20.5 split that packet in two and left this half
    /// holding a bare string. A client attaches signatures only when the
    /// command it is running has an argument the server declared as
    /// `minecraft:message` — `/say`, `/msg`, `/me` — and sends
    /// `chat_command_signed` in that case instead. Every other command a
    /// vanilla client runs arrives here.
    ///
    /// So the string is bounded at the protocol default rather than at chat's
    /// 256: the vanilla chat box will not let anyone type more than 256, but
    /// the packet does not say so, and a decoder that refuses what the format
    /// permits disconnects a client instead of answering it. A command too
    /// long to mean anything is a thing to reply to, not to drop a player for.
    "minecraft:chat_command" => ChatCommand {
        command: ProtocolString,
    },

    /// A plugin channel message toward the server.
    "minecraft:custom_payload" => CustomPayload {
        channel: Identifier,
        data: RestOfPacket,
    },

    /// The echo of a keep-alive, same eight bytes back.
    "minecraft:keep_alive" => KeepAlive {
        id: i64,
    },

    /// Movement with position only; the client sends whichever of these
    /// family members covers the change it has to report.
    "minecraft:move_player_pos" => MovePlayerPos {
        x: f64,
        y: f64,
        z: f64,
        on_ground: bool,
    },

    /// Movement plus look direction, the ordinary walking packet.
    "minecraft:move_player_pos_rot" => MovePlayerPosRot {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },

    /// Look direction without movement.
    "minecraft:move_player_rot" => MovePlayerRot {
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },

    /// Nothing but the on-ground flag, for a player who did not move and did
    /// not turn but landed or jumped in place. Sent on a timer even when
    /// nothing happened, which is why its arrival is not evidence of motion.
    "minecraft:move_player_status_only" => MovePlayerStatusOnly {
        on_ground: bool,
    },

    /// The answer to the clientbound ping: the same int, straight back.
    "minecraft:pong" => Pong {
        id: i32,
    },

    /// Flight toggles. The flags byte is the whole packet; the speeds live
    /// only in the clientbound direction, because a client tells the server
    /// what it wants to do and the server tells it how fast that may happen.
    "minecraft:player_abilities" => PlayerAbilities {
        flags: Abilities,
    },

    /// A latency probe from the client side; answered with pong_response.
    "minecraft:ping_request" => PingRequest {
        payload: i64,
    },

    /// Which hotbar slot the player switched to, as a short here where the
    /// clientbound twin uses a byte. Same name, different widths.
    "minecraft:set_carried_item" => SetCarriedItem {
        slot: i16,
    },

    /// The player clicked a slot: what they did, and the container as they
    /// believe it now stands.
    ///
    /// The server replays the click over its own state and pushes back only
    /// the slots that disagree, which is why this packet carries the client's
    /// opinion rather than a request. `state_id` ties it to the last full or
    /// partial sync; see [`ChangedSlot`] and [`ClickType`].
    "minecraft:container_click" => ClickContainer {
        window_id: u8,
        state_id: VarInt,
        slot: i16,
        button: i8,
        mode: ClickType,
        changed_slots: Vec<ChangedSlot>,
        cursor_item: Slot,
    },

    /// The advancement screen opened onto one tab, or closed.
    ///
    /// The action picks whether the tab id follows — see
    /// [`SeenAdvancementsBody`].
    "minecraft:seen_advancements" => SeenAdvancements {
        body: SeenAdvancementsBody,
    },

    /// "Where is this block entity's data?" The NBT comes back through a
    /// tag query response with the same transaction id.
    "minecraft:block_entity_tag_query" => QueryBlockNbt {
        transaction_id: VarInt,
        location: crate::types::Position,
    },

    /// Ask for the world's difficulty. One byte — the lock flag lives in
    /// its own packet, unlike the clientbound twin.
    "minecraft:change_difficulty" => ChangeDifficulty {
        difficulty: DifficultyByte,
    },

    /// "Everything before this message id is seen." Just the offset: the
    /// per-message bitmask lives only inside chat itself.
    "minecraft:chat_ack" => AcknowledgeMessage {
        offset: VarInt,
    },

    /// How many chunks per tick the client wants after the last batch.
    /// The server paces future batches against it.
    "minecraft:chunk_batch_received" => ChunkBatchReceived {
        chunks_per_tick: f32,
    },

    /// Respawn, open statistics, and that is the whole list on this
    /// version.
    "minecraft:client_command" => ClientCommand {
        action: ClientStatusAction,
    },

    /// "Complete me": the tail of whatever command or name is being typed.
    /// The bound is 32500, not the default — one of the protocol's odder
    /// constants, kept exactly because it is load-bearing somewhere.
    "minecraft:command_suggestion" => CommandSuggestionsRequest {
        transaction_id: VarInt,
        text: BoundedString<32_500>,
    },

    /// The client is ready to walk configuration again after play ended.
    "minecraft:configuration_acknowledged" => AcknowledgeConfiguration {},

    /// A screen's button was pressed: enchantment rows, lectern pages,
    /// stonecutter recipes. Both ids are VarInts on this version.
    "minecraft:container_button_click" => ClickContainerButton {
        window_id: VarInt,
        button_id: VarInt,
    },

    /// The player closed a screen. Id 0 closes their own inventory.
    "minecraft:container_close" => CloseContainer {
        window_id: u8,
    },

    /// Toggle one slot's state — crafter slots are the reason it exists.
    "minecraft:container_slot_state_changed" => SlotChangedState {
        slot_id: VarInt,
        screen_handler_id: VarInt,
        new_state: bool,
    },

    /// Answer to a cookie request, payload present when the cookie exists.
    "minecraft:cookie_response" => CookieResponse {
        key: Identifier,
        payload: Option<crate::types::PrefixedBytes<5120>>,
    },

    /// Edit a written book: swap which slot is held, or write pages (and,
    /// when titling, finish the book).
    ///
    /// The case split rides on `title`: present means sign-and-finish.
    /// Pages are bounded at 8192 units each, 200 of them; the title at 128.
    "minecraft:edit_book" => EditBook {
        slot: VarInt,
        pages: Vec<BookPage>,
        title: Option<BoundedString<128>>,
    },

    /// Same query as block entities, pointed at an entity instead.
    "minecraft:entity_tag_query" => QueryEntityNbt {
        transaction_id: VarInt,
        entity_id: VarInt,
    },

    /// The player used or attacked an entity.
    ///
    /// Interact-at carries where on the entity was clicked and which hand;
    /// attack carries neither, which is why the body is one type. Sneaking
    /// rides last so the server knows whether the interaction could have
    /// been something else.
    "minecraft:interact" => InteractEntity {
        entity_id: VarInt,
        kind: InteractionKind,
        sneaking: bool,
    },

    /// Trigger a jigsaw structure's generation from its starting piece.
    /// A creative-mode tool, sent from the jigsaw block's interface.
    "minecraft:jigsaw_generate" => JigsawGenerate {
        location: crate::types::Position,
        levels: VarInt,
        keep_jigsaws: bool,
    },

    /// Lock the difficulty toggle so nobody can change it mid-game.
    "minecraft:lock_difficulty" => LockDifficulty {
        locked: bool,
    },

    /// The vehicle the player drives moved; absolute coordinates, echoed
    /// against the clientbound copy to detect disagreement.
    "minecraft:move_vehicle" => MoveVehicle {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    },

    /// Which paddles are stroking this tick. Boat physics live client-side;
    /// this is what makes them authoritative there.
    "minecraft:paddle_boat" => PaddleBoat {
        left_paddle: bool,
        right_paddle: bool,
    },

    /// Creative pick-block: pull the looked-at block or item into the
    /// hotbar slot named here.
    "minecraft:pick_item" => PickItem {
        slot: VarInt,
    },

    /// Put one recipe into the crafting grid. `craft_all` shifts the click.
    "minecraft:place_recipe" => PlaceRecipe {
        window_id: u8,
        recipe: Identifier,
        craft_all: bool,
    },

    /// Start, continue or finish digging; each carries the sequence number
    /// the server must acknowledge.
    "minecraft:player_action" => PlayerAction {
        status: PlayerActionKind,
        location: crate::types::Position,
        face: u8,
        sequence: VarInt,
    },

    /// An entity-level command about a player: jump, sneaking state, leave
    /// bed. `jump_boost` follows only the jump action — see
    /// [`PlayerCommandBody`].
    "minecraft:player_command" => PlayerCommand {
        body: PlayerCommandBody,
    },

    /// Movement inputs as the player holds them, twice a tick: strafe and
    /// forward as floats, jump and sneak packed in one flags byte.
    "minecraft:player_input" => PlayerInput {
        sideways: f32,
        forward: f32,
        flags: InputFlags,
    },

    /// Recipe book display toggles, one category per packet rather than all
    /// four in one — the settings travel per-tab here and in bulk only
    /// clientbound.
    "minecraft:recipe_book_change_settings" => RecipeBookChangeSettings {
        book_category: RecipeBookType,
        gui_open: bool,
        filtering_craftable: bool,
    },

    /// The player looked at a recipe in the book, so the server can stop
    /// highlighting it.
    "minecraft:recipe_book_seen_recipe" => SeenRecipe {
        recipe: Identifier,
    },

    /// Rename an item at an anvil. The limit is the default string bound;
    /// the anvil's level cost decides whether the rename sticks.
    "minecraft:rename_item" => RenameItem {
        item_name: BoundedString<{ crate::types::DEFAULT_STRING_LIMIT }>,
    },

    /// The play-state copy of client information. The connection already
    /// sent this in configuration; it re-sends when settings change mid-game,
    /// so the definitions match field for field.
    "minecraft:client_information" => ClientInformation {
        locale: BoundedString<16>,
        view_distance: i8,
        chat_mode: crate::types::ChatVisibility,
        chat_colors: bool,
        displayed_skin_parts: u8,
        main_hand: crate::types::MainHand,
        text_filtering_enabled: bool,
        allow_server_listings: bool,
    },

    /// The player answered a resource-pack push. Same layout as its
    /// configuration twin.
    "minecraft:resource_pack" => ResourcePackResponse {
        uuid: crate::types::Uuid,
        result: crate::types::ResourcePackResult,
    },

    /// Which trade the player selected in a merchant screen, by index.
    "minecraft:select_trade" => SelectTrade {
        selected_slot: VarInt,
    },

    /// Set a beacon's effects. Either may be absent — a tier-1 beacon has
    /// no secondary, and both absent resets it.
    "minecraft:set_beacon" => UpdateBeacon {
        primary: Option<VarInt>,
        secondary: Option<VarInt>,
    },

    /// Program a command block: command, mode, and three independent bits.
    "minecraft:set_command_block" => UpdateCommandBlock {
        location: crate::types::Position,
        command: BoundedString<{ crate::types::DEFAULT_STRING_LIMIT }>,
        mode: CommandBlockMode,
        flags: CommandBlockFlags,
    },

    /// Program a command-block minecart. No mode: minecarts are always
    /// impulse blocks on rails.
    "minecraft:set_command_minecart" => UpdateCommandBlockMinecart {
        entity_id: VarInt,
        command: BoundedString<{ crate::types::DEFAULT_STRING_LIMIT }>,
        track_output: bool,
    },

    /// Set a creative-mode slot directly — the one inventory write the
    /// client may make without a container open, and therefore the one
    /// Slot-carrying serverbound packet outside clicks.
    "minecraft:set_creative_mode_slot" => SetCreativeModeSlot {
        slot: i16,
        item: crate::types::Slot,
    },

    /// Program a jigsaw block: pool, target, name, and the priorities that
    /// order multi-piece generation.
    "minecraft:set_jigsaw_block" => UpdateJigsaw {
        location: crate::types::Position,
        name: Identifier,
        target: Identifier,
        pool: Identifier,
        final_state: BoundedString<{ crate::types::DEFAULT_STRING_LIMIT }>,
        joint_type: BoundedString<{ crate::types::DEFAULT_STRING_LIMIT }>,
        selection_priority: VarInt,
        placement_priority: VarInt,
    },

    /// Program a structure block. Everything but the position travels even
    /// when the mode ignores it, which is why the fields stay flat.
    "minecraft:set_structure_block" => UpdateStructureBlock {
        location: crate::types::Position,
        action: StructureBlockAction,
        mode: StructureBlockMode,
        template_name: BoundedString<{ crate::types::DEFAULT_STRING_LIMIT }>,
        offset_x: i8,
        offset_y: i8,
        offset_z: i8,
        size_x: i8,
        size_y: i8,
        size_z: i8,
        mirror: StructureBlockMirror,
        rotation: StructureBlockRotation,
        metadata: BoundedString<{ crate::types::DEFAULT_STRING_LIMIT }>,
        integrity: f32,
        seed: crate::types::VarLong,
        flags: StructureBlockFlags,
    },

    /// Write up to four lines onto one face of a sign.
    "minecraft:sign_update" => UpdateSign {
        location: crate::types::Position,
        is_front_text: bool,
        lines: [BookPage; 4],
    },

    /// Swing an arm. Sent far more often than it means anything, which is
    /// why it costs two bytes.
    "minecraft:swing" => SwingArm {
        hand: Hand,
    },

    /// Spectator teleport: whose eyes to borrow, by UUID rather than entity
    /// id — the target need not be in range yet.
    "minecraft:teleport_to_entity" => SpectateTeleport {
        target: crate::types::Uuid,
    },

    /// Right-clicked a block: which hand, what was hit, and the prediction
    /// sequence to acknowledge.
    ///
    /// The hit is a block position plus face plus cursor offsets plus an
    /// inside-the-block flag; see [`BlockHit`]. No world-border flag on this
    /// version — that arrives later.
    "minecraft:use_item_on" => UseItemOnBlock {
        hand: Hand,
        hit: BlockHit,
        sequence: VarInt,
    },

    /// Right-clicked with a bare item: hand, prediction sequence, and the
    /// head angles at the moment of use.
    "minecraft:use_item" => UseItem {
        hand: Hand,
        sequence: VarInt,
        yaw: f32,
        pitch: f32,
    },
}

// ---------------------------------------------------------------------------
// Serverbound field types the definitions above lean on
// ---------------------------------------------------------------------------

var_int_enum! {
    /// What a client status request asks for. Two values on this version.
    pub enum ClientStatusAction {
        PerformRespawn = 0,
        RequestStats = 1,
    }
}

var_int_enum! {
    /// How an entity interaction happened.
    ///
    /// Attack carries nothing extra; interact names a hand; interact-at also
    /// names where on the entity was clicked — which is why
    /// [`InteractEntity`] holds the extras as options keyed off this.
    pub enum InteractionKind {
        Interact = 0,
        Attack = 1,
        InteractAt = 2,
    }
}

var_int_enum! {
    /// A digging phase: start, cancel, or finish breaking one block.
    pub enum PlayerActionKind {
        StartDigging = 0,
        CancelDigging = 1,
        FinishDigging = 2,
        DropItemStack = 3,
        DropItem = 4,
        ReleaseUseItem = 5,
        SwapHands = 6,
    }
}

/// Everything after the entity id on a player command.
///
/// # The boost is always on the wire
///
/// It reads as though it should be conditional — only the horse-jump actions
/// mean anything by it — and this crate modelled it that way, as an
/// `Option` present for those two actions and refused for the rest. **That was
/// wrong, and a real 1.21.1 server said so.** Vanilla's reader takes three
/// VarInts whatever the action; asked to accept two it disconnects with
/// `Failed to decode packet 'serverbound/minecraft:player_command'`, and asked
/// to accept three it carries on without complaint. So every sneak and every
/// sprint a real client sends carries a zero here, and a decoder that stopped
/// after the action left a byte in the frame and refused the packet.
///
/// The field is a plain `VarInt` now, because that is what the wire holds.
/// Which actions give it a *meaning* is a separate question and
/// [`PlayerCommandBody::meaningful_boost`] is where it is answered — a
/// distinction worth keeping and not worth spending the packet's shape on.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerCommandBody {
    pub entity_id: VarInt,
    pub action_id: PlayerCommandAction,
    /// How high a horse jump is charged. Zero, and meaningless, for every
    /// action that is not one — see the type's note.
    pub jump_boost: VarInt,
}

var_int_enum! {
    /// Which entity-level command about a player this is.
    ///
    /// The horse-jump family carries the boost height; everything else does
    /// not, and the body enforces that pairing rather than trusting it.
    pub enum PlayerCommandAction {
        StartSneaking = 0,
        StopSneaking = 1,
        LeaveBed = 2,
        StartSprinting = 3,
        StopSprinting = 4,
        StartJumpWithHorse = 5,
        StopJumpWithHorse = 6,
        OpenHorseInventory = 7,
        StartFlyingWithElytra = 8,
    }
}

impl PlayerCommandBody {
    /// The boost, where the action is one that has one.
    ///
    /// `None` is "this action does not carry a height", not "the field was
    /// absent" — the field is never absent. A caller that acted on the raw
    /// value for a sneak would be acting on a zero somebody's client wrote
    /// because the format demanded a number, not because they asked for
    /// anything.
    #[must_use]
    pub fn meaningful_boost(&self) -> Option<i32> {
        Self::asks_for_boost(self.action_id).then_some(self.jump_boost.0)
    }

    fn asks_for_boost(action: PlayerCommandAction) -> bool {
        matches!(
            action,
            PlayerCommandAction::StartJumpWithHorse | PlayerCommandAction::StopJumpWithHorse
        )
    }
}

impl crate::types::Decode for PlayerCommandBody {
    fn decode<R: crate::wire::WireRead + ?Sized>(
        input: &mut R,
        version: crate::ProtocolVersion,
    ) -> Result<Self, crate::wire::DecodeError> {
        let entity_id = VarInt::decode(input, version)?;
        let action_id = PlayerCommandAction::decode(input, version)?;
        // Unconditional. See the type's note: vanilla reads three VarInts
        // whatever the action, and this read two until a real server refused
        // the result.
        let jump_boost = VarInt::decode(input, version)?;
        Ok(Self {
            entity_id,
            action_id,
            jump_boost,
        })
    }
}

impl crate::types::Encode for PlayerCommandBody {
    fn encode<W: crate::wire::WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: crate::ProtocolVersion,
    ) -> Result<(), crate::wire::EncodeError> {
        self.entity_id.encode(out, version)?;
        self.action_id.encode(out, version)?;
        self.jump_boost.encode(out, version)?;
        Ok(())
    }
}

/// The movement-input bits: bit 0 jump, bit 1 sneak. Kept raw — they mirror
/// key states, not game semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputFlags(pub u8);

impl InputFlags {
    pub const JUMP: u8 = 0x01;
    pub const SNEAK: u8 = 0x02;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

impl crate::types::Decode for InputFlags {
    fn decode<R: crate::wire::WireRead + ?Sized>(
        input: &mut R,
        _version: crate::ProtocolVersion,
    ) -> Result<Self, crate::wire::DecodeError> {
        input.read_u8().map(Self)
    }
}

impl crate::types::Encode for InputFlags {
    fn encode<W: crate::wire::WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: crate::ProtocolVersion,
    ) -> Result<(), crate::wire::EncodeError> {
        out.write_u8(self.0);
        Ok(())
    }
}

/// Where on a block the crosshair was pointing when the item was used.
///
/// `cursor_*` are offsets into the block (0..=1) from its face, and
/// `inside_block` says whether the player's own head was inside the hit
/// block — which changes what vanilla does with the placement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockHit {
    pub location: crate::types::Position,
    pub face: u8,
    pub cursor_x: f32,
    pub cursor_y: f32,
    pub cursor_z: f32,
    pub inside_block: bool,
}

impl crate::types::Decode for BlockHit {
    fn decode<R: crate::wire::WireRead + ?Sized>(
        input: &mut R,
        version: crate::ProtocolVersion,
    ) -> Result<Self, crate::wire::DecodeError> {
        Ok(Self {
            location: crate::types::Position::decode(input, version)?,
            face: input.read_u8()?,
            cursor_x: input.read_f32()?,
            cursor_y: input.read_f32()?,
            cursor_z: input.read_f32()?,
            inside_block: input.read_bool()?,
        })
    }
}

impl crate::types::Encode for BlockHit {
    fn encode<W: crate::wire::WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: crate::ProtocolVersion,
    ) -> Result<(), crate::wire::EncodeError> {
        self.location.encode(out, _version)?;
        out.write_u8(self.face);
        out.write_f32(self.cursor_x);
        out.write_f32(self.cursor_y);
        out.write_f32(self.cursor_z);
        out.write_bool(self.inside_block);
        Ok(())
    }
}

var_int_enum! {
    /// A command block's mode.
    pub enum CommandBlockMode {
        Sequence = 0,
        Auto = 1,
        Redstone = 2,
    }
}

/// The command block's three independent bits: track output, conditional,
/// always-active. Kept raw because each maps to a GUI checkbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommandBlockFlags(pub u8);

impl CommandBlockFlags {
    pub const TRACK_OUTPUT: u8 = 0x01;
    pub const CONDITIONAL: u8 = 0x02;
    pub const ALWAYS_ACTIVE: u8 = 0x04;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

impl crate::types::Decode for CommandBlockFlags {
    fn decode<R: crate::wire::WireRead + ?Sized>(
        input: &mut R,
        _version: crate::ProtocolVersion,
    ) -> Result<Self, crate::wire::DecodeError> {
        input.read_u8().map(Self)
    }
}

impl crate::types::Encode for CommandBlockFlags {
    fn encode<W: crate::wire::WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: crate::ProtocolVersion,
    ) -> Result<(), crate::wire::EncodeError> {
        out.write_u8(self.0);
        Ok(())
    }
}

var_int_enum! {
    /// What a structure block should do when triggered.
    pub enum StructureBlockAction {
        Update = 0,
        Save = 1,
        Load = 2,
        Corner = 3,
        Detect = 4,
    }
}

var_int_enum! {
    /// A structure block's editing mode.
    pub enum StructureBlockMode {
        Save = 0,
        Load = 1,
        Corner = 2,
        Data = 3,
    }
}

var_int_enum! {
    /// How a loaded structure mirrors itself.
    pub enum StructureBlockMirror {
        None = 0,
        LeftRight = 1,
        FrontBack = 2,
    }
}

var_int_enum! {
    /// How a loaded structure rotates.
    pub enum StructureBlockRotation {
        None = 0,
        Clockwise90 = 1,
        Clockwise180 = 2,
        CounterClockwise90 = 3,
    }
}

/// The structure block's show bits: ignore entities, show air, show
/// bounding box. Raw, like every other checkbox byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StructureBlockFlags(pub u8);

impl StructureBlockFlags {
    pub const IGNORE_ENTITIES: u8 = 0x01;
    pub const SHOW_AIR: u8 = 0x02;
    pub const SHOW_BOUNDING_BOX: u8 = 0x04;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

impl crate::types::Decode for StructureBlockFlags {
    fn decode<R: crate::wire::WireRead + ?Sized>(
        input: &mut R,
        _version: crate::ProtocolVersion,
    ) -> Result<Self, crate::wire::DecodeError> {
        input.read_u8().map(Self)
    }
}

impl crate::types::Encode for StructureBlockFlags {
    fn encode<W: crate::wire::WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: crate::ProtocolVersion,
    ) -> Result<(), crate::wire::EncodeError> {
        out.write_u8(self.0);
        Ok(())
    }
}
