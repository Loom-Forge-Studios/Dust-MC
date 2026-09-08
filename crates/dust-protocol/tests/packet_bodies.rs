//! The offline half of Phase 1's check: vectors, coverage, and round trips of
//! every defined packet.
//!
//! Needs no network and no JVM. The other half — whether these layouts are
//! what a real 1.21.1 server actually speaks — needs a live server, and
//! neither half can do the other's job. This file proves the code is
//! self-consistent and agrees with tables computed elsewhere; a live client
//! proves it agrees with Minecraft.
//!
//! The per-packet round trips themselves live in [`common::corpus`], because
//! the mutation loop and the coverage checks need the same frames this file
//! used to build privately. What stays here is what is about single field
//! types and about the suite's own guardrails.

mod common;

use common::{corpus, id, s, some_nbt, v};
use dust_protocol::conformance::{
    check_field_types, check_nbt, check_wire, in_crate_nbt, in_crate_wire,
};
use dust_protocol::nbt::{self, TextComponent};
use dust_protocol::packets::{unclaimed_for, undefined_for, COMPLETE_PAIRS};
use dust_protocol::types::{
    Angle, BitSet, BoundedString, ChatVisibility, Decode, Encode, FixedBitSet, Identifier,
    MainHand, NextState, Position, Slot,
};
use dust_protocol::wire::{DecodeError, Reader, WireRead, WireWrite, Writer};
use dust_protocol::{ConnectionState, Direction, ProtocolVersion};

