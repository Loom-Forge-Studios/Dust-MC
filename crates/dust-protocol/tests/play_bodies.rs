//! Behaviour tests for the pieces of Play that are more than a field list.
//!
//! The corpus in [`common`] proves every Play packet round-trips. This file
//! proves the *policies*: which metadata serializers are refused and why, how
//! a chunk blob is walked and when that walk fails, which nibble of a
//! multi-block entry is which coordinate, what an acknowledged message with id
//! zero promises, and the handful of flag semantics game code will lean on.
//! Each test names the behaviour it pins, so a red line reads as a sentence.

mod common;

use common::{corpus, v};
use dust_protocol::packets::play::chat::{
    AcknowledgedMessage, ChatFilter, MessageAcknowledgement, SignatureBytes,
};
use dust_protocol::packets::play::chunk::{
    ChunkData, LightArray, LightData, Section, CHUNK_DATA_MAX_BYTES, LIGHT_SECTION_BYTES,
};
use dust_protocol::packets::play::metadata::{MetadataEntries, MetadataEntry, MetadataValue, Pose};
use dust_protocol::packets::play::player_info::{
    PlayerInfoActions, PlayerInfoBody, PlayerInfoEntry,
};
use dust_protocol::packets::play::serverbound as sb;
use dust_protocol::packets::play::{
    Abilities, BlockChangeEntry, ChunkSectionPosition, EntityVelocity, GameModeByte, Gamemode,
    PreviousGameMode, TeleportFlags,
};
use dust_protocol::types::{BitSet, Decode, Encode, FixedBitSet, PrefixedBytes, VarInt};
use dust_protocol::wire::{DecodeError, EncodeError, Reader, WireRead, WireWrite, Writer};

// ---------------------------------------------------------------------------
// Packed coordinates: the sign-extension traps, again, at different widths
// ---------------------------------------------------------------------------

#[test]
fn a_chunk_section_position_sign_extends_its_three_fields() {
    // 22/20/22 bits this time, negative y included, because sections below y=0
    // are where everyone digs first.
    for (x, y, z) in [
        (-4, 3, 12),
        (-1, -5, -1),
        (262_143, -524_288, -262_144),
        (0, 0, 0),
    ] {
        let packed = ChunkSectionPosition::pack(x, y, z);
        assert_eq!(packed.x(), x, "{packed:?}");
        assert_eq!(packed.y(), y, "{packed:?}");
        assert_eq!(packed.z(), z, "{packed:?}");
    }

    // And it travels as a plain long.
    let mut writer = Writer::new();
    ChunkSectionPosition::pack(-2, -1, 3)
        .encode(&mut writer, v())
        .expect("encodes");
    let back =
        ChunkSectionPosition::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
    assert_eq!((back.x(), back.y(), back.z()), (-2, -1, 3));
}

#[test]
fn a_block_change_entry_knows_which_nibble_is_which() {
    // The published example order: state above x above z above **y**. The
    // known-answer rows were computed from the format description by hand;
    // they are here because a swap of two nibbles still round-trips.
    let entry = BlockChangeEntry::pack(4095, 15, 14, 13);
    assert_eq!(entry.state_id(), 4095);
    assert_eq!(entry.local_x(), 15);
    assert_eq!(entry.local_y(), 14);
    assert_eq!(entry.local_z(), 13);

    let mut writer = Writer::new();
    entry.encode(&mut writer, v()).expect("encodes");
    assert_eq!(
        BlockChangeEntry::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        entry
    );
}

// ---------------------------------------------------------------------------
// Entity metadata: the closed enum over an open set
// ---------------------------------------------------------------------------

fn one_entry(index: u8, serializer: i32, payload: &[u8]) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.write_u8(index);
    writer.write_var_int(serializer);
    writer.write_slice(payload);
    writer.write_u8(0xFF);
    writer.into_bytes()
}

#[test]
fn metadata_entries_run_until_the_terminator_and_stop_there() {
    // Two entries then the terminator, followed by another field's bytes. A
    // reader that kept going past 0xFF would eat them.
    let mut bytes = one_entry(2, 0, &[7]);
    bytes.push(0xAA);
    let mut reader = Reader::new(&bytes);
    let entries = MetadataEntries::decode(&mut reader, v()).expect("decodes");
    assert_eq!(
        entries.0,
        vec![MetadataEntry {
            index: 2,
            value: MetadataValue::Byte(7),
        }]
    );
    assert_eq!(reader.read_u8(), Ok(0xAA));

    // No entries at all is just the terminator, and encodes as exactly it.
    let empty = MetadataEntries(vec![]);
    let mut writer = Writer::new();
    empty.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.into_bytes(), vec![0xFF]);
}

