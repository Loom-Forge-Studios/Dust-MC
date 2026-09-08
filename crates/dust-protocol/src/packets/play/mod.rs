//! Play: the connection once the world exists.
//!
//! # Where this state stands
//!
//! Every packet this version's table lists is defined here except five, and
//! those five are blocked rather than skipped. Each carries one of two walls,
//! named so nobody has to rediscover which one applies:
//!
//! | Packet | Direction | Blocker |
//! | --- | --- | --- |
//! | `minecraft:delete_chat` | clientbound | **Chat signing** — removes messages by signature digest |
//! | `minecraft:chat_command_signed` | serverbound | **Chat signing** — argument signatures and acknowledgements |
//! | `minecraft:chat_session_update` | serverbound | **Chat signing** — session keys this crate never verifies |
//! | `minecraft:debug_sample` | clientbound | **Dev-only** — F3 debug samples; the vanilla server gates them on operator status |
//! | `minecraft:debug_sample_subscription` | serverbound | **Dev-only** — subscribes to the above |
//!
//! The signing wall is [`chat`]'s: Dust is offline-first, and the day online
//! mode arrives, the layouts there are where verification plugs in. The dev
//! pair is a pair because neither half means anything alone.
//!
//! # `chat_command` was on that list and never belonged on it
//!
//! It sat under the signing wall on the reasoning that "unsigned commands
//! still ride the signed-chat envelope". That was true of 1.20.4 and stopped
//! being true in 1.20.5, which split the packet in two: `chat_command_signed`
//! kept the timestamp, the salt, the per-argument signatures and the
//! acknowledgement chain, and `chat_command` was left holding **a single
//! string and nothing else**. A client sends the signed half only for a
//! command with an argument the server declared as `minecraft:message`;
//! everything else — `/time`, `/gamemode`, `/give` — arrives unsigned.
//!
//! Confirmed against `minecraft-data`, which is a separate implementation of
//! this protocol and describes the same one-field container. The wall that
//! remains is real and is the signed half's.
//!
//! # The Slot wall was never a clientbound wall
//!
//! `container_set_content` and `container_set_slot` sat on that list until the
//! inventory needed them, and the reason they came off is worth stating,
//! because the same mistake is available for every other Slot-carrying packet.
//! [`crate::types::Slot`] refuses a stack whose *added* components are
//! present, and it refuses it **on decode**: a component carries no length, so
//! an unknown one cannot be stepped over. Encoding has no such problem. It
//! writes a zero for the additions and knows exactly where every byte goes.
//!
//! These two are clientbound, so the server only ever encodes them, and the
//! wall is on the side it never touches. What is still true is that a body
//! decoded back — by the round-trip tests here, or by anything replaying a
//! capture — refuses added components exactly as it does everywhere else. That
//! is the same refusal, not a new one, and it is why these could be claimed
//! without finishing the component work: what Dust *sends* has no added
//! components, because Dust does not model any.
//!
//! [`crate::packets::unclaimed_for`] returns exactly this list for 1.21.1, and
//! a test pins it: a packet added here must either leave that list or update
//! it deliberately.
//!
//! When every Play packet is defined, both pairs graduate into
//! [`crate::packets::COMPLETE_PAIRS`] and become guarded from drift.
//!
//! # The three shapes the macro cannot say, and what each became
//!
//! [`packet_group`](crate::packet_group) defines bodies whose fields are a
//! fixed sequence. Three Play packets are not that, and each got a different
//! treatment worth naming before copying:
//!
//! - **A value-dependent tail** (entity metadata: entries until a terminator)
//!   became a named field type that owns the loop — [`metadata::MetadataEntries`].
//!   The packet definition stays declarative; the branch hides inside one type
//!   with one job.
//! - **A header that decides later fields' shapes** (player info update: a
//!   bitmask selecting six per-entry layouts) could not be split across fields,
//!   because a field cannot see the one before it. It became a single body
//!   struct holding everything after the id — see [`player_info`].
//! - **A blob another crate owns** (chunk sections) stayed bytes on this side
//!   of a trait — see [`chunk::Section`]. The envelope is exact; the contents
//!   are dust-world's.
//!
//! # What this crate refuses to know about chat signing
//!
//! Since 1.19 the chat packets carry RSA signatures, session keys and message
//! acknowledgements. Dust is offline-first and verifies none of that, but the
//! fields are *laid out* here in full, because a decoder that skipped what it
//! did not understand could not find the end of the packet. Signatures travel
//! as opaque byte arrays ([`chat::SignatureBytes`]); the day online mode
//! arrives, verification plugs in where those bytes are interpreted, and no
//! layout changes. See [`chat`].