#[test]
fn the_wire_primitives_agree_with_the_vectors() {
    // The same runner `dust-net` will call against its own implementation. If
    // this ever fails after that merge, the vectors say which of the two is
    // wrong, which is the thing checking them against each other could not do.
    let failures = check_wire(&in_crate_wire());
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn the_nbt_scanner_agrees_with_the_vectors() {
    let failures = check_nbt(in_crate_nbt());
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn the_field_types_agree_with_the_vectors() {
    let failures = check_field_types(v());
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---------------------------------------------------------------------------
// The traps
// ---------------------------------------------------------------------------

#[test]
fn a_string_is_measured_in_utf16_code_units_and_prefixed_in_bytes() {
    // The whole trap in one test. `café` is four UTF-16 units and five bytes.
    let text = "café";
    let mut writer = Writer::new();
    s::<4>(text).encode(&mut writer, v()).expect("fits at 4");
    assert_eq!(writer.as_bytes()[0], 5, "the prefix counts bytes");
    assert_eq!(writer.len(), 6);

    // A limit of four accepts it and a limit of three does not — which is the
    // assertion a byte-length implementation fails, because it would compare
    // five against three and reject, or compare five against five and accept
    // a string that is over the real limit.
    let bytes = writer.as_bytes().to_vec();
    assert!(BoundedString::<4>::decode(&mut Reader::new(&bytes), v()).is_ok());
    assert_eq!(
        BoundedString::<3>::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::StringTooLong {
            limit: 3,
            actual: 4
        })
    );

    // And the other direction: a 16-unit name of non-ASCII characters is 48
    // bytes, and a byte-length limit of 16 would refuse a name vanilla accepts.
    let long = "日".repeat(16);
    assert_eq!(dust_protocol::types::utf16_len(&long), 16);
    assert_eq!(long.len(), 48);
    assert!(BoundedString::<16>::new(&long).is_ok());
}

#[test]
fn a_hostile_length_prefix_is_refused_before_anything_is_allocated() {
    // A VarInt saying two billion, and four bytes of body. The refusal must
    // come from the limit and not from running out of input, because running
    // out of input is what happens *after* an implementation has tried to
    // reserve two gigabytes.
    let mut writer = Writer::new();
    writer.write_var_int(2_000_000_000);
    writer.write_slice(b"oops");
    let bytes = writer.into_bytes();
    assert!(matches!(
        BoundedString::<16>::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::StringTooLong { limit: 16, .. })
    ));
}

#[test]
fn a_position_sign_extends_and_negative_coordinates_survive() {
    // The failure this catches works perfectly around spawn. Every coordinate
    // here is one an ordinary player reaches in ten seconds of walking.
    for (x, y, z) in [
        (0, 0, 0),
        (-1, -1, -1),
        (-100, -60, 100),
        (100, -64, -100),
        (-33554432, -2048, 33554431),
    ] {
        let position = Position::new(x, y, z);
        assert!(position.is_representable(), "{position:?}");
        let mut writer = Writer::new();
        position.encode(&mut writer, v()).expect("encodes");
        let back = Position::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
        assert_eq!(back, position);
    }

    // A masking implementation that forgot to sign-extend produces this value,
    // so asserting it is *not* what comes back names the bug rather than just
    // failing.
    let masked_x = 0x3FF_FFFF - 99;
    let decoded = Position::decode(
        &mut Reader::new(&Position::new(-100, -60, 100).to_bits().to_be_bytes()),
        v(),
    )
    .expect("decodes");
    assert_ne!(decoded.x, masked_x);
    assert_eq!(decoded.x, -100);
}

#[test]
fn a_position_outside_the_range_wraps_rather_than_erroring() {
    // Documented behaviour, matching vanilla, and pinned here so a later change
    // to it is a deliberate one.
    let far = Position::new(1 << 26, 0, 0);
    assert!(!far.is_representable());
    assert_eq!(Position::from_bits(far.to_bits()).x, 0);
}

#[test]
fn an_angle_round_trips_as_steps_and_not_as_degrees() {
    // The lossy direction is stated rather than hidden: every step survives a
    // trip through degrees, and a degree value does not survive a trip through
    // a step.
    for raw in 0..=255u8 {
        let angle = Angle(raw);
        assert_eq!(Angle::from_degrees(angle.to_degrees()), angle, "{raw}");
    }
    assert_ne!(Angle::from_degrees(1.0).to_degrees(), 1.0);
    assert_eq!(Angle::from_degrees(360.0), Angle(0), "a full turn wraps");
    assert_eq!(Angle::from_degrees(-90.0), Angle(192), "and so does -90");
}

#[test]
fn an_unknown_enum_discriminant_is_a_named_error_and_never_a_default() {
    // Attacker-controlled input. A panic here is a remote crash and a silent
    // fallback to the first variant is worse, because the wrong value then
    // travels somewhere with no connection to the packet that produced it.
    let mut writer = Writer::new();
    writer.write_var_int(99);
    let bytes = writer.into_bytes();
    assert_eq!(
        NextState::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownVariant {
            name: "NextState",
            value: 99
        })
    );
    assert_eq!(
        ChatVisibility::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownVariant {
            name: "ChatVisibility",
            value: 99
        })
    );
    // A negative discriminant is the same story and is easy to miss, because a
    // VarInt happily carries one.
    let mut writer = Writer::new();
    writer.write_var_int(-1);
    assert!(matches!(
        MainHand::decode(&mut Reader::new(&writer.into_bytes()), v()),
        Err(DecodeError::UnknownVariant { value: -1, .. })
    ));
    for state in NextState::ALL {
        assert_eq!(
            NextState::from_discriminant(state.discriminant()),
            Some(*state)
        );
    }
}

#[test]
fn the_two_bit_sets_are_encoded_differently() {
    // Same idea, different formats, and conflating them is a stream that is a
    // few bytes out in a way nothing local notices.
    let mut growable = BitSet::default();
    growable.set(0, true);
    growable.set(70, true);
    let mut writer = Writer::new();
    growable.encode(&mut writer, v()).expect("encodes");
    // A VarInt count of two longs, then sixteen bytes.
    assert_eq!(writer.len(), 1 + 16);
    assert_eq!(
        BitSet::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        growable
    );
    assert!(growable.get(0) && growable.get(70) && !growable.get(1));

    let mut fixed = FixedBitSet::<7>::new();
    fixed.set(6, true);
    let mut writer = Writer::new();
    fixed.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.len(), 1, "seven bits is one byte and no prefix");
    assert_eq!(
        FixedBitSet::<7>::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        fixed
    );
}

