//! Play, server to client: the packets that keep a world in front of a
//! player.
//!
//! Field layouts here were written against the protocol documentation for 767
//! and are checked three ways: round trips over every packet, property tests
//! over generated values, and the mutation loop that insists a hostile body
//! can fail but never panic. What no offline test can check is whether
//! Mojang's client agrees with all of it; that is what pointing a real
//! 1.21.1 client at this crate is for.

use crate::nbt::Nbt;
use crate::packet_group;
use crate::packets::play::advancements::AdvancementsBody;
use crate::packets::play::boss_bar::BossEventBody;
use crate::packets::play::chunk::{BlockEntity, ChunkData, LightData};
use crate::packets::play::commands::{CommandsBody, SuggestionMatch};
use crate::packets::play::containers::{EquipmentEntries, MerchantOffersBody, Recipe};
use crate::packets::play::map_item::MapDataBody;
use crate::packets::play::metadata::MetadataEntries;
use crate::packets::play::particle::ParticleValue;
use crate::packets::play::player_info::PlayerInfoBody;
use crate::packets::play::sound::{SoundCategory, SoundId, StopSoundBody};
use crate::packets::play::{chat as chat_fields, TeleportFlags};
use crate::packets::play::{
    Abilities, BlockChangeEntry, ChunkSectionPosition, DeathLocation, DifficultyByte,
};
use crate::packets::play::{
    Anchor, DamageSourcePosition, EffectFlags, EntityDelta, EntityLinkKind, EntityVelocity,
    ExplosionInteraction, ExplosionRecord, GameModeByte, Hand, LookAtTarget, OffsetEntityId,
    PreviousGameMode, RespawnFlags,
};
use crate::text::Component;
use crate::types::{Angle, BoundedString, Identifier, Position, RestOfPacket, Uuid, VarInt};