pub mod advancements;
pub mod attributes;
pub mod boss_bar;
pub mod chat;
pub mod chunk;
pub mod clientbound;
pub mod commands;
pub mod containers;
pub mod map_item;
pub mod metadata;
pub mod particle;
pub mod player_info;
pub mod scoreboard;
pub mod serverbound;
pub mod sound;

use crate::types::{Decode, Encode, Identifier, Position, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{var_int_enum, wire_struct, ProtocolVersion};

var_int_enum! {
    /// How a player may interact with the world.
    ///
    /// The ids are the registry's own and appear in more than one encoding:
    /// the join packet carries the current mode as a bare byte, the tab list
    /// carries it as a VarInt. Both spellings resolve through
    /// [`Gamemode::from_discriminant`], so there is exactly one table to get
    /// right.
    pub enum Gamemode {
        Survival = 0,
        Creative = 1,
        Adventure = 2,
        Spectator = 3,
    }
}

var_int_enum! {
    /// How much the world fights back.
    ///
    /// Travels as a bare byte in both directions of the difficulty packets,
    /// never as a VarInt — see [`DifficultyByte`] for the spelling the wire
    /// uses.
    pub enum Difficulty {
        Peaceful = 0,
        Easy = 1,
        Normal = 2,
        Hard = 3,
    }
}

/// The difficulty packets' spelling of [`Difficulty`]: one unsigned byte.
///
/// The same wrapper [`GameModeByte`] is for gamemodes: the value travels
/// narrower than a VarInt, so the enum's VarInt codec cannot be reused and a
/// bare `u8` would lose the closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DifficultyByte(pub Difficulty);

impl Decode for DifficultyByte {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let raw = input.read_u8()?;
        let difficulty =
            Difficulty::from_discriminant(i32::from(raw)).ok_or(DecodeError::UnknownVariant {
                name: "Difficulty",
                value: i32::from(raw),
            })?;
        Ok(Self(difficulty))
    }
}

impl Encode for DifficultyByte {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_u8(self.0.discriminant() as u8);
        Ok(())
    }
}

/// The join packet's spelling of [`Gamemode`]: one unsigned byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameModeByte(pub Gamemode);

impl Decode for GameModeByte {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let raw = input.read_u8()?;
        let gamemode =
            Gamemode::from_discriminant(i32::from(raw)).ok_or(DecodeError::UnknownVariant {
                name: "Gamemode",
                value: i32::from(raw),
            })?;
        Ok(Self(gamemode))
    }
}

impl Encode for GameModeByte {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_u8(self.0.discriminant() as u8);
        Ok(())
    }
}

/// The join packet's spelling of the *previous* [`Gamemode`]: a signed byte
/// where −1 means "there was no previous mode".
///
/// A fresh join has no previous mode, which makes the sentinel load-bearing:
/// modelling this as a bare [`Gamemode`] would force a lie onto every first
/// spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PreviousGameMode(pub Option<Gamemode>);

impl Decode for PreviousGameMode {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let raw = input.read_i8()?;
        Ok(Self(match raw {
            -1 => None,
            other => Some(Gamemode::from_discriminant(i32::from(other)).ok_or(
                DecodeError::UnknownVariant {
                    name: "Gamemode",
                    value: i32::from(other),
                },
            )?),
        }))
    }
}