#[test]
fn a_slot_with_components_is_refused_by_name_rather_than_guessed_at() {
    // The honest half-implementation. An empty stack and a stack with only
    // removals work; the moment a component appears, the answer is a named
    // refusal at the exact byte, because a component has no length and
    // guessing past one loses the position of everything after it.
    let empty = Slot::Empty;
    let mut writer = Writer::new();
    empty.encode(&mut writer, v()).expect("encodes");
    assert_eq!(writer.as_bytes(), &[0x00]);
    assert_eq!(
        Slot::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        empty
    );

    let stack = Slot::Present {
        count: 3,
        item_id: 1,
        components: dust_protocol::components::ComponentPatch::removing(&[7, 9]),
    };
    let mut writer = Writer::new();
    stack.encode(&mut writer, v()).expect("encodes");
    assert_eq!(
        Slot::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
        stack
    );

    // count 1, item 1, one component to add, none to remove.
    let with_component = [0x01, 0x01, 0x01, 0x00, 0x00];
    assert!(matches!(
        Slot::decode(&mut Reader::new(&with_component), v()),
        Err(DecodeError::Unsupported {
            field: "Slot components",
            ..
        })
    ));
}

#[test]
fn an_identifier_is_validated_and_a_bare_path_takes_the_default_namespace() {
    assert_eq!(id("minecraft:stone").to_string(), "minecraft:stone");
    assert_eq!(id("stone").namespace, "minecraft");
    assert_eq!(id("dust:some/path.thing-1").path, "some/path.thing-1");
    for bad in [
        "",
        ":",
        "Minecraft:stone",
        "minecraft:",
        "a b:c",
        "mine/craft:x",
    ] {
        assert!(
            Identifier::parse(bad).is_err(),
            "`{bad}` should not be an identifier"
        );
    }
}

#[test]
fn nbt_is_delimited_without_being_interpreted() {
    // The seam's whole contract: where does the value end.
    let bytes = some_nbt().0;
    assert_eq!(nbt::scan(&bytes), Ok(bytes.len()));

    // Inside a packet, followed by another field. A scanner that consumed the
    // rest of the body instead of the value would pass every test where NBT
    // was last, and this is the one where it is not.
    let mut writer = Writer::new();
    TextComponent(some_nbt())
        .encode(&mut writer, v())
        .expect("encodes");
    writer.write_var_int(1234);
    let bytes = writer.into_bytes();
    let mut reader = Reader::new(&bytes);
    let text = TextComponent::decode(&mut reader, v()).expect("decodes");
    assert_eq!(text.0, some_nbt());
    assert_eq!(reader.read_var_int(), Ok(1234));
    assert_eq!(reader.remaining(), 0);
}

#[test]
fn nbt_nesting_is_bounded_rather_than_overflowing_the_stack() {
    // Reachable from an unauthenticated socket, and a stack overflow in Rust
    // is an abort that no caller can catch.
    // Genuinely nested, rather than merely long: each level is a compound
    // entry with an empty name, so the repeating unit is tag, name length,
    // and nothing else.
    let levels = 600;
    let mut deep = vec![0x0a];
    for _ in 0..levels {
        deep.extend_from_slice(&[0x0a, 0x00, 0x00]);
    }
    deep.extend(std::iter::repeat_n(0x00, levels + 1));
    assert!(
        matches!(nbt::scan(&deep), Err(DecodeError::Nbt { .. })),
        "600 levels is past the limit of {}",
        nbt::MAX_DEPTH
    );

    // And the positive control, without which the assertion above would pass
    // for a scanner that refused everything.
    let mut shallow = vec![0x0a];
    for _ in 0..8 {
        shallow.extend_from_slice(&[0x0a, 0x00, 0x00]);
    }
    shallow.extend(std::iter::repeat_n(0x00, 9));
    assert_eq!(nbt::scan(&shallow), Ok(shallow.len()));
}

// ---------------------------------------------------------------------------
// Coverage: the anti-drift guard
// ---------------------------------------------------------------------------

#[test]
fn every_complete_pair_has_a_definition_and_every_definition_is_a_packet() {
    // This is the test that makes hand-written definitions defensible. The ids
    // are generated; the bodies are not; this is the only thing that connects
    // them, and it runs on every pull request forever.
    for protocol in ProtocolVersion::all() {
        let problems = undefined_for(protocol);
        assert!(problems.is_empty(), "{}: {problems:#?}", protocol.name());
    }
}