#[test]
fn metadata_offsets_distinguish_absent_from_zero() {
    // Optional entity id: zero means absent; a real entity zero would be a 1
    // on the wire. Same shape for optional block state.
    let value = MetadataValue::OptionalEntityId(None);
    let mut writer = Writer::new();
    value.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.as_bytes(), &[20, 0], "serializer 20, absent");

    let present = MetadataValue::OptionalEntityId(Some(VarInt(0)));
    let mut writer = Writer::new();
    present.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.as_bytes(), &[20, 1], "entity zero is wire 1");
    let back = MetadataValue::read(20, &mut Reader::new(&[1]), v()).expect("decodes");
    assert_eq!(back, present);

    let block_state = MetadataValue::OptionalBlockState(Some(VarInt(9)));
    let mut writer = Writer::new();
    block_state.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.as_bytes(), &[15, 10]);
    let back = MetadataValue::read(15, &mut Reader::new(&[10]), v()).expect("decodes");
    assert_eq!(back, block_state);

    // Serializer 7, the item stack, which was on the refusal list until item
    // entities needed it: an item entity's index-8 metadata *is* a stack, and
    // an item entity without it renders as nothing at all. Round-tripped like
    // every other modelled one — a count, an item id, no added components and
    // no removed ones, which is what a stack with no data components says.
    let stack = MetadataValue::Slot(dust_protocol::types::Slot::Present {
        count: 3,
        item_id: 42,
        components: dust_protocol::components::ComponentPatch::EMPTY,
    });
    let mut writer = Writer::new();
    stack.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.as_bytes(), &[7, 3, 42, 0, 0]);
    let back = MetadataValue::read(7, &mut Reader::new(&[3, 42, 0, 0]), v()).expect("decodes");
    assert_eq!(back, stack);
}

#[test]
fn unknown_metadata_serializers_are_refused_by_id_and_never_guessed_at() {
    // The open-set policy in one line each: a modelled-but-unimplementable
    // serializer names its reason; an unmodelled id says why no guess is
    // possible. Neither may panic, default, or skip.
    for (serializer, payload) in [
        (17i32, &[0][..]), // particle: options have no length
        (18, &[][..]),     // particles
        (23, &[0][..]),    // wolf variant, inline form
        (26, &[0][..]),    // painting variant, inline form
        (31, &[0][..]),    // not modelled at all on this version
        (-3, &[][..]),     // hostile id
    ] {
        let bytes = one_entry(0, serializer, payload);
        match MetadataEntries::decode(&mut Reader::new(&bytes), v()) {
            Err(DecodeError::Unsupported { field, .. }) => {
                assert_eq!(field, "entity metadata", "{serializer}")
            }
            other => panic!("serializer {serializer} was accepted: {other:?}"),
        }
    }

    // The registry-id family shares one variant precisely because resolving
    // ids belongs to dust-registry; cat variant (22) decodes raw.
    let bytes = one_entry(4, 22, &[5]);
    let entries = MetadataEntries::decode(&mut Reader::new(&bytes), v()).expect("decodes");
    assert_eq!(entries.0[0].value, MetadataValue::RegistryId(VarInt(5)));
}

#[test]
fn pose_refuses_a_discriminant_this_version_does_not_define() {
    let bytes = [99];
    assert_eq!(
        Pose::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownVariant {
            name: "Pose",
            value: 99
        })
    );
}

// ---------------------------------------------------------------------------
// Chunks: the envelope is ours, the contents are the world crate's
// ---------------------------------------------------------------------------

/// A stand-in for dust-world's future section type: consumes a fixed number of
/// bytes per section, enough to prove the walk without implementing palettes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FakeSection(u32);

impl Section for FakeSection {
    fn decode_wire<R: dust_protocol::wire::WireRead + ?Sized>(
        input: &mut R,
        _version: dust_protocol::ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let bytes = input.read_slice(4)?;
        Ok(Self(u32::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ])))
    }

    fn encode_wire<W: dust_protocol::wire::WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: dust_protocol::ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_slice(&self.0.to_be_bytes());
        Ok(())
    }
}