impl Encode for PreviousGameMode {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        let raw = match self.0 {
            None => -1,
            Some(mode) => mode.discriminant() as i8,
        };
        out.write_i8(raw);
        Ok(())
    }
}

wire_struct! {
    /// Where the player died, carried back on join so the client can show it.
    ///
    /// On the wire this is one boolean followed by two fields, which is the
    /// protocol's "optional" spelled across three slots instead of one; the
    /// struct is what makes it one thing again.
    pub struct DeathLocation {
        dimension: Identifier,
        position: Position,
    }
}

wire_struct! {
    /// An entity's motion, in units of 1/8000 of a block per tick.
    ///
    /// The unit is the protocol's own and appears wherever entities move
    /// abruptly — spawns carry an initial velocity, hits carry the knockback.
    /// The three shorts stay together because every user wants all three and
    /// the unit is easy to lose when they travel apart.
    pub struct EntityVelocity {
        x: i16,
        y: i16,
        z: i16,
    }
}

wire_struct! {
    /// One block destroyed by an explosion, relative to the explosion's centre.
    ///
    /// Three *signed* bytes, one per axis. The offsets are small — an explosion
    /// reaches a few blocks, not a few hundred — which is why they are bytes at
    /// all; a `Position` here would be eight bytes of mostly sign bits.
    pub struct ExplosionRecord {
        x: i8,
        y: i8,
        z: i8,
    }
}

var_int_enum! {
    /// Which hand holds the item an animation or use refers to.
    ///
    /// A VarInt enum rather than a bool because the wire spells it as one, and a
    /// third value arriving from a future version should be refused by name rather
    /// than read as "off hand".
    pub enum Hand {
        Main = 0,
        Off = 1,
    }
}

wire_struct! {
    /// An entity position change, in units of 1/4096 of a block.
    ///
    /// Distinct from [`EntityVelocity`] in meaning and in unit, and the two
    /// are separate types precisely so that a delta cannot be written where a
    /// velocity belongs — they are the same Rust shape, three `i16`s, and a
    /// swap would compile forever and look like lag.
    pub struct EntityDelta {
        x: i16,
        y: i16,
        z: i16,
    }
}

/// Which parts of a position sync are relative to the client's current values.
///
/// A cleared flag means absolute: "you are at these coordinates". A set flag
/// means relative: "move this far". The distinction decides whether a lost
/// packet drifts the client permanently, so the flags are a named type rather
/// than a bare byte — code that syncs positions should be able to ask
/// [`TeleportFlags::is_relative`] instead of knowing that bit 3 is pitch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TeleportFlags(pub u8);

impl TeleportFlags {
    pub const X: u8 = 0x01;
    pub const Y: u8 = 0x02;
    pub const Z: u8 = 0x04;
    pub const PITCH: u8 = 0x08;
    pub const YAW: u8 = 0x10;

    /// Whether this axis is relative. The naming follows the protocol's own,
    /// where yaw is rotation about the X axis and pitch about the Y axis —
    /// which is why the constants and the axis names do not line up.
    pub fn is_relative(self, mask: u8) -> bool {
        self.0 & mask != 0
    }
}

impl Decode for TeleportFlags {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        input.read_u8().map(Self)
    }
}

impl Encode for TeleportFlags {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_u8(self.0);
        Ok(())
    }
}

/// The player ability bits, as the client toggles and the server grants them.
///
/// One subtlety lives in the combination, not the bits: flying set while
/// allow-flying is clear is a player who cannot stop flying. That is why
/// [`Abilities::can_stop_flying`] exists — it is the question the game logic
/// actually asks, and asking it through two field reads invites getting one of
/// them backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Abilities(pub u8);