#[test]
fn the_complete_pairs_cover_exactly_the_forty_one_packets_before_play() {
    let mut counted = 0;
    for state in ConnectionState::ALL {
        for direction in Direction::ALL {
            if !COMPLETE_PAIRS.contains(&(state, direction)) {
                continue;
            }
            counted += v().table(state, direction).len();
        }
    }
    assert_eq!(counted, 41, "the four states before Play have 41 packets");
}

#[test]
fn play_is_partially_defined_and_marked_incomplete() {
    // Scope, asserted rather than assumed. Play grows family by family, and
    // while it grows its pairs stay outside COMPLETE_PAIRS — so nothing can
    // quietly mistake a long definition list for a finished state. When the
    // last packet lands, the pair graduates and `every_complete_pair...`
    // above becomes its guard from that moment.
    let incomplete: Vec<(ConnectionState, Direction)> = [
        (ConnectionState::Play, Direction::Clientbound),
        (ConnectionState::Play, Direction::Serverbound),
    ]
    .iter()
    .copied()
    .filter(|pair| !COMPLETE_PAIRS.contains(pair))
    .collect();
    assert_eq!(
        incomplete.len(),
        2,
        "both Play pairs are still being written"
    );

    // Every definition that does exist claims a version whose table really has
    // the packet — the typo direction of the guard, applied to the unfinished
    // pair too.
    let claimed: Vec<&str> = dust_protocol::packets::GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .filter(|meta| meta.state == ConnectionState::Play)
        .map(|meta| meta.name)
        .collect();
    let tabled: Vec<&str> = [
        (ConnectionState::Play, Direction::Clientbound),
        (ConnectionState::Play, Direction::Serverbound),
    ]
    .iter()
    .flat_map(|&(state, direction)| v().table(state, direction).packets().map(|(_, name)| name))
    .collect();
    for name in &claimed {
        assert!(
            tabled.contains(name),
            "{name} is defined but {:#?} has no such packet",
            incomplete
        );
    }
    assert!(!claimed.is_empty());

    // And the worklist is real: packets remain unclaimed, which is exactly
    // what an honest partial state looks like.
    let remaining = unclaimed_for(v());
    assert!(!remaining.is_empty(), "play still has packets to write");
    assert!(remaining.len() < 124 + 58, "and not all of them");
}

#[test]
fn the_corpus_covers_every_definition_exactly_once_per_name() {
    let frames = corpus();

    // Every defined packet appears at least once. A corpus that quietly
    // stopped covering a definition would keep every other test green while
    // that layout went unchecked — the failure mode where a guard degrades
    // instead of breaking.
    let defined: Vec<&str> = dust_protocol::packets::GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .map(|meta| meta.name)
        .collect();
    let covered: Vec<&str> = frames.iter().map(|frame| frame.name).collect();
    let mut missing: Vec<&str> = defined
        .iter()
        .filter(|name| !covered.contains(name))
        .copied()
        .collect();
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "definitions with no frame in the corpus: {missing:#?}"
    );
    assert_eq!(
        defined.len(),
        covered.len(),
        "one frame per definition keeps the count honest"
    );
}

// ---------------------------------------------------------------------------
// Frame-level decode policy
// ---------------------------------------------------------------------------

#[test]
fn a_body_that_ends_early_or_late_is_an_error_rather_than_a_shrug() {
    // Trailing bytes mean the layout this crate believes is not the layout
    // that was sent. Accepting them would mean the next packet on a shared
    // buffer starts in the wrong place, which is a much harder bug to find
    // than this.
    let frame = corpus()
        .into_iter()
        .find(|frame| {
            frame.name == "minecraft:ping_request" && frame.state == ConnectionState::Status
        })
        .expect("the status ping is in the corpus");

    let mut bytes = frame.bytes.clone();
    bytes.push(0xff);
    assert_eq!(
        dust_protocol::packets::status::serverbound::Packet::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::TrailingBytes { left: 1 })
    );

    bytes.truncate(bytes.len() - 3);
    assert!(matches!(
        dust_protocol::packets::status::serverbound::Packet::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnexpectedEnd { .. })
    ));
}