#[test]
fn chunk_sections_are_walked_bottom_up_and_the_rest_of_the_column_is_air() {
    // Two four-byte sections, big-endian as the fake decoder reads them.
    let data = ChunkData(PrefixedBytes(vec![0, 0, 0, 1, 0, 0, 0, 2]));
    let sections = data
        .parse_sections::<FakeSection>(5, v())
        .expect("two sections fit a five-tall column");
    assert_eq!(
        sections,
        vec![Some(FakeSection(1)), Some(FakeSection(2)), None, None, None]
    );
}

#[test]
fn a_section_that_overreads_fails_the_walk_rather_than_shifting_the_rest() {
    // The blob holds three bytes; the section decoder wants four. The failure
    // must surface here, at this section, instead of leaving the next section
    // (or the trailing check) to discover it.
    let data = ChunkData(PrefixedBytes(vec![1, 0, 0]));
    assert!(data.parse_sections::<FakeSection>(1, v()).is_err());
}

#[test]
fn more_sections_than_the_column_is_tall_are_an_error() {
    // One section of data, a column declared one tall, and a whole second
    // section's bytes still sitting there: the walk refuses rather than
    // guessing whether they were padding.
    let data = ChunkData(PrefixedBytes(vec![
        0, 0, 0, 1, // section zero
        9, 9, 9, 9, // nobody claimed this
    ]));
    assert!(matches!(
        data.parse_sections::<FakeSection>(1, v()),
        Err(DecodeError::Nbt { .. })
    ));
}

#[test]
fn a_hostile_chunk_length_is_bounded_before_allocation() {
    // Four mebibytes is the bound; a prefix claiming more is refused by name
    // before any allocation, which is the same discipline strings follow.
    let mut writer = Writer::new();
    writer.write_var_int(CHUNK_DATA_MAX_BYTES as i32 + 1);
    writer.write_slice(b"tiny");
    assert!(matches!(
        ChunkData::decode(&mut Reader::new(writer.as_bytes()), v()),
        Err(DecodeError::StringTooLong { .. })
    ));
}

#[test]
fn light_arrays_carry_exactly_two_kilobytes_each() {
    let light = LightData {
        sky_mask: bitset(&[1]),
        block_mask: bitset(&[]),
        empty_sky_mask: bitset(&[]),
        empty_block_mask: bitset(&[]),
        sky_arrays: vec![LightArray(vec![0; LIGHT_SECTION_BYTES])],
        block_arrays: vec![],
    };
    let mut writer = Writer::new();
    light.encode(&mut writer, v()).expect("encodes");
    let back = LightData::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
    assert_eq!(back.sky_arrays.len(), 1);

    // The length is a constant of the format, so an array claiming any other
    // size is refused at the byte, not read as a shorter truth.
    let mut hostile = Writer::new();
    hostile.write_var_int(100);
    assert!(matches!(
        LightArray::decode(&mut Reader::new(hostile.as_bytes()), v()),
        Err(DecodeError::StringTooLong {
            limit: 2048,
            actual: 100
        })
    ));
}

fn bitset(bits: &[usize]) -> BitSet {
    let mut set = BitSet::default();
    for &bit in bits {
        set.set(bit, true);
    }
    set
}

// ---------------------------------------------------------------------------
// Chat fields: the signing seam's layout rules
// ---------------------------------------------------------------------------

#[test]
fn an_acknowledged_message_with_id_zero_carries_its_signature_inline() {
    let signature: SignatureBytes = core::array::from_fn(|i| i as u8);
    let referenced = AcknowledgedMessage {
        id: VarInt(5),
        signature: None,
    };
    let inline = AcknowledgedMessage {
        id: VarInt(0),
        signature: Some(signature),
    };

    for message in [&referenced, &inline] {
        let mut writer = Writer::new();
        message.encode(&mut writer, v()).expect("encodes");
        let bytes = writer.into_bytes();
        assert_eq!(
            AcknowledgedMessage::decode(&mut Reader::new(&bytes), v()).expect("decodes"),
            *message
        );
    }

    // The two states the type can hold but the wire cannot spell: both are
    // refusals, because writing either would desynchronise the peer.
    let mut writer = Writer::new();
    let lies = [
        AcknowledgedMessage {
            id: VarInt(0),
            signature: None,
        },
        AcknowledgedMessage {
            id: VarInt(2),
            signature: Some(signature),
        },
    ];
    for lie in &lies {
        assert!(matches!(
            lie.encode(&mut writer, v()),
            Err(EncodeError::Unsupported { .. })
        ));
    }
}