packet_group! {
    state: Play,
    direction: Clientbound,
    versions: ["1.21.1"],

    /// A player or object becomes visible: identity, kind, where and how it
    /// moves.
    ///
    /// `data` means something different per entity kind — a launcher's item
    /// for arrows, nothing for most — and is carried raw because resolving it
    /// needs the entity table, not the protocol.
    "minecraft:add_entity" => AddEntity {
        entity_id: VarInt,
        uuid: Uuid,
        kind: VarInt,
        x: f64,
        y: f64,
        z: f64,
        pitch: Angle,
        yaw: Angle,
        head_yaw: Angle,
        data: VarInt,
        velocity: EntityVelocity,
    },

    /// One block changed. The chunk it sits in must already be loaded, which
    /// is why this cannot replace [`SectionBlocksUpdate`] for world edits:
    /// two blocks in one tick go out together or the client animates them
    /// apart.
    "minecraft:block_update" => BlockUpdate {
        location: Position,
        block_id: VarInt,
    },

    /// Kicks the player to the server list with a reason. **NBT** component,
    /// same as every play-state text field.
    "minecraft:disconnect" => Disconnect {
        reason: Component,
    },

    /// The whole chunk column plus its light, on first sight.
    ///
    /// The sections themselves are bytes behind a trait; see
    /// [`crate::packets::play::chunk::Section`] for where dust-world plugs
    /// in. Heightmaps ride along as NBT this layer delimits without opening.
    "minecraft:level_chunk_with_light" => LevelChunkWithLight {
        chunk_x: i32,
        chunk_z: i32,
        heightmaps: Nbt,
        data: ChunkData,
        block_entities: Vec<BlockEntity>,
        light: LightData,
    },

    /// Liveness: eight opaque bytes the client must hand straight back.
    "minecraft:keep_alive" => KeepAlive {
        id: i64,
    },

    /// You have joined: who you are, which world, how much of it to load.
    ///
    /// The registries themselves were synced during configuration; this
    /// packet carries only the *names* of the dimensions and the id of the
    /// one being spawned into, which is the shape 1.20.5+ settled on.
    /// `dimension_type` is an id into the dimension-type registry — a VarInt
    /// here where older versions spelled an identifier, and exactly the kind
    /// of change the version parameter exists for.
    "minecraft:login" => Login {
        entity_id: i32,
        hardcore: bool,
        dimensions: Vec<Identifier>,
        max_players: VarInt,
        view_distance: VarInt,
        simulation_distance: VarInt,
        reduced_debug_info: bool,
        respawn_screen: bool,
        limited_crafting: bool,
        dimension_type: VarInt,
        dimension_name: Identifier,
        hashed_seed: i64,
        game_mode: GameModeByte,
        previous_game_mode: PreviousGameMode,
        debug: bool,
        flat: bool,
        death_location: Option<DeathLocation>,
        portal_cooldown: VarInt,
        secure_chat: bool,
    },

    /// Entities moved and turned at once. Deltas are 1/4096-block shorts so
    /// ordinary motion costs eleven bytes instead of twenty-seven.
    "minecraft:move_entity_pos_rot" => MoveEntityPosRot {
        entity_id: VarInt,
        delta: EntityDelta,
        yaw: Angle,
        pitch: Angle,
        on_ground: bool,
    },

    /// An entity moved without turning.
    "minecraft:move_entity_pos" => MoveEntityPos {
        entity_id: VarInt,
        delta: EntityDelta,
        on_ground: bool,
    },

    /// An entity turned without moving.
    "minecraft:move_entity_rot" => MoveEntityRot {
        entity_id: VarInt,
        yaw: Angle,
        pitch: Angle,
        on_ground: bool,
    },

    /// Another player's chat message, with its signing envelope.
    ///
    /// Dust relays messages unsigned; the fields still decode fully because a
    /// decoder that skipped them could not find the end of the packet. See
    /// [`crate::packets::play::chat`] for the seam.
    "minecraft:player_chat" => PlayerChatMessage {
        sender: Uuid,
        index: VarInt,
        signature: Option<chat_fields::SignatureBytes>,
        message: BoundedString<256>,
        timestamp: i64,
        salt: i64,
        previous_messages: Vec<chat_fields::AcknowledgedMessage>,
        unsigned_content: Option<Component>,
        filter: chat_fields::ChatFilter,
        chat_type: VarInt,
        network_name: Component,
        network_target_name: Option<Component>,
    },

    /// Grants or revokes flight, invulnerability and their friends, with the
    /// speeds the client should feel them at.
    "minecraft:player_abilities" => PlayerAbilities {
        flags: Abilities,
        flying_speed: f32,
        fov_modifier: f32,
    },

    /// Where the player now is, and whether any of that is relative. The
    /// teleport id exists to be echoed back by the serverbound
    /// `accept_teleportation`, which is how both sides agree the move
    /// happened.
    "minecraft:player_position" => PlayerPosition {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        flags: TeleportFlags,
        teleport_id: VarInt,
    },

    /// Which players exist and what the tab list says about them.
    ///
    /// One bitmask selects up to six layouts per entry, which is why the body
    /// after the id is one type; see [`PlayerInfoBody`]'s module for how to
    /// read that.
    "minecraft:player_info_update" => PlayerInfoUpdate {
        body: PlayerInfoBody,
    },

    /// Players gone from the tab list. Removals carry no actions: the uuids
    /// are the whole message.
    "minecraft:player_info_remove" => PlayerInfoRemove {
        uuids: Vec<Uuid>,
    },

    /// A plugin channel message toward the client.
    "minecraft:custom_payload" => CustomPayload {
        channel: Identifier,
        data: RestOfPacket,
    },

    /// Entities ceased to exist. A list rather than one-per-packet because
    /// deaths and chunk unloads take many at once.
    "minecraft:remove_entities" => RemoveEntities {
        entity_ids: Vec<VarInt>,
    },

    /// The head's yaw, separately from the body's. Living entities need both;
    /// the head leads turns and the body follows, which is why this is not
    /// just another angle on the movement packets.
    "minecraft:rotate_head" => RotateHead {
        entity_id: VarInt,
        head_yaw: Angle,
    },

    /// Several blocks changed in one section, in one tick.
    "minecraft:section_blocks_update" => SectionBlocksUpdate {
        section: ChunkSectionPosition,
        entries: Vec<BlockChangeEntry>,
    },

    /// Which hotbar slot is held. A byte here and a short in the serverbound
    /// direction — same name, different widths, four packets apart.
    "minecraft:set_carried_item" => SetCarriedItem {
        slot: u8,
    },

    /// A named slot on an entity changed value: pose, health, fire, name.
    ///
    /// The entries run until a terminator byte, so they are a type of their
    /// own; unknown serializers refuse rather than guess. See
    /// [`crate::packets::play::metadata`].
    "minecraft:set_entity_data" => SetEntityData {
        entity_id: VarInt,
        entries: MetadataEntries,
    },

    /// Server-to-player text that is nobody's speech: motds, game messages,
    /// action-bar lines. `overlay` picks the action bar over the chat log.
    "minecraft:system_chat" => SystemChat {
        content: Component,
        overlay: bool,
    },

    /// Round-trip latency probe: an int the client echoes through pong.
    "minecraft:ping" => Ping {
        id: i32,
    },

    /// The answer to the client's own ping request, payload untouched.
    "minecraft:pong_response" => PongResponse {
        payload: i64,
    },

    /// One boss bar appears, changes or disappears.
    ///
    /// The action picks the layout, which is why the body is one type; see
    /// [`BossEventBody`]'s module for how to read it.
    "minecraft:boss_event" => BossEvent {
        body: BossEventBody,
    },

    /// Every command the server offers, as a brigadier node graph.
    ///
    /// Children and redirects are indices into the packet's own node array,
    /// and may point **either way** through it: vanilla writes the root first
    /// and every one of its children after it, so forward references are the
    /// normal case and a reader has to resolve in a second pass. See
    /// [`CommandsBody`]'s module for the node format and the parser table.
    "minecraft:commands" => Commands {
        body: CommandsBody,
    },

    /// The advancement tree, its display cards and each player's progress.
    ///
    /// There is no incremental form: every sync carries all four sections,
    /// and `reset` decides whether they replace or patch what the client had.
    /// See [`AdvancementsBody`].
    "minecraft:update_advancements" => UpdateAdvancements {
        body: AdvancementsBody,
    },

    /// Which tab of the advancement screen the client should show. `None`
    /// means the screen was closed on the server side.
    "minecraft:select_advancements_tab" => SelectAdvancementsTab {
        tab: Option<Identifier>,
    },

    /// A mob's visible gear changed. Entries continue while a high bit says
    /// so — see [`EquipmentEntries`].
    "minecraft:set_equipment" => SetEquipment {
        entity_id: VarInt,
        entries: EquipmentEntries,
    },

    /// A burst of particles at a point in the world.
    ///
    /// `count` copies are scattered through the offsets; a count of zero
    /// places exactly one particle at the position itself, which is how a
    /// single dramatic effect is spelled.
    "minecraft:level_particles" => LevelParticles {
        long_distance: bool,
        x: f64,
        y: f64,
        z: f64,
        offset_x: f32,
        offset_y: f32,
        offset_z: f32,
        max_speed: f32,
        count: i32,
        particle: ParticleValue,
    },

    /// A map item's icons and colour patch, in one update.
    ///
    /// Both halves are optional and value-dependent, so the tail is one type;
    /// see [`MapDataBody`]'s module.
    "minecraft:map_item_data" => MapItemData {
        map_id: VarInt,
        data: MapDataBody,
    },

    /// A sound plays from a point in the world.
    ///
    /// **The three position fields are eighths of a block, not blocks.**
    /// Vanilla writes `(int)(x * 8.0)` and the client reads it back as
    /// `x / 8.0`; a caller that put a block coordinate here would place the
    /// sound at an eighth of the distance from the origin, which is audible
    /// only as "that sounded far away" and never as an error.
    /// [`eighths`](super::sound::eighths) is the conversion. See [`SoundId`]
    /// for why the sound itself is a tagged union.
    "minecraft:sound" => Sound {
        sound: SoundId,
        category: SoundCategory,
        position_x: i32,
        position_y: i32,
        position_z: i32,
        volume: f32,
        pitch: f32,
        seed: i64,
    },

    /// A sound plays from an entity, so the client can follow it around.
    "minecraft:sound_entity" => SoundEntity {
        sound: SoundId,
        category: SoundCategory,
        entity_id: VarInt,
        volume: f32,
        pitch: f32,
        seed: i64,
    },

    /// Stop one playing sound, a whole category, or everything.
    ///
    /// One flags byte selects among four layouts; see [`StopSoundBody`].
    "minecraft:stop_sound" => StopSound {
        body: StopSoundBody,
    },

    /// Every recipe the client may craft, with each recipe's own layout.
    ///
    /// The type id leads the data and picks the struct that follows; see
    /// [`Recipe`]. What the client *may craft* is the separate recipe book
    /// packet, which carries ids rather than layouts.
    "minecraft:update_recipes" => UpdateRecipes {
        recipes: Vec<Recipe>,
    },

    /// The answer to the client's tab-completion request: what to offer.
    ///
    /// `start`/`length` name the span of the input being replaced. Each
    /// match carries its own optional tooltip; see [`SuggestionMatch`].
    "minecraft:command_suggestions" => CommandSuggestions {
        id: VarInt,
        start: VarInt,
        length: VarInt,
        matches: Vec<SuggestionMatch>,
    },

    /// Marks the end of a bundle: every packet since the last delimiter is
    /// one atomic group the client must apply together.
    ///
    /// The body is empty on purpose — the delimiter *is* the message.
    "minecraft:bundle_delimiter" => BundleDelimiter {},

    /// An entity swung an arm, took damage or left a nest egg. `animation`
    /// is the protocol's own byte table; the meanings belong to the entity
    /// that sent it, not to this crate.
    "minecraft:animate" => Animate {
        entity_id: VarInt,
        animation: u8,
    },

    /// The player's statistic counters, in full. There is no delta form:
    /// statistics change rarely and the whole map is cheaper than tracking
    /// what the client still believes.
    "minecraft:award_stats" => AwardStats {
        statistics: Vec<crate::packets::play::containers::StatisticEntry>,
    },

    /// The server acknowledges a client interaction by sequence number,
    /// closing out the block-change prediction the client made for it.
    "minecraft:block_changed_ack" => BlockChangedAck {
        sequence: VarInt,
    },

    /// A block-breaking animation progresses (or clears, at stage outside
    /// 0..=9). One entity per stage; several miners on one block each get
    /// their own crack.
    "minecraft:block_destruction" => BlockDestruction {
        entity_id: VarInt,
        location: Position,
        destroy_stage: i8,
    },

    /// A block entity's data changed — chest contents, sign text, beacon
    /// effects. The NBT is delimited here and interpreted elsewhere.
    "minecraft:block_entity_data" => BlockEntityData {
        location: Position,
        kind: VarInt,
        data: Nbt,
    },

    /// A block does something: piston extends, note block plays, chest
    /// lid opens. Two free-form bytes plus the block's type id; the meaning
    /// of the bytes belongs to the block, which is why they stay raw.
    "minecraft:block_event" => BlockEvent {
        location: Position,
        action_id: u8,
        action_parameter: u8,
        block_type: VarInt,
    },

    /// Sets the world difficulty and whether the player may change it.
    ///
    /// Travels as a bare byte, never as a VarInt — see [`DifficultyByte`].
    "minecraft:change_difficulty" => ChangeDifficulty {
        difficulty: DifficultyByte,
        locked: bool,
    },

    /// Ends a chunk batch, telling the client how many chunks it contained;
    /// the client uses its own timing over the batch to pace the next one.
    "minecraft:chunk_batch_finished" => ChunkBatchFinished {
        batch_size: VarInt,
    },

    /// Marks where a chunk batch begins. Nothing but a boundary marker.
    "minecraft:chunk_batch_start" => ChunkBatchStart {},

    /// Biome sections for chunks the client already holds.
    ///
    /// The payload per chunk is a blob like a chunk column's — see
    /// [`crate::packets::play::chunk::ChunkBiomesEntry`] — with z before x,
    /// because the pair reads as one big-endian long.
    "minecraft:chunks_biomes" => ChunksBiomes {
        chunks: Vec<crate::packets::play::chunk::ChunkBiomesEntry>,
    },

    /// Clears the title and subtitle, optionally resetting their timings
    /// too.
    "minecraft:clear_titles" => ClearTitles {
        reset: bool,
    },

    /// Close a container window. Id 0 is the player's own inventory.
    "minecraft:container_close" => ContainerClose {
        window_id: u8,
    },

    /// Every slot of a window at once, plus whatever is on the cursor.
    ///
    /// The expensive one, and the reason its neighbour exists: a container of
    /// forty-six slots costs forty-seven stacks on the wire whether one slot
    /// changed or all of them. Sent on join and after a click the client and
    /// the server disagree about too widely to reconcile slot by slot;
    /// [`ContainerSetSlot`] is for everything else.
    ///
    /// `state_id` is the sequence number a later
    /// [`ClickContainer`](crate::packets::play::serverbound::ClickContainer)
    /// quotes back. It is the server's, not the client's: a click carrying a
    /// stale one is a click made against a window that has since moved.
    "minecraft:container_set_content" => ContainerSetContent {
        window_id: u8,
        state_id: VarInt,
        slots: Vec<crate::types::Slot>,
        carried_item: crate::types::Slot,
    },

    /// One slot of one window.
    ///
    /// The window id is **signed** here where every other container packet on
    /// this version has it unsigned, and the negative values are the whole
    /// reason: `-1` addresses the cursor rather than a slot, and `-2` writes
    /// the player's own inventory without the client checking `state_id`
    /// against its own. Widening it to `u8` would make both unreachable and
    /// the mistake would look like a working packet.
    "minecraft:container_set_slot" => ContainerSetSlot {
        window_id: i8,
        state_id: VarInt,
        slot: i16,
        item: crate::types::Slot,
    },

    /// A container's progress bars moved — furnace fuel, enchantment seed,
    /// stonecutter selection. Which property means what depends on the open
    /// screen, so both halves travel raw.
    "minecraft:container_set_data" => ContainerSetData {
        window_id: u8,
        property: i16,
        value: i16,
    },

    /// Asks whether the client still holds a cookie set earlier.
    "minecraft:cookie_request" => CookieRequest {
        key: Identifier,
    },

    /// Puts an item type on cooldown — ender pearls, shields. Zero ticks
    /// clears it.
    "minecraft:cooldown" => Cooldown {
        item_id: VarInt,
        cooldown_ticks: VarInt,
    },

    /// Hints for chat auto-completion: names worth offering while typing.
    ///
    /// Add and remove patch the set; Set replaces it wholesale.
    "minecraft:custom_chat_completions" => CustomChatCompletions {
        action: chat_fields::ChatCompletionsAction,
        entries: Vec<crate::types::ProtocolString>,
    },

    /// An entity was hurt: by what, through what chain, and from where.
    ///
    /// The two causes are offset ids — **id + 1**, zero meaning none; see
    /// [`OffsetEntityId`]. The position rides along only for damages with no
    /// entity behind them, like `/damage` with explicit coordinates.
    "minecraft:damage_event" => DamageEvent {
        entity_id: VarInt,
        source_type: VarInt,
        source_cause: OffsetEntityId,
        source_direct: OffsetEntityId,
        source_position: Option<DamageSourcePosition>,
    },

    /// Chat from nobody in particular — console `/say`, command feedback.
    ///
    /// No signing envelope exists to lay out: the message formats itself
    /// through the chat-type registry exactly as a player message would,
    /// minus everything cryptographic.
    "minecraft:disguised_chat" => DisguisedChat {
        message: Component,
        chat_type: VarInt,
        sender_name: Component,
        target_name: Option<Component>,
    },

    /// An experience orb appeared. The orb is an entity like any other but
    /// arrives early and often, so it gets a cheaper packet than add_entity.
    "minecraft:add_experience_orb" => ExperienceOrbSpawn {
        entity_id: VarInt,
        x: f64,
        y: f64,
        z: f64,
        experience: i16,
    },

    /// An entity did something status-shaped: wolf tamed, villager angry,
    /// living flame extinguished. The byte's meaning varies per entity type,
    /// so it stays a byte.
    "minecraft:entity_event" => EntityEvent {
        entity_id: i32,
        status: u8,
    },

    /// An explosion went off: blocks removed, the player pushed, particles
    /// and sound chosen by the server rather than guessed per strength.
    ///
    /// The records are offsets from the centre — three signed bytes each,
    /// see [`ExplosionRecord`] — and the two particles carry their own
    /// option blocks via [`ParticleValue`].
    "minecraft:explode" => Explode {
        x: f64,
        y: f64,
        z: f64,
        radius: f32,
        records: Vec<ExplosionRecord>,
        player_motion_x: f32,
        player_motion_y: f32,
        player_motion_z: f32,
        block_interaction: ExplosionInteraction,
        small_particle: ParticleValue,
        large_particle: ParticleValue,
        sound: SoundId,
    },

    /// A chunk column left the view distance. Z first, then x — the pair
    /// reads as one long, high half z.
    "minecraft:forget_level_chunk" => ForgetLevelChunk {
        chunk_z: i32,
        chunk_x: i32,
    },

    /// Weather shifts, gamemode changes, rain thickens. The event table is
    /// the protocol's own; the value's meaning follows the event.
    "minecraft:game_event" => GameEvent {
        event: u8,
        value: f32,
    },

    /// Opens the horse's inventory — the one screen with its own packet,
    /// because the client needs the horse's id to fill the saddle slots.
    "minecraft:horse_screen_open" => HorseScreenOpen {
        window_id: u8,
        slot_count: VarInt,
        entity_id: i32,
    },

    /// Plays the damage-tilt bobbing: which way the hit came from.
    "minecraft:hurt_animation" => HurtAnimation {
        entity_id: VarInt,
        yaw: f32,
    },

    /// The world border's complete state in one packet, sent on join and
    /// after configuration. Speed is real-time milliseconds, not ticks.
    "minecraft:initialize_border" => InitializeBorder {
        center_x: f64,
        center_z: f64,
        old_diameter: f64,
        new_diameter: f64,
        lerp_speed: crate::types::VarLong,
        portal_boundary: VarInt,
        warning_blocks: VarInt,
        warning_time: VarInt,
    },

    /// A world-level effect: record starts, gate breaks, dragon breathes.
    /// The data field's meaning follows the event; `global` opts out of
    /// distance falloff for the few effects that must be heard everywhere.
    "minecraft:level_event" => LevelEvent {
        event: i32,
        position: Position,
        data: i32,
        global: bool,
    },

    /// Light levels for one chunk, without the chunk itself. Same layout as
    /// the light half of a chunk column; see [`LightData`].
    "minecraft:light_update" => LightUpdate {
        chunk_x: VarInt,
        chunk_z: VarInt,
        light: LightData,
    },

    /// A trader presents its wares. See [`MerchantOffersBody`] for the
    /// trade layout; note the window id is a VarInt here where every other
    /// container packet spells a byte.
    "minecraft:merchant_offers" => MerchantOffers {
        window_id: VarInt,
        body: MerchantOffersBody,
    },

    /// A vehicle the player is riding moved: absolute position and facing.
    /// The client echoes it back serverbound when it drives.
    "minecraft:move_vehicle" => MoveVehicle {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    },

    /// Open a written book the player is holding.
    "minecraft:open_book" => OpenBook {
        hand: Hand,
    },

    /// Open a container screen: which menu layout, titled how.
    ///
    /// `menu_kind` ids into the `minecraft:menu` registry; resolving the id
    /// to slot arrangements is the screen layer's business, not the wire's.
    "minecraft:open_screen" => OpenScreen {
        window_id: u8,
        menu_kind: VarInt,
        title: Component,
    },

    /// Point the sign editor at one face of one sign.
    "minecraft:open_sign_editor" => OpenSignEditor {
        location: Position,
        is_front_text: bool,
    },

    /// Show one recipe as a ghost in the crafting grid until the player
    /// fills it or moves.
    "minecraft:place_ghost_recipe" => PlaceGhostRecipe {
        window_id: u8,
        recipe: Identifier,
    },

    /// Combat ended: how long since the last attack. Unused by the vanilla
    /// client beyond bookkeeping, and shaped accordingly.
    "minecraft:player_combat_end" => PlayerCombatEnd {
        duration_in_ticks: VarInt,
    },

    /// The player entered combat. No fields; the state is the message.
    "minecraft:player_combat_enter" => PlayerCombatEnter {},

    /// The player died, and here is why.
    "minecraft:player_combat_kill" => PlayerCombatKill {
        player_id: VarInt,
        killer_id: VarInt,
        message: Component,
    },

    /// Turn the player's view toward a point or an entity.
    ///
    /// When an entity is targeted, its own anchor — feet or eyes, chosen
    /// independently of the player's — decides the exact aim point; see
    /// [`LookAtTarget`].
    "minecraft:player_look_at" => PlayerLookAt {
        anchor: Anchor,
        x: f64,
        y: f64,
        z: f64,
        target: Option<LookAtTarget>,
    },

    /// Which recipes the client may craft, and how the book displays.
    ///
    /// Ids, not layouts — the layouts came earlier in update_recipes; see
    /// [`RecipeBookBody`].
    "minecraft:recipe" => RecipeBookUnlock {
        body: crate::packets::play::containers::RecipeBookBody,
    },

    /// One mob effect ended on one entity. Removing an effect the client
    /// never saw is allowed to be silent.
    "minecraft:remove_mob_effect" => RemoveMobEffect {
        entity_id: VarInt,
        effect: VarInt,
    },

    /// Remove a score, from one objective or from all of them at once.
    "minecraft:reset_score" => ResetScore {
        entity_name: crate::types::ProtocolString,
        objective: Option<crate::types::ProtocolString>,
    },

    /// Pop one pushed resource pack, or all of them when the uuid is
    /// absent. Same layout as its configuration twin.
    "minecraft:resource_pack_pop" => ResourcePackPop {
        uuid: Option<Uuid>,
    },

    /// Push a resource pack, url first, hash for verification, prompt if
    /// the client wants to ask. Same layout as its configuration twin.
    "minecraft:resource_pack_push" => ResourcePackPush {
        uuid: Uuid,
        url: BoundedString<256>,
        hash: BoundedString<40>,
        forced: bool,
        prompt_message: Option<crate::nbt::TextComponent>,
    },

    /// Respawned, possibly in another dimension.
    ///
    /// The spawn info mirrors the join packet's second half; the flags keep
    /// entity metadata and entities across same-world respawns so the end
    /// credits do not rebuild the world.
    "minecraft:respawn" => Respawn {
        dimension_type: VarInt,
        dimension_name: Identifier,
        hashed_seed: i64,
        game_mode: GameModeByte,
        previous_game_mode: PreviousGameMode,
        debug: bool,
        flat: bool,
        death_location: Option<DeathLocation>,
        portal_cooldown: VarInt,
        flags: RespawnFlags,
    },

    /// The server list ping's play-state cousin: description plus optional
    /// icon, shown before join.
    "minecraft:server_data" => ServerData {
        motd: Component,
        icon: Option<crate::types::PrefixedBytes<1_048_576>>,
    },

    /// One line above the hotbar. Equivalent to system chat with overlay
    /// set, minus chat-blocking semantics.
    "minecraft:set_action_bar_text" => SetActionBarText {
        text: Component,
    },

    /// Move the world border's centre.
    "minecraft:set_border_center" => SetBorderCenter {
        center_x: f64,
        center_z: f64,
    },

    /// Resize the border gradually: from, to, and how long in milliseconds.
    "minecraft:set_border_lerp_size" => SetBorderLerpSize {
        old_diameter: f64,
        new_diameter: f64,
        lerp_speed: crate::types::VarLong,
    },

    /// Resize the border immediately.
    "minecraft:set_border_size" => SetBorderSize {
        diameter: f64,
    },

    /// How long the red border warning shows before the wall arrives.
    "minecraft:set_border_warning_delay" => SetBorderWarningDelay {
        warning_time: VarInt,
    },

    /// How close to the wall the warning begins, in blocks.
    "minecraft:set_border_warning_distance" => SetBorderWarningDistance {
        warning_blocks: VarInt,
    },

    /// Spectate from this entity's viewpoint instead of the player's.
    "minecraft:set_camera" => SetCamera {
        camera_entity_id: VarInt,
    },

    /// Which chunk sits at the centre of the client's view grid. Z first,
    /// then x, matching the unload packet.
    "minecraft:set_chunk_cache_center" => SetCenterChunk {
        chunk_z: VarInt,
        chunk_x: VarInt,
    },

    /// How far the client should render, when the server decides it rather
    /// than the client's own setting.
    "minecraft:set_chunk_cache_radius" => SetChunkCacheRadius {
        distance: VarInt,
    },

    /// Where compasses point and players respawn: a position and the angle
    /// to face there.
    "minecraft:set_default_spawn_position" => SetDefaultSpawnPosition {
        location: Position,
        angle: f32,
    },

    /// Which objective shows in a sidebar slot, and none anymore.
    "minecraft:set_display_objective" => DisplayObjective {
        slot: crate::packets::play::scoreboard::ScoreboardSlot,
        score_name: crate::types::ProtocolString,
        display_text: Option<Component>,
        render_type: Option<crate::packets::play::scoreboard::ObjectiveRenderType>,
    },

    /// Attach or detach one entity from another: leashes, riding.
    ///
    /// Both ids are plain ints here — the leash half of this packet predates
    /// most of the protocol's VarInt discipline and kept its widths.
    "minecraft:set_entity_link" => LinkEntities {
        attached_to: i32,
        connecting_entity: i32,
        link_kind: EntityLinkKind,
    },

    /// An entity's velocity was set abruptly — knockback, explosion, launch.
    /// Units are 1/8000 blocks per tick; see [`EntityVelocity`].
    "minecraft:set_entity_motion" => SetEntityMotion {
        entity_id: VarInt,
        velocity: EntityVelocity,
    },

    /// The experience bar's shape: fill, level, points.
    ///
    /// **The order is bar, then level, then total**, and it was written here
    /// as bar, total, level until somebody sent one. The two are both `VarInt`
    /// so nothing about the wire complains; a client just draws the wrong
    /// number. Read out of the operator's own jar rather than out of a
    /// reference: `ClientboundSetExperiencePacket`'s write puts
    /// `experienceProgress`, then `experienceLevel`, then `totalExperience`,
    /// which `javap -p -c` on the inner server jar shows as fields `b`, `d`,
    /// `c` in that order. `minecraft-data` agrees; this repository did not.
    "minecraft:set_experience" => SetExperience {
        experience_bar: f32,
        level: VarInt,
        total_experience: VarInt,
    },

    /// Health, hunger, saturation. At health zero the death screen waits
    /// for combat_kill's reason.
    "minecraft:set_health" => SetHealth {
        health: f32,
        food: VarInt,
        food_saturation: f32,
    },

    /// Create, remove or retitle a scoreboard objective.
    "minecraft:set_objective" => UpdateObjectives {
        objective_name: crate::types::ProtocolString,
        body: crate::packets::play::scoreboard::UpdateObjectivesBody,
    },

    /// Who rides whom. One vehicle, many passengers.
    "minecraft:set_passengers" => SetPassengers {
        vehicle_id: VarInt,
        passengers: Vec<VarInt>,
    },

    /// Create, dissolve or edit a team — colours, friendly fire, members.
    "minecraft:set_player_team" => UpdateTeams {
        team_name: crate::types::ProtocolString,
        body: crate::packets::play::scoreboard::TeamBody,
    },

    /// Set one score under one objective.
    "minecraft:set_score" => UpdateScore {
        entity_name: crate::types::ProtocolString,
        body: crate::packets::play::scoreboard::UpdateScoreBody,
    },

    /// How far simulation runs — weather, crops, mob AI — as opposed to
    /// rendering.
    "minecraft:set_simulation_distance" => SetSimulationDistance {
        distance: VarInt,
    },

    /// The smaller line under a title.
    "minecraft:set_subtitle_text" => SetSubtitleText {
        text: Component,
    },

    /// World clock: total age, and time-of-day whose sign says whether the
    /// sun moves at all.
    "minecraft:set_time" => SetTime {
        world_age: i64,
        time_of_day: i64,
    },

    /// The big line in the middle of the screen.
    "minecraft:set_title_text" => SetTitleText {
        text: Component,
    },

    /// Title fade-in, hold and fade-out, in ticks. Ints here, not VarInts —
    /// three of the protocol's few remaining fixed-width counters.
    "minecraft:set_titles_animation" => SetTitlesAnimation {
        fade_in: i32,
        stay: i32,
        fade_out: i32,
    },

    /// Play-state ends; back to configuration for a reload or transfer.
    /// The client answers with its acknowledgement, and the connection
    /// walks configuration again.
    "minecraft:start_configuration" => StartConfiguration {},

    /// The tab list's header and footer, either of which may be empty.
    "minecraft:tab_list" => SetTabListHeaderFooter {
        header: Component,
        footer: Component,
    },

    /// Answers a serverbound block-entity tag query: the NBT, or nothing.
    "minecraft:tag_query" => TagQueryResponse {
        transaction_id: VarInt,
        nbt: Nbt,
    },

    /// An item entity merged into someone's inventory: what flew to whom,
    /// and how many.
    "minecraft:take_item_entity" => TakeItemEntity {
        collected_entity_id: VarInt,
        collector_entity_id: VarInt,
        pickup_item_count: VarInt,
    },

    /// An entity jumped somewhere in one move — too far for deltas.
    /// Absolute coordinates, angles as bytes, ground flag for fall damage.
    "minecraft:teleport_entity" => TeleportEntity {
        entity_id: VarInt,
        x: f64,
        y: f64,
        z: f64,
        yaw: Angle,
        pitch: Angle,
        on_ground: bool,
    },

    /// Whether the world ticks at all, and how fast.
    "minecraft:ticking_state" => TickingState {
        tick_rate: f32,
        frozen: bool,
    },

    /// Advance the world a fixed number of ticks while frozen — the
    /// single-step button, spelled as a packet.
    "minecraft:ticking_step" => TickStep {
        tick_steps: VarInt,
    },

    /// Transfer the player to another server without dropping the
    /// connection. Same layout as its configuration twin.
    "minecraft:transfer" => Transfer {
        host: BoundedString<256>,
        port: VarInt,
    },

    /// An entity's attributes changed: base values and every modifier.
    /// Modifier ids are identifiers on this version — see
    /// [`crate::packets::play::attributes::AttributeModifier`].
    "minecraft:update_attributes" => UpdateAttributes {
        entity_id: VarInt,
        properties: Vec<crate::packets::play::attributes::AttributeProperty>,
    },

    /// A mob effect began or changed: which effect, how strong, how long,
    /// and how it presents. Flags are bits — see [`EffectFlags`].
    "minecraft:update_mob_effect" => ApplyMobEffect {
        entity_id: VarInt,
        effect_id: VarInt,
        amplifier: VarInt,
        duration: VarInt,
        flags: EffectFlags,
    },

    /// Every tag for every synced registry. Same layout as configuration's
    /// update_tags; play gets its own copy because registries can change
    /// across a reconfiguration.
    "minecraft:update_tags" => UpdateTags {
        registries: Vec<crate::packets::common::TagRegistry>,
    },

    /// A projectile was launched or deflected: how hard it accelerates.
    "minecraft:projectile_power" => ProjectilePower {
        entity_id: VarInt,
        acceleration_power: f64,
    },

    /// Hand a cookie to the client for later retrieval — including by
    /// another server after a transfer. Same layout as its configuration
    /// twin.
    "minecraft:store_cookie" => StoreCookie {
        key: Identifier,
        payload: crate::types::PrefixedBytes<5120>,
    },

    /// Key-value pairs that land in the crash report's environment
    /// section. Same layout as its configuration twin.
    "minecraft:custom_report_details" => CustomReportDetails {
        details: Vec<crate::packets::common::ReportDetail>,
    },

    /// The links a server may show on the pause screen: bug tracker, rules,
    /// whatever it declares. Same layout as its configuration twin.
    "minecraft:server_links" => ServerLinks {
        links: Vec<crate::packets::common::ServerLink>,
    },
}