impl Abilities {
    pub const INVULNERABLE: u8 = 0x01;
    pub const FLYING: u8 = 0x02;
    pub const ALLOW_FLYING: u8 = 0x04;
    pub const INSTANT_BREAK: u8 = 0x08;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    pub fn can_stop_flying(self) -> bool {
        !self.has(Self::FLYING) || self.has(Self::ALLOW_FLYING)
    }
}

impl Decode for Abilities {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        input.read_u8().map(Self)
    }
}

impl Encode for Abilities {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_u8(self.0);
        Ok(())
    }
}

/// A packed chunk-section coordinate: where one 16³ slice of the world sits.
///
/// The packing is the block-position packing's sibling with different widths —
/// 22 bits each for x and z and 20 for y, signed — and the same failure mode:
/// a reader that masks without sign-extending works around spawn and breaks
/// under the ocean. The shifts below are arithmetic for exactly the reason
/// [`crate::types::Position`] documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChunkSectionPosition(pub i64);

impl ChunkSectionPosition {
    pub fn pack(x: i32, y: i32, z: i32) -> Self {
        Self(
            ((i64::from(x) & 0x3F_FFFF) << 42)
                | ((i64::from(z) & 0x3F_FFFF) << 20)
                | (i64::from(y) & 0xF_FFFF),
        )
    }

    pub fn x(self) -> i32 {
        (self.0 >> 42) as i32
    }

    pub fn y(self) -> i32 {
        ((self.0 << 44) >> 44) as i32
    }

    pub fn z(self) -> i32 {
        ((self.0 << 22) >> 42) as i32
    }
}

impl Decode for ChunkSectionPosition {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        input.read_i64().map(Self)
    }
}

impl Encode for ChunkSectionPosition {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_i64(self.0);
        Ok(())
    }
}

/// One changed block inside a multi-block change, packed as the wire packs it.
///
/// The long holds twelve bits of block state above twelve bits of
/// section-local coordinates, and the coordinate nibbles run **y lowest** — z
/// above it and x above that — which is the opposite order to nearly everything
/// else in the protocol and therefore exactly the order an implementation gets
/// wrong once and ships. The accessors here are the single place that order is
/// written down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockChangeEntry(pub u64);

impl BlockChangeEntry {
    pub const STATE_BITS: u32 = 12;

    /// Pack from a state id and section-local coordinates, each masked to its
    /// nibble as vanilla masks. A coordinate outside 0..16 is a caller bug,
    /// not a wire concern; masking keeps the long well-formed regardless.
    pub fn pack(state_id: u32, local_x: u8, local_y: u8, local_z: u8) -> Self {
        Self(
            (u64::from(state_id) << Self::STATE_BITS)
                | (u64::from(local_x & 0xF) << 8)
                | (u64::from(local_z & 0xF) << 4)
                | u64::from(local_y & 0xF),
        )
    }

    pub fn state_id(self) -> u32 {
        (self.0 >> Self::STATE_BITS) as u32
    }

    pub fn local_x(self) -> u8 {
        ((self.0 >> 8) & 0xF) as u8
    }

    pub fn local_y(self) -> u8 {
        (self.0 & 0xF) as u8
    }

    pub fn local_z(self) -> u8 {
        ((self.0 >> 4) & 0xF) as u8
    }
}

impl Decode for BlockChangeEntry {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        // The entry travels as a VarLong. Its bits fit in the low twenty-four
        // places for any block registry that will exist soon, so the two's
        // complement round trip through i64 is exact; casting is reading the
        // unsigned meaning of the same bytes, which is what vanilla stores.
        input.read_var_long().map(|bits| Self(bits as u64))
    }
}

impl Encode for BlockChangeEntry {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_var_long(self.0 as i64);
        Ok(())
    }
}

/// An entity id the wire spells as **id + 1**, so that zero can mean "none".
///
/// The damage event's two causes use this spelling. It looks like an
/// `Option<i32>` and is one — but a plain `Option` codec would write a
/// boolean, not an offset integer, which is why it has its own type: the
/// offset is the kind of detail that round-trips perfectly against itself
/// and means nothing to vanilla.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OffsetEntityId(pub Option<i32>);