#[test]
fn the_chat_filter_mask_exists_only_when_filtering_was_partial() {
    for (filter, extra_bytes) in [
        (ChatFilter::PassThrough, 0),
        (ChatFilter::FullyFiltered, 0),
        (ChatFilter::PartiallyFiltered(bitset(&[3])), 1 + 8),
    ] {
        let mut writer = Writer::new();
        filter.encode(&mut writer, v()).expect("encodes");
        assert_eq!(writer.len(), 1 + extra_bytes, "{filter:?}");
        let back = ChatFilter::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
        assert_eq!(back, filter);
    }
}

#[test]
fn the_acknowledgement_bit_set_is_twenty_bits_with_no_prefix() {
    let ack = MessageAcknowledgement {
        offset: VarInt(9),
        acknowledged: FixedBitSet::<20>::new(),
    };
    let mut writer = Writer::new();
    ack.encode(&mut writer, v()).expect("encodes");
    // One byte of VarInt offset plus ceil(20/8) = 3 fixed bytes.
    assert_eq!(writer.len(), 4);
}

// ---------------------------------------------------------------------------
// Small semantics game code will lean on
// ---------------------------------------------------------------------------

#[test]
fn teleport_flags_ask_whether_an_axis_is_relative() {
    let flags = TeleportFlags(TeleportFlags::X | TeleportFlags::YAW);
    assert!(flags.is_relative(TeleportFlags::X));
    assert!(flags.is_relative(TeleportFlags::YAW));
    assert!(!flags.is_relative(TeleportFlags::PITCH));
    assert!(!flags.is_relative(TeleportFlags::Y));

    // All clear means everything absolute — the ordinary hard teleport.
    assert!(!TeleportFlags(0).is_relative(0xFF));
}

#[test]
fn flying_without_permission_is_a_player_who_cannot_land() {
    assert!(Abilities(Abilities::FLYING).can_stop_flying().eq(&false));
    assert!(Abilities(Abilities::FLYING | Abilities::ALLOW_FLYING).can_stop_flying());
    assert!(Abilities(0).can_stop_flying());
}

#[test]
fn the_previous_gamemode_sentinel_survives_a_round_trip() {
    for mode in [
        PreviousGameMode(None),
        PreviousGameMode(Some(Gamemode::Spectator)),
    ] {
        let mut writer = Writer::new();
        mode.encode(&mut writer, v()).expect("encodes");
        assert_eq!(
            PreviousGameMode::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
            mode
        );
    }
    // None is the -1 byte, which is the whole point of the wrapper.
    let mut writer = Writer::new();
    PreviousGameMode(None)
        .encode(&mut writer, v())
        .expect("encodes");
    assert_eq!(writer.as_bytes(), &[0xFF]);

    // And an out-of-range byte is refused by naming the variant table.
    assert_eq!(
        GameModeByte::decode(&mut Reader::new(&[9]), v()),
        Err(DecodeError::UnknownVariant {
            name: "Gamemode",
            value: 9
        })
    );
}

#[test]
fn velocity_and_delta_units_are_distinct_types_that_do_not_convert() {
    // Nothing here can accidentally assign one to the other; the test only
    // pins that both survive the wire intact, since their layouts coincide.
    let velocity = EntityVelocity {
        x: i16::MIN,
        y: 0,
        z: i16::MAX,
    };
    let mut writer = Writer::new();
    velocity.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.len(), 6);
    assert_eq!(
        EntityVelocity::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        velocity
    );
}

// ---------------------------------------------------------------------------
// Player info update: the bitmask is law
// ---------------------------------------------------------------------------

#[test]
fn player_info_actions_outside_the_version_are_refused() {
    // An action bit a peer invented would leave every entry's length unknown.
    // Refusing the packet is the only safe answer, and it names the byte. The
    // bytes here are hand-written because the encoder refuses to produce them.
    let bytes = [0xFF, 0x00];
    assert_eq!(
        PlayerInfoBody::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownVariant {
            name: "PlayerInfoActions",
            value: 255
        })
    );
}