#[test]
fn an_id_this_state_has_no_packet_for_is_a_named_error() {
    let bytes = [0x7f];
    assert_eq!(
        dust_protocol::packets::status::serverbound::Packet::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownPacket {
            state: "status",
            direction: "serverbound",
            protocol_id: 127
        })
    );

    // And in Play, where the same rule matters more: an id past the end of a
    // 124-packet table is refused by naming the id, the state and the
    // direction, which is everything a log line needs.
    let bytes = [0xff, 0x01];
    assert!(matches!(
        dust_protocol::packets::play::clientbound::Packet::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::UnknownPacket {
            state: "play",
            protocol_id: 255,
            ..
        })
    ));
}

#[test]
fn a_packet_takes_its_id_from_the_generated_table_and_not_from_a_constant() {
    // The property that makes hand-written definitions survive a version bump:
    // nothing in a definition names a number, so a release that renumbers the
    // protocol changes nothing here.
    use dust_protocol::packets::PacketBody;
    assert_eq!(
        dust_protocol::packets::login::serverbound::LoginAcknowledged::protocol_id(v()),
        Some(3)
    );
    assert_eq!(
        dust_protocol::packets::play::clientbound::Login::protocol_id(v()),
        v().protocol_id(
            ConnectionState::Play,
            Direction::Clientbound,
            "minecraft:login"
        )
    );

    // Every definition resolves through its version's table, both directions
    // of the lookup agreeing.
    for meta in dust_protocol::packets::GROUPS.iter().copied().flatten() {
        let id = v()
            .protocol_id(meta.state, meta.direction, meta.name)
            .unwrap_or_else(|| panic!("{} does not resolve", meta.name));
        assert_eq!(
            v().packet_name(meta.state, meta.direction, id),
            Some(meta.name),
            "{meta:?} does not resolve back"
        );
    }

    let packet = dust_protocol::packets::status::serverbound::Packet::StatusRequest(
        dust_protocol::packets::status::serverbound::StatusRequest {},
    );
    let mut writer = Writer::new();
    assert!(packet.encode(&mut writer, v()).is_ok());
}

// ---------------------------------------------------------------------------
// The worklist's end state, pinned
// ---------------------------------------------------------------------------

#[test]
fn the_unclaimed_worklist_is_exactly_the_blocked_set() {
    use dust_protocol::packets::unclaimed_for;
    use dust_protocol::{version, ConnectionState as State, Direction};

    // Every packet left unclaimed must be one of these, with the blocker
    // that keeps it out. A definition added to Play either shrinks this list
    // or updates it on purpose — silently growing it is how a worklist stops
    // being one.
    let blocked: &[(State, Direction, &str)] = &[
        // The chat-signing wall: offline-first means no session keys, and
        // these packets exist to carry them. See `play::chat`.
        (State::Play, Direction::Clientbound, "minecraft:delete_chat"),
        // `minecraft:chat_command` used to be here and is now defined: 1.20.5
        // split the signing artifacts out into the packet below and left it a
        // bare string. See `play::mod`'s header for why that was a mistake in
        // the reasoning rather than a change of mind about signing.
        (
            State::Play,
            Direction::Serverbound,
            "minecraft:chat_command_signed",
        ),
        (
            State::Play,
            Direction::Serverbound,
            "minecraft:chat_session_update",
        ),
        // Development-only pair; neither half means anything alone.
        (
            State::Play,
            Direction::Clientbound,
            "minecraft:debug_sample",
        ),
        (
            State::Play,
            Direction::Serverbound,
            "minecraft:debug_sample_subscription",
        ),
    ];

    let mut actual = unclaimed_for(version::V1_21_1);
    let mut expected: Vec<_> = blocked.to_vec();
    actual.sort_by_key(|&(s, d, n)| (s, d, n));
    expected.sort_by_key(|&(s, d, n)| (s, d, n));

    assert_eq!(
        actual.len(),
        expected.len(),
        "the worklist moved: {actual:#?} — update this test deliberately"
    );
    for (got, want) in actual.iter().zip(expected.iter()) {
        assert_eq!(
            got, want,
            "the worklist moved — update this test deliberately"
        );
    }
}