impl Decode for OffsetEntityId {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let raw = input.read_var_int()?;
        // Zero is "no entity"; anything else is id + 1.
        Ok(Self((raw != 0).then(|| raw - 1)))
    }
}

impl Encode for OffsetEntityId {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        let raw = match self.0 {
            None => 0,
            Some(id) => id + 1,
        };
        out.write_var_int(raw);
        Ok(())
    }
}

/// Where a damage came from, when the source has no entity: three doubles.
///
/// A struct rather than bare fields because the whole triple is optional as
/// one unit, and `Option<(f64, f64, f64)>` reads as a tuple that happens to
/// be optional rather than a position that happens to be three floats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DamageSourcePosition {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Decode for DamageSourcePosition {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            x: input.read_f64()?,
            y: input.read_f64()?,
            z: input.read_f64()?,
        })
    }
}

impl Encode for DamageSourcePosition {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_f64(self.x);
        out.write_f64(self.y);
        out.write_f64(self.z);
        Ok(())
    }
}

var_int_enum! {
    /// How an explosion interacts with the blocks it touches.
    ///
    /// The client renders differently for each; the server decides, so this
    /// travels one way.
    pub enum ExplosionInteraction {
        Keep = 0,
        Destroy = 1,
        DestroyWithDecay = 2,
        TriggerBlock = 3,
    }
}

var_int_enum! {
    /// Which end of an entity "look at" measures from.
    pub enum Anchor {
        Feet = 0,
        Eyes = 1,
    }
}

/// The entity half of a look-at: whose position to face, and from where on
/// that entity.
///
/// The anchor repeats because the target entity's anchor is independent of
/// the player's own; flattening them into one field would make the packet's
/// two anchors read as one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LookAtTarget {
    pub entity_id: VarInt,
    pub anchor: Anchor,
}

var_int_enum! {
    /// Whether an entity link is being made or broken.
    ///
    /// One link kind per packet; the ids are the protocol's own and a third
    /// from a future peer is refused by name.
    pub enum EntityLinkKind {
        Remove = 0,
        Ride = 1,
    }
}

/// The status-effect presentation bits: ambient, particles, icon.
///
/// A newtype over the byte rather than bare `u8` fields because all three
/// bits travel together and game code asks single questions of them —
/// "does this look like a drink?", "is this hidden?" — which reads better
/// through [`EffectFlags::has`] than through shifts at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EffectFlags(pub u8);

impl EffectFlags {
    pub const AMBIENT: u8 = 0x01;
    pub const SHOW_PARTICLES: u8 = 0x02;
    pub const SHOW_ICON: u8 = 0x04;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

impl Decode for EffectFlags {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        input.read_u8().map(Self)
    }
}

impl Encode for EffectFlags {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_u8(self.0);
        Ok(())
    }
}

/// The respawn packet's keep flags, as one byte.
///
/// Bit 0 keeps entity metadata across the dimension change and bit 1 keeps
/// the entities themselves; both clear means a fresh world. The bits exist
/// so respawning in place — end credits, `/respawn` — does not force the
/// client to rebuild everything it already knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RespawnFlags(pub u8);

impl RespawnFlags {
    pub const KEEP_ENTITY_METADATA: u8 = 0x01;
    pub const KEEP_ENTITIES: u8 = 0x02;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

impl Decode for RespawnFlags {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        input.read_u8().map(Self)
    }
}

impl Encode for RespawnFlags {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_u8(self.0);
        Ok(())
    }
}

impl Decode for LookAtTarget {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            entity_id: VarInt::decode(input, version)?,
            anchor: Anchor::decode(input, version)?,
        })
    }
}

impl Encode for LookAtTarget {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.entity_id.encode(out, version)?;
        self.anchor.encode(out, version)
    }
}