#[test]
fn each_action_selects_its_field_per_entry_without_prefixes() {
    // Update latency only: uuid, then the varint, nothing else. If presence
    // ever grew a boolean prefix, this byte count is where it shows.
    let body = PlayerInfoBody {
        actions: PlayerInfoActions(PlayerInfoActions::UPDATE_LATENCY),
        entries: vec![PlayerInfoEntry {
            uuid: dust_protocol::types::Uuid(1),
            latency: Some(VarInt(300)),
            ..PlayerInfoEntry::default()
        }],
    };
    let mut writer = Writer::new();
    body.encode(&mut writer, v()).expect("encodes");
    // actions(1) + count(1) + uuid(16) + varint(2)
    assert_eq!(writer.len(), 20);
    let back = PlayerInfoBody::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
    assert_eq!(back, body);

    // An enabled action with no data behind it is an encoding refusal, not a
    // silent default row on someone's tab list.
    let hollow = PlayerInfoBody {
        actions: PlayerInfoActions(PlayerInfoActions::UPDATE_GAME_MODE),
        entries: vec![PlayerInfoEntry::default()],
    };
    assert!(matches!(
        hollow.encode(&mut Writer::new(), v()),
        Err(EncodeError::Unsupported { .. })
    ));

    // And decoding enforces the mirror image: the entry must consume exactly
    // its selected fields, so a truncated body errors instead of inventing
    // values.
    let mut bytes = writer.into_bytes();
    bytes.pop();
    assert!(PlayerInfoBody::decode(&mut Reader::new(&bytes), v()).is_err());
}

// ---------------------------------------------------------------------------
// The corpus itself
// ---------------------------------------------------------------------------

#[test]
fn every_play_frame_still_decodes_through_its_group_dispatch() {
    // Cheap insurance that the shared corpus stays valid as definitions move:
    // if a layout changes under a stale sample, this is the first line to go
    // red, pointing at the packet by name.
    for frame in corpus() {
        if frame.state != dust_protocol::ConnectionState::Play {
            continue;
        }
        assert!(
            (frame.decodes)(&frame.bytes).is_ok(),
            "{} no longer decodes",
            frame.name
        );
    }
}

#[test]
fn play_definitions_claim_the_only_version_there_is() {
    // D3's dimension in miniature: nothing in a definition may assume the
    // version list will always be length one, so the claim is explicit and
    // this merely checks nobody typo'd the version string.
    use dust_protocol::packets::PacketBody;
    assert_eq!(
        sb::TeleportConfirm::protocol_id(v()),
        Some(
            v().protocol_id(
                dust_protocol::ConnectionState::Play,
                dust_protocol::Direction::Serverbound,
                "minecraft:accept_teleportation"
            )
            .expect("in table")
        )
    );
}

// ---------------------------------------------------------------------------
// Wave-two field types: layouts the definitions lean on
// ---------------------------------------------------------------------------

fn simple_slot(item_id: i32) -> dust_protocol::types::Slot {
    dust_protocol::types::Slot::Present {
        count: 1,
        item_id,
        components: dust_protocol::components::ComponentPatch::EMPTY,
    }
}

#[test]
fn equipment_entries_continue_while_the_high_bit_stands() {
    use dust_protocol::packets::play::containers::{
        EquipmentEntries, EquipmentEntry, EquipmentSlot,
    };
    use dust_protocol::types::Slot;

    let entries = EquipmentEntries(vec![
        EquipmentEntry {
            slot: EquipmentSlot::MainHand,
            item: Slot::Empty,
        },
        EquipmentEntry {
            slot: EquipmentSlot::Boots,
            item: Slot::Empty,
        },
        EquipmentEntry {
            slot: EquipmentSlot::Helmet,
            item: simple_slot(9),
        },
    ]);
    let mut writer = Writer::new();
    entries.encode(&mut writer, v()).expect("encodes");
    let bytes = writer.into_bytes();

    // The first two slot bytes carry the continuation bit; the last does not.
    // Empty slots are a single zero byte each, so the layout is fully visible.
    assert_eq!(
        bytes[0],
        EquipmentSlot::MainHand.discriminant() as u8 | 0x80
    );
    assert_eq!(bytes[2], EquipmentSlot::Boots.discriminant() as u8 | 0x80);
    assert_eq!(bytes[4], EquipmentSlot::Helmet.discriminant() as u8);
    assert_eq!(
        EquipmentEntries::decode(&mut Reader::new(&bytes), v()).expect("decodes"),
        entries
    );

    // An equipment update with nothing on it is not a message worth sending;
    // encoding one is refused rather than written as an empty frame.
    assert!(matches!(
        EquipmentEntries(vec![]).encode(&mut Writer::new(), v()),
        Err(EncodeError::Unsupported { .. })
    ));
}

#[test]
fn the_stop_sound_flags_byte_selects_among_four_layouts() {
    use dust_protocol::packets::play::sound::{SoundCategory, StopSoundBody};

    for (body, flags, len) in [
        (StopSoundBody::default(), 0u8, 1),
        (
            StopSoundBody {
                source: Some(SoundCategory::Ambient),
                name: None,
            },
            0x01,
            2,
        ),
        (
            StopSoundBody {
                source: None,
                name: Some(dust_protocol::types::Identifier::parse("dust:stop").expect("valid")),
            },
            0x02,
            1 + 10,
        ),
        (
            StopSoundBody {
                source: Some(SoundCategory::Master),
                name: Some(dust_protocol::types::Identifier::parse("dust:stop").expect("valid")),
            },
            0x03,
            12,
        ),
    ] {
        let mut writer = Writer::new();
        body.encode(&mut writer, v()).expect("encodes");
        assert_eq!(writer.as_bytes()[0], flags, "{body:?}");
        assert_eq!(writer.len(), len, "{body:?}");
        assert_eq!(
            StopSoundBody::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
            body
        );
    }

    // A flag outside the table leaves the layout unknowable, so it is named
    // and refused instead of half-read.
    assert_eq!(
        StopSoundBody::decode(&mut Reader::new(&[0x08]), v()),
        Err(DecodeError::UnknownVariant {
            name: "StopSoundBody",
            value: 8
        })
    );
}

#[test]
fn a_zero_column_map_patch_carries_no_rows_and_no_data() {
    use dust_protocol::packets::play::map_item::MapPatch;

    // "Icons only" is spelled as columns=0 followed by literally nothing.
    let patch = MapPatch {
        columns: 0,
        rows: 9,
        x: 9,
        z: 9,
        data: vec![9; 9],
    };
    let mut writer = Writer::new();
    patch.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.len(), 1, "the ignored fields must not be written");
    assert_eq!(
        MapPatch::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        MapPatch {
            columns: 0,
            rows: 0,
            x: 0,
            z: 0,
            data: vec![]
        }
    );

    let mut trailing = writer;
    trailing.write_slice(&[0xAA]);
    let mut reader = Reader::new(trailing.as_bytes());
    MapPatch::decode(&mut reader, v()).expect("decodes");
    assert_eq!(reader.read_u8(), Ok(0xAA), "the next field survives intact");
}

#[test]
fn boss_bar_actions_carry_exactly_their_own_fields() {
    use dust_protocol::packets::play::boss_bar::{BossBarAction, BossBarColor, BossBarDivision};

    // Remove is the uuid plus one varint and nothing else; add is everything.
    let remove = BossBarAction::Remove;
    let mut writer = Writer::new();
    remove.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.len(), 1);
    assert_eq!(
        writer.as_bytes()[0],
        1,
        "actions travel as their own varints"
    );

    let style = BossBarAction::UpdateStyle {
        color: BossBarColor::Yellow,
        division: BossBarDivision::Notches20,
    };
    let mut writer = Writer::new();
    style.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.len(), 3);

    // An action id from a future version is refused by name rather than read
    // as some other action. The bytes are hand-written because no encoder
    // here produces them.
    use dust_protocol::types::Decode as _;
    assert!(matches!(
        BossBarAction::decode(&mut Reader::new(&[6, 0xFF]), v()),
        Err(DecodeError::UnknownVariant {
            name: "BossBarAction",
            value: 6
        })
    ));
}

use dust_protocol::types::ProtocolString;

#[test]
fn the_offset_entity_id_spells_none_as_zero_and_ids_shifted_by_one() {
    use dust_protocol::packets::play::OffsetEntityId;

    let mut writer = Writer::new();
    OffsetEntityId(None)
        .encode(&mut writer, v())
        .expect("encodes");
    assert_eq!(writer.as_bytes(), &[0], "none is the reserved zero");

    let mut writer = Writer::new();
    OffsetEntityId(Some(0))
        .encode(&mut writer, v())
        .expect("encodes");
    // Entity zero is wire one: the shift is what keeps zero free.
    assert_eq!(writer.as_bytes(), &[1]);
    assert_eq!(
        OffsetEntityId::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        OffsetEntityId(Some(0))
    );

    let mut writer = Writer::new();
    OffsetEntityId(Some(i32::MAX - 1))
        .encode(&mut writer, v())
        .expect("encodes");
    assert_eq!(
        OffsetEntityId::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        OffsetEntityId(Some(i32::MAX - 1))
    );
}

#[test]
fn team_methods_carry_only_their_own_sections() {
    use dust_protocol::packets::play::scoreboard::{
        CollisionRule, NameTagVisibility, TeamBody, TeamInfo, TeamMethod,
    };

    // Remove-team is the method varint and nothing else.
    let mut writer = Writer::new();
    TeamBody {
        method: TeamMethod::Remove,
        info: None,
        members: vec![],
    }
    .encode(&mut writer, v())
    .expect("encodes");
    assert_eq!(writer.len(), 1);

    // A create without its descriptive fields would desynchronise the
    // client's reader; it is refused rather than written hollow.
    assert!(matches!(
        TeamBody {
            method: TeamMethod::Create,
            info: None,
            members: vec![],
        }
        .encode(&mut Writer::new(), v()),
        Err(EncodeError::Unsupported { .. })
    ));

    let full = TeamBody {
        method: TeamMethod::Create,
        info: Some(TeamInfo {
            display_name: dust_protocol::text::Component::text("Blue"),
            friendly_flags: 0,
            name_tag_visibility: NameTagVisibility::Always,
            collision_rule: CollisionRule::Never,
            colour: VarInt(11),
            prefix: dust_protocol::text::Component::text(""),
            suffix: dust_protocol::text::Component::text(""),
        }),
        members: vec![ProtocolString::new("jeb_").expect("fits")],
    };
    let mut writer = Writer::new();
    full.encode(&mut writer, v()).expect("encodes");
    assert_eq!(
        TeamBody::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        full
    );

    // The visibility words are strings on this version, not enum ids.
    let mut writer = Writer::new();
    ProtocolString::new("hideForOtherTeams")
        .expect("fits")
        .encode(&mut writer, v())
        .expect("encodes");
    assert_eq!(writer.len(), 1 + 17);
}

#[test]
fn objective_updates_treat_number_format_as_an_option_of_an_option() {
    use dust_protocol::packets::play::containers::NumberFormat;
    use dust_protocol::packets::play::scoreboard::{
        ObjectiveMode, ObjectiveRenderType, UpdateObjectivesBody,
    };

    fn body(format: Option<Option<NumberFormat>>) -> UpdateObjectivesBody {
        UpdateObjectivesBody {
            mode: ObjectiveMode::Update,
            display_name: Some(dust_protocol::text::Component::text("obj")),
            render_type: Some(ObjectiveRenderType::Integer),
            number_format: format,
        }
    }

    // No boolean at all; absent boolean; present blank. Three different
    // messages, each one byte longer than the last — the option-of-an-option
    // is what keeps them distinct on the wire.
    let mut previous_len = None;
    for format in [None, Some(None), Some(Some(NumberFormat::Blank))] {
        let mut writer = Writer::new();
        body(format.clone())
            .encode(&mut writer, v())
            .expect("encodes");
        match previous_len {
            None => {}
            Some(previous) => assert_eq!(writer.len(), previous + 1, "{format:?}"),
        }
        previous_len = Some(writer.len());
        let back = UpdateObjectivesBody::decode(&mut Reader::new(writer.as_bytes()), v())
            .expect("decodes");
        assert_eq!(back, body(format), "format changed on the way round");
    }
}

/// A player command is three VarInts, whatever the action.
///
/// It reads as though the boost should be conditional — only the horse-jump
/// actions mean anything by it — and this crate modelled it that way until a
/// real 1.21.1 server was asked. Sent two VarInts, vanilla disconnects with
/// `Failed to decode packet 'serverbound/minecraft:player_command'`; sent
/// three, it carries on. So every sneak and every sprint a real client sends
/// carries a zero here.
///
/// The lengths are written out rather than compared to each other: what makes
/// this test worth having is that a sneak and a horse jump are *the same size*
/// on the wire, and a test that only checked they round-tripped would have
/// passed under the wrong model too.
#[test]
fn every_player_command_carries_a_jump_boost_even_when_it_means_nothing() {
    use dust_protocol::packets::play::serverbound::{PlayerCommandAction, PlayerCommandBody};

    let sneak = PlayerCommandBody {
        entity_id: VarInt(1),
        action_id: PlayerCommandAction::StartSneaking,
        jump_boost: VarInt(0),
    };
    let jump = PlayerCommandBody {
        entity_id: VarInt(1),
        action_id: PlayerCommandAction::StartJumpWithHorse,
        jump_boost: VarInt(0),
    };

    for body in [&sneak, &jump] {
        let mut writer = Writer::new();
        body.encode(&mut writer, v()).expect("encodes");
        assert_eq!(
            writer.len(),
            3,
            "entity id, action, boost — three VarInts for {:?}",
            body.action_id
        );
        let back =
            PlayerCommandBody::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
        assert_eq!(&back, body);
    }

    // Two VarInts is what the old model wrote for a sneak, and it is short by
    // one — the decode runs off the end rather than succeeding with a byte to
    // spare, which is the failure vanilla reported.
    let mut short = Writer::new();
    VarInt(1).encode(&mut short, v()).expect("encodes");
    VarInt(0).encode(&mut short, v()).expect("encodes");
    assert!(
        PlayerCommandBody::decode(&mut Reader::new(short.as_bytes()), v()).is_err(),
        "two VarInts is not a player command"
    );

    // The *meaning* is still conditional, and that is where the distinction
    // lives now: a caller acting on a sneak's boost would be acting on a zero
    // the format demanded rather than on anything a player asked for.
    assert_eq!(sneak.meaningful_boost(), None);
    assert_eq!(jump.meaningful_boost(), Some(0));
}

// ---------------------------------------------------------------------------
// Sound positions: the unit the field does not carry in its name
// ---------------------------------------------------------------------------

#[test]
fn a_sound_position_is_eighths_of_a_block() {
    use dust_protocol::packets::play::sound::eighths;

    // Vanilla writes `(int)(x * 8.0)` and the client reads `x / 8.0`, so the
    // block centre a placed-block sound plays from is `8n + 4` and never `n`.
    // A caller that wrote the block coordinate straight into the field would
    // put the sound an eighth of the way to the origin — legal, quiet, and
    // wrong in a way no decoder can see.
    assert_eq!(eighths(0.5), 4);
    assert_eq!(eighths(64.5), 516);
    assert_eq!(eighths(-1.5), -12);

    // Truncating toward zero, as the cast does, so the two implementations
    // agree on the side of the origin a near-zero coordinate falls.
    assert_eq!(eighths(0.05), 0);
    assert_eq!(eighths(-0.05), 0);
    assert_eq!(eighths(-0.2), -1);
}

// ---------------------------------------------------------------------------
// The two smithing bodies, byte for byte
// ---------------------------------------------------------------------------

/// A smithing recipe's body starts with its template ingredient and **not with
/// a group string**.
///
/// `SmithingTransformRecipe`'s stream codec is four fields — template, base,
/// addition, result — and `SmithingTrimRecipe`'s is the first three. Every
/// other recipe in this packet carries a group; these two never did.
///
/// **A round trip cannot say this and neither can `mutation.rs`.** Both read
/// what this crate wrote, so a group written here and read back here agrees
/// with itself forever while a real client reads the recipe id that follows as
/// a stack's components and drops the connection. That is what happened, and
/// it is why this test spells the bytes out rather than encoding and decoding:
/// the assertion has to be about the layout, not about our agreement with
/// ourselves. `tools/bot/benches.js` is the check that has a second
/// implementation on the other side of it; this one is the guard that fails in
/// CI the day somebody adds the field back.
#[test]
fn a_smithing_body_opens_with_its_template_and_carries_no_group() {
    use dust_protocol::packets::play::containers::{
        Ingredient, SmithingTransformData, SmithingTrimData,
    };

    let one = |item_id| Ingredient {
        items: vec![simple_slot(item_id)],
    };
    // A present slot is count, item id, then an empty component patch as two
    // zeroes; an ingredient is a VarInt count and then that. So each of these
    // ingredients is exactly five bytes and the layout below is readable.
    let transform = SmithingTransformData {
        template: one(1),
        base: one(2),
        addition: one(3),
        result: simple_slot(4),
    };
    let mut writer = Writer::new();
    transform.encode(&mut writer, v()).expect("encodes");
    assert_eq!(
        writer.into_bytes(),
        vec![
            1, 1, 1, 0, 0, // template: one stack of item 1
            1, 1, 2, 0, 0, // base: one stack of item 2
            1, 1, 3, 0, 0, // addition: one stack of item 3
            1, 4, 0, 0, // result: one of item 4, no ingredient count in front
        ],
        "a leading zero here is an empty group, and the client would read the \
         recipe id after this body as a component patch"
    );

    let trim = SmithingTrimData {
        template: one(1),
        base: one(2),
        addition: one(3),
    };
    let mut writer = Writer::new();
    trim.encode(&mut writer, v()).expect("encodes");
    assert_eq!(
        writer.into_bytes(),
        vec![1, 1, 1, 0, 0, 1, 1, 2, 0, 0, 1, 1, 3, 0, 0],
        "a trim has no result either: the client derives it from the template"
    );
}
