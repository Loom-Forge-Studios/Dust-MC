//! Helpers shared by the protocol test binaries.
//!
//! # Why the corpus is built once, here
//!
//! Every offline guarantee this crate makes is a statement about *every*
//! packet: coverage, round trips, hostile-input robustness. A statement about
//! everything is worthless if the list of everything lives in four files that
//! drift apart, so the list lives once, below, and each guarantee consumes
//! it. Building a frame also validates it — encode, decode back, insist the
//! value survived — so a corpus entry is a frame that has already passed its
//! round trip, which means the fuzz loop mutates real layouts rather than
//! accidents.
//!
//! Each test binary links this module on its own and uses a different slice
//! of it — the fuzzer wants the corpus, the body tests want the constructors
//! — so a helper unused in one binary is load-bearing in another, which is
//! why the whole module tolerates dead code rather than each item.
#![allow(dead_code)]

use dust_protocol::nbt::{JsonTextComponent, Nbt, TextComponent};
use dust_protocol::packets::common::{
    BuiltInLinkLabel, KnownPack, ProfileProperty, RegistryEntry, ReportDetail, ServerLink,
    ServerLinkLabel, Tag, TagRegistry,
};
use dust_protocol::packets::{configuration, handshake, login, status};
use dust_protocol::types::{
    BitSet, BoundedString, ChatVisibility, FixedBitSet, Identifier, MainHand, NextState, Position,
    PrefixedBytes, ResourcePackResult, RestOfPacket, Uuid, VarInt,
};
use dust_protocol::version;
use dust_protocol::wire::{DecodeError, Reader, Writer};
use dust_protocol::{ConnectionState, Direction, ProtocolVersion};

/// The one protocol version these tables describe.
pub fn v() -> ProtocolVersion {
    version::V1_21_1
}

/// A bounded string that fits, failing the test if the limit was miscounted.
pub fn s<const N: usize>(text: &str) -> BoundedString<N> {
    BoundedString::new(text).expect("fits")
}

/// An identifier, failing the test if it was malformed.
pub fn id(text: &str) -> Identifier {
    Identifier::parse(text).expect("valid")
}

/// An NBT compound, so component fields carry structure rather than the
/// one-byte empty value every scanner gets right.
pub fn some_nbt() -> Nbt {
    Nbt(vec![
        0x0a, 0x08, 0x00, 0x04, b't', b'e', b't', b'x', 0x00, 0x04, b'D', b'u', b's', b't', 0x00,
    ])
}

/// One encoded packet, plus what it takes to throw hostile input at it again.
///
/// Each test binary links this module independently and uses a different
/// subset of these fields — the fuzzer wants the bytes and the decoder, the
/// coverage checks want the names — so a field unused in one binary is load-
/// bearing in another.
#[allow(dead_code)]
#[derive(Clone)]
pub struct Frame {
    /// The packet's namespaced name, for coverage checks.
    pub name: &'static str,
    pub state: ConnectionState,
    pub direction: Direction,
    /// The full wire form: VarInt id, then body.
    pub bytes: Vec<u8>,
    /// Whether a full-frame decode of `bytes` succeeds. Almost always true;
    /// false marks an intentionally ambiguous sample, of which there are
    /// none today.
    pub decodes: fn(&[u8]) -> Result<(), DecodeError>,
}

macro_rules! frame {
    ($group:path, $state:ident, $direction:ident, $value:expr) => {{
        use $group as g;
        let packet: g::Packet = ($value).into();
        let name = packet.name();
        let mut writer = Writer::new();
        let protocol_id = packet.encode(&mut writer, v()).expect("encodes");
        let bytes = writer.into_bytes();
        let back = g::Packet::decode(&mut Reader::new(&bytes), v())
            .unwrap_or_else(|e| panic!("{name} (id {protocol_id}) did not decode: {e}"));
        assert_eq!(back, packet, "{name} changed on the way round");
        fn decodes(bytes: &[u8]) -> Result<(), DecodeError> {
            g::Packet::decode(&mut Reader::new(bytes), v()).map(drop)
        }
        Frame {
            name,
            state: ConnectionState::$state,
            direction: Direction::$direction,
            bytes,
            decodes,
        }
    }};
}

/// One value of every packet this crate defines.
///
/// Written out rather than derived, because a `Default` would fill every field
/// with a zero and a round trip over zeros is a weaker test than one over
/// values that differ from each other — a swapped pair of fields of the same
/// type is invisible when both are zero.
#[allow(clippy::too_many_lines)]
pub fn corpus() -> Vec<Frame> {
    let mut out = Vec::new();

    macro_rules! push {
        ($($t:tt)*) => {
            out.push(frame!($($t)*));
        };
    }

    push!(
        handshake::serverbound,
        Handshake,
        Serverbound,
        handshake::serverbound::Intention {
            protocol_version: VarInt(767),
            server_address: s("dust.example"),
            server_port: 25565,
            next_state: NextState::Login,
        }
    );

    push!(
        status::clientbound,
        Status,
        Clientbound,
        status::clientbound::StatusResponse {
            json: s(r#"{"description":"x"}"#),
        }
    );
    push!(
        status::clientbound,
        Status,
        Clientbound,
        status::clientbound::PongResponse { payload: -9 }
    );
    push!(
        status::serverbound,
        Status,
        Serverbound,
        status::serverbound::StatusRequest {}
    );
    push!(
        status::serverbound,
        Status,
        Serverbound,
        status::serverbound::PingRequest {
            payload: 81985529216486895
        }
    );

    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::LoginDisconnect {
            reason: JsonTextComponent(s(r#"{"text":"no"}"#)),
        }
    );
    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::Hello {
            server_id: s(""),
            public_key: PrefixedBytes(vec![1, 2, 3]),
            verify_token: PrefixedBytes(vec![4, 5, 6, 7]),
            should_authenticate: true,
        }
    );
    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::GameProfile {
            uuid: Uuid(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
            username: s("Notch"),
            properties: vec![
                ProfileProperty {
                    name: s("textures"),
                    value: s("base64"),
                    signature: Some(s("sig"))
                },
                ProfileProperty {
                    name: s("unsigned"),
                    value: s("value"),
                    signature: None
                },
            ],
            strict_error_handling: true,
        }
    );
    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::LoginCompression {
            threshold: VarInt(256)
        }
    );
    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::CustomQuery {
            message_id: VarInt(7),
            channel: id("fabric:hello"),
            data: RestOfPacket(vec![9, 9, 9]),
        }
    );
    push!(
        login::clientbound,
        Login,
        Clientbound,
        login::clientbound::CookieRequest {
            key: id("dust:session")
        }
    );

    push!(
        login::serverbound,
        Login,
        Serverbound,
        login::serverbound::Hello {
            name: s("Notch"),
            profile_id: Uuid(1),
        }
    );
    push!(
        login::serverbound,
        Login,
        Serverbound,
        login::serverbound::Key {
            shared_secret: PrefixedBytes(vec![1; 16]),
            verify_token: PrefixedBytes(vec![2; 4]),
        }
    );
    push!(
        login::serverbound,
        Login,
        Serverbound,
        login::serverbound::CustomQueryAnswer {
            message_id: VarInt(7),
            data: Some(RestOfPacket(vec![1, 2])),
        }
    );
    // The `data: None` half of this packet is exercised by the proptest
    // suite rather than the corpus, which keeps one frame per definition.
    push!(
        login::serverbound,
        Login,
        Serverbound,
        login::serverbound::LoginAcknowledged {}
    );
    push!(
        login::serverbound,
        Login,
        Serverbound,
        login::serverbound::CookieResponse {
            key: id("dust:session"),
            payload: Some(PrefixedBytes(vec![3, 3, 3])),
        }
    );

    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::CookieRequest { key: id("dust:c") }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::CustomPayload {
            channel: id("minecraft:brand"),
            data: RestOfPacket(b"\x04Dust".to_vec()),
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::Disconnect {
            reason: TextComponent(some_nbt()),
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::FinishConfiguration {}
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::KeepAlive { id: -1 }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::Ping { id: -2 }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::ResetChat {}
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::RegistryData {
            registry_id: id("minecraft:dimension_type"),
            entries: vec![
                RegistryEntry {
                    entry_id: id("minecraft:overworld"),
                    data: Some(some_nbt())
                },
                RegistryEntry {
                    entry_id: id("minecraft:the_nether"),
                    data: None
                },
            ],
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::ResourcePackPop {
            uuid: Some(Uuid(5))
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::ResourcePackPush {
            uuid: Uuid(6),
            url: s("https://example.invalid/p.zip"),
            hash: s("0123456789012345678901234567890123456789"),
            forced: true,
            prompt_message: Some(dust_protocol::nbt::TextComponent(some_nbt())),
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::StoreCookie {
            key: id("dust:c"),
            payload: PrefixedBytes(vec![1, 2, 3]),
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::Transfer {
            host: s("elsewhere.example"),
            port: VarInt(25565),
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::UpdateEnabledFeatures {
            features: vec![id("minecraft:vanilla")],
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::UpdateTags {
            registries: vec![TagRegistry {
                registry: id("minecraft:block"),
                tags: vec![Tag {
                    name: id("minecraft:logs"),
                    entries: vec![VarInt(1), VarInt(2), VarInt(3)]
                }],
            }],
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::SelectKnownPacks {
            packs: vec![KnownPack {
                namespace: s("minecraft"),
                id: s("core"),
                version: s("1.21.1")
            }],
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::CustomReportDetails {
            details: vec![ReportDetail {
                title: s("server"),
                description: s("Dust")
            }],
        }
    );
    push!(
        configuration::clientbound,
        Configuration,
        Clientbound,
        configuration::clientbound::ServerLinks {
            links: vec![
                ServerLink {
                    label: ServerLinkLabel::BuiltIn(BuiltInLinkLabel::BugReport),
                    url: s("https://example.invalid/bugs"),
                },
                ServerLink {
                    label: ServerLinkLabel::Custom(dust_protocol::nbt::TextComponent(some_nbt())),
                    url: s("https://example.invalid/other"),
                },
            ],
        }
    );

    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::ClientInformation {
            locale: s("en_GB"),
            view_distance: 12,
            chat_mode: ChatVisibility::System,
            chat_colors: true,
            displayed_skin_parts: 0b0111_1111,
            main_hand: MainHand::Left,
            text_filtering_enabled: false,
            allow_server_listings: true,
        }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::CookieResponse {
            key: id("dust:c"),
            payload: None,
        }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::CustomPayload {
            channel: id("minecraft:brand"),
            data: RestOfPacket(b"\x06vanilla".to_vec()),
        }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::FinishConfiguration {}
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::KeepAlive { id: 42 }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::Pong { id: 43 }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::ResourcePack {
            uuid: Uuid(6),
            result: ResourcePackResult::Declined,
        }
    );
    push!(
        configuration::serverbound,
        Configuration,
        Serverbound,
        configuration::serverbound::SelectKnownPacks { packs: vec![] }
    );

    play_frames(&mut out);
    out
}

#[allow(clippy::too_many_lines)]
fn play_frames(out: &mut Vec<Frame>) {
    use dust_protocol::packets::play::advancements::{
        Advancement, AdvancementDisplay, AdvancementProgress, AdvancementsBody, CriterionProgress,
        FrameType, NamedAdvancement, NamedProgress, SeenAdvancementsAction, SeenAdvancementsBody,
    };
    use dust_protocol::packets::play::boss_bar::{
        BossBarAction, BossBarColor, BossBarDivision, BossBarFlags, BossEventBody,
    };
    use dust_protocol::packets::play::chat::{
        AcknowledgedMessage, ChatFilter, MessageAcknowledgement, SignatureBytes,
    };
    use dust_protocol::packets::play::chunk::{BlockEntity, ChunkData, LightArray, LightData};
    use dust_protocol::packets::play::clientbound as cb;
    use dust_protocol::packets::play::commands::{
        CommandsBody, Node, NodeType, NumericRange, ParserProperties, SuggestionMatch,
    };
    use dust_protocol::packets::play::containers::{
        ChangedSlot, ClickType, CraftingBookCategory, EquipmentEntries, EquipmentEntry,
        EquipmentSlot, Ingredient, Recipe, RecipeKind, StonecuttingData,
    };
    use dust_protocol::packets::play::map_item::{MapDataBody, MapIcon, MapIconKind, MapPatch};
    use dust_protocol::packets::play::metadata::{MetadataEntries, MetadataValue};
    use dust_protocol::packets::play::particle::ParticleValue;
    use dust_protocol::packets::play::player_info::{
        ChatSession, PlayerInfoActions, PlayerInfoBody, PlayerInfoEntry, ProfileAddition,
    };
    use dust_protocol::packets::play::serverbound as sb;
    use dust_protocol::packets::play::sound::{SoundCategory, SoundId, StopSoundBody};
    use dust_protocol::packets::play::{
        Abilities, BlockChangeEntry, ChunkSectionPosition, DeathLocation, EntityDelta,
        EntityVelocity, GameModeByte, PreviousGameMode, TeleportFlags,
    };
    use dust_protocol::types::{ProtocolString, Slot, VarLong};

    let signature: SignatureBytes = core::array::from_fn(|i| (i * 7 + 3) as u8);

    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::AddEntity {
            entity_id: VarInt(17),
            uuid: Uuid(0x11),
            kind: VarInt(120),
            x: 8.5,
            y: -64.0,
            z: 4096.25,
            pitch: Angle(200),
            yaw: Angle(10),
            head_yaw: Angle(12),
            data: VarInt(-1),
            velocity: EntityVelocity {
                x: 100,
                y: -40,
                z: 8000
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::BlockUpdate {
            location: Position::new(-100, -60, 100),
            block_id: VarInt(4044),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Disconnect {
            reason: dust_protocol::text::Component::text("bye").bold(true),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::LevelChunkWithLight {
            chunk_x: -3,
            chunk_z: 19,
            heightmaps: some_nbt(),
            data: ChunkData(PrefixedBytes(vec![0xAA; 64])),
            block_entities: vec![
                BlockEntity {
                    packed_xz: 0x5A,
                    y: -20,
                    kind: VarInt(7),
                    data: some_nbt()
                },
                BlockEntity {
                    packed_xz: 0x01,
                    y: 90,
                    kind: VarInt(2),
                    data: Nbt::empty()
                },
            ],
            light: LightData {
                sky_mask: bitset(&[0, 5, 27]),
                block_mask: bitset(&[5]),
                empty_sky_mask: bitset(&[]),
                empty_block_mask: bitset(&[0, 1, 2, 3]),
                sky_arrays: vec![LightArray(vec![0xFF; 2048]); 3],
                block_arrays: vec![LightArray(vec![0x00; 2048])],
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::KeepAlive { id: i64::MIN + 7 }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Login {
            entity_id: 1,
            hardcore: true,
            dimensions: vec![id("minecraft:overworld"), id("dust:pocket")],
            max_players: VarInt(100),
            view_distance: VarInt(12),
            simulation_distance: VarInt(10),
            reduced_debug_info: false,
            respawn_screen: true,
            limited_crafting: false,
            dimension_type: VarInt(0),
            dimension_name: id("minecraft:overworld"),
            hashed_seed: 0x1234_5678_9abc_def0,
            game_mode: GameModeByte(dust_protocol::packets::play::Gamemode::Survival),
            previous_game_mode: PreviousGameMode(None),
            debug: false,
            flat: true,
            death_location: Some(DeathLocation {
                dimension: id("minecraft:the_end"),
                position: Position::new(99, 2047, -99),
            }),
            portal_cooldown: VarInt(40),
            secure_chat: false,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::MoveEntityPosRot {
            entity_id: VarInt(17),
            delta: EntityDelta {
                x: -300,
                y: 5,
                z: 300
            },
            yaw: Angle(255),
            pitch: Angle(1),
            on_ground: true,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::MoveEntityPos {
            entity_id: VarInt(18),
            delta: EntityDelta {
                x: 1,
                y: -1,
                z: 4095
            },
            on_ground: false,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::MoveEntityRot {
            entity_id: VarInt(19),
            yaw: Angle(128),
            pitch: Angle(254),
            on_ground: true,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerChatMessage {
            sender: Uuid(0x22),
            index: VarInt(4),
            signature: Some(signature),
            message: s::<256>("hello there"),
            timestamp: 1_700_000_000_000,
            salt: -55,
            previous_messages: vec![
                AcknowledgedMessage {
                    id: VarInt(3),
                    signature: None
                },
                AcknowledgedMessage {
                    id: VarInt(0),
                    signature: Some(signature)
                },
            ],
            unsigned_content: None,
            filter: ChatFilter::PartiallyFiltered(bitset(&[2, 9])),
            chat_type: VarInt(1),
            network_name: dust_protocol::text::Component::translate("chat.type.text", None)
                .italic(false),
            network_target_name: Some(dust_protocol::text::Component::text("<notch>")),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerAbilities {
            flags: Abilities(Abilities::FLYING | Abilities::ALLOW_FLYING),
            flying_speed: 0.05,
            fov_modifier: 0.1,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerPosition {
            x: 1.5,
            y: -59.99,
            z: 2.5,
            yaw: 179.5,
            pitch: -12.25,
            flags: TeleportFlags(TeleportFlags::X | TeleportFlags::Z | TeleportFlags::YAW),
            teleport_id: VarInt(77),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerInfoUpdate {
            body: PlayerInfoBody {
                actions: PlayerInfoActions(
                    PlayerInfoActions::ADD_PLAYER
                        | PlayerInfoActions::INITIALIZE_CHAT
                        | PlayerInfoActions::UPDATE_LISTED
                        | PlayerInfoActions::UPDATE_LATENCY,
                ),
                entries: vec![
                    PlayerInfoEntry {
                        uuid: Uuid(0x33),
                        profile: Some(ProfileAddition {
                            name: s("Notch"),
                            properties: vec![ProfileProperty {
                                name: s("textures"),
                                value: s("base64"),
                                signature: None,
                            }],
                        }),
                        chat_session: Some(None),
                        game_mode: None,
                        listed: Some(true),
                        latency: Some(VarInt(-1)),
                        display_name: None,
                    },
                    PlayerInfoEntry {
                        uuid: Uuid(0x34),
                        profile: Some(ProfileAddition {
                            name: s("Dinnerbone"),
                            properties: vec![]
                        }),
                        chat_session: Some(Some(ChatSession {
                            session_id: Uuid(0x35),
                            expires_at: 1_800_000_000_000,
                            public_key: PrefixedBytes(vec![9; 494]),
                            key_signature: PrefixedBytes(vec![8; 512]),
                        })),
                        game_mode: None,
                        listed: Some(false),
                        latency: Some(VarInt(42)),
                        display_name: None,
                    },
                ],
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerInfoRemove {
            uuids: vec![Uuid(0x33), Uuid(0x34)],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::CustomPayload {
            channel: id("minecraft:brand"),
            data: RestOfPacket(b"\x04Dust".to_vec()),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::RemoveEntities {
            entity_ids: vec![VarInt(1), VarInt(2), VarInt(3), VarInt(-9)],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::RotateHead {
            entity_id: VarInt(17),
            head_yaw: Angle(64),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SectionBlocksUpdate {
            section: ChunkSectionPosition::pack(-4, 3, 12),
            entries: vec![
                BlockChangeEntry::pack(1, 15, 0, 8),
                BlockChangeEntry::pack(9000, 3, 15, 2),
            ],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetCarriedItem { slot: 8 }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetEntityData {
            entity_id: VarInt(17),
            entries: MetadataEntries(vec![
                dust_protocol::packets::play::metadata::MetadataEntry {
                    index: 0,
                    value: MetadataValue::Byte(0x01),
                },
                dust_protocol::packets::play::metadata::MetadataEntry {
                    index: 6,
                    value: MetadataValue::VarInt(VarInt(300)),
                },
                dust_protocol::packets::play::metadata::MetadataEntry {
                    index: 9,
                    value: MetadataValue::TextComponent(
                        dust_protocol::text::Component::text("custom name").colored(
                            dust_protocol::text::Color::Named(
                                dust_protocol::text::NamedColor::Gold
                            ),
                        ),
                    ),
                },
            ]),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SystemChat {
            content: dust_protocol::text::Component::text("welcome").italic(true),
            overlay: false,
        }
    ));
    out.push(frame!(cb, Play, Clientbound, cb::Ping { id: -3 }));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PongResponse { payload: 987654321 }
    ));

    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::TeleportConfirm {
            teleport_id: VarInt(77)
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::Chat {
            message: s::<256>("a message"),
            timestamp: 1_700_000_000_000,
            salt: 12,
            signature: Some(signature),
            acknowledgement: MessageAcknowledgement {
                offset: VarInt(30),
                acknowledged: fixed_bits(),
            },
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::ChatCommand {
            command: s("time set day"),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::CustomPayload {
            channel: id("dust:hello"),
            data: RestOfPacket(vec![1, 2, 3]),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::KeepAlive { id: i64::MIN + 7 }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::MovePlayerPos {
            x: -30_000_000.0,
            y: -64.0,
            z: 29_999_999.75,
            on_ground: true,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::MovePlayerPosRot {
            x: 0.125,
            y: 320.0,
            z: 0.25,
            yaw: -359.5,
            pitch: 89.75,
            on_ground: false,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::MovePlayerRot {
            yaw: 359.5,
            pitch: -89.75,
            on_ground: true,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::MovePlayerStatusOnly { on_ground: true }
    ));
    out.push(frame!(sb, Play, Serverbound, sb::Pong { id: i32::MIN }));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::PlayerAbilities {
            flags: Abilities(Abilities::FLYING),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::PingRequest { payload: -2 }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::SetCarriedItem { slot: 5 }
    ));

    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::BossEvent {
            body: BossEventBody {
                uuid: Uuid(0xB055),
                action: BossBarAction::Add {
                    title: dust_protocol::text::Component::text("Ender Dragon").colored(
                        dust_protocol::text::Color::Named(
                            dust_protocol::text::NamedColor::DarkPurple,
                        )
                    ),
                    health: 0.75,
                    color: BossBarColor::Purple,
                    division: BossBarDivision::Notches10,
                    flags: BossBarFlags(BossBarFlags::DARKEN_SKY | BossBarFlags::DRAGON_BAR),
                },
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Commands {
            body: CommandsBody {
                nodes: vec![
                    Node::literal(NodeType::Root, None),
                    {
                        let mut greet = Node::literal(NodeType::Literal, Some("greet"));
                        greet.executable = true;
                        greet.children = vec![VarInt(2)];
                        greet
                    },
                    {
                        let mut speed = Node::literal(NodeType::Argument, Some("speed"));
                        speed.parser = Some((
                            3,
                            Some(ParserProperties::Integer(NumericRange {
                                min: Some(1),
                                max: None,
                            })),
                        ));
                        speed.suggestions = Some(id("minecraft:ask_server"));
                        speed
                    },
                ],
                root_index: VarInt(0),
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::UpdateAdvancements {
            body: AdvancementsBody {
                reset: true,
                added: vec![NamedAdvancement {
                    key: id("minecraft:story/root"),
                    value: Advancement {
                        parent: None,
                        display: Some(AdvancementDisplay {
                            title: dust_protocol::nbt::TextComponent(some_nbt()),
                            description: dust_protocol::nbt::TextComponent(some_nbt()),
                            icon: Slot::Present {
                                count: 1,
                                item_id: 42,
                                components: dust_protocol::components::ComponentPatch::removing(&[
                                    7
                                ]),
                            },
                            frame: FrameType::Task,
                            flags: AdvancementDisplay::SHOW_TOAST,
                            background: None,
                            x: 0.5,
                            y: -12.25,
                        }),
                        criteria: vec![id("minecraft:mine_stone")],
                        requirements: vec![vec![
                            ProtocolString::new("minecraft:mined").expect("fits")
                        ]],
                        sends_telemetry: false,
                    },
                }],
                removed: vec![id("minecraft:old/path")],
                progress: vec![NamedProgress {
                    key: id("minecraft:story/root"),
                    value: AdvancementProgress {
                        key: id("minecraft:story/root"),
                        criteria: vec![CriterionProgress {
                            identifier: id("minecraft:mine_stone"),
                            achieved_at: Some(1_700_000_000_000),
                        }],
                    },
                }],
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SelectAdvancementsTab {
            tab: Some(id("minecraft:story/root"))
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetEquipment {
            entity_id: VarInt(17),
            entries: EquipmentEntries(vec![
                EquipmentEntry {
                    slot: EquipmentSlot::MainHand,
                    item: Slot::Empty,
                },
                EquipmentEntry {
                    slot: EquipmentSlot::Helmet,
                    item: Slot::Present {
                        count: 1,
                        item_id: 9,
                        components: dust_protocol::components::ComponentPatch::EMPTY,
                    },
                },
            ]),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::LevelParticles {
            long_distance: true,
            x: -0.5,
            y: 64.125,
            z: 4096.0,
            offset_x: 0.25,
            offset_y: 1.0,
            offset_z: -0.25,
            max_speed: 0.1,
            count: 30,
            particle: ParticleValue::Dust {
                id: 13,
                red: 0.8,
                green: 0.2,
                blue: 0.1,
                scale: 1.5,
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::MapItemData {
            map_id: VarInt(4),
            data: MapDataBody {
                scale: 2,
                locked: true,
                icons: Some(vec![MapIcon {
                    kind: MapIconKind::Mansion,
                    x: -64,
                    z: 96,
                    direction: 8,
                    display_name: Some(dust_protocol::text::Component::text("woodland mansion")),
                }]),
                patch: MapPatch {
                    columns: 2,
                    rows: 3,
                    x: 10,
                    z: 20,
                    data: vec![0xA5; 6],
                },
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Sound {
            sound: SoundId::Id(VarInt(90)),
            category: SoundCategory::Hostile,
            position_x: -512,
            position_y: -32,
            position_z: 2048,
            volume: 0.85,
            pitch: 1.25,
            seed: 1234567890,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SoundEntity {
            sound: SoundId::Inline {
                name: id("dust:custom_sound"),
                fixed_range: Some(24.0),
            },
            category: SoundCategory::Voice,
            entity_id: VarInt(-7),
            volume: 2.0,
            pitch: 0.5,
            seed: -987654321,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::StopSound {
            body: StopSoundBody {
                source: Some(SoundCategory::Music),
                name: Some(id("minecraft:music.game")),
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::UpdateRecipes {
            recipes: vec![
                Recipe {
                    id: id("minecraft:oak_planks"),
                    kind: RecipeKind::Stonecutting(StonecuttingData {
                        group: ProtocolString::new("planks").expect("fits"),
                        ingredient: Ingredient {
                            items: vec![Slot::Present {
                                count: 1,
                                item_id: 15,
                                components: dust_protocol::components::ComponentPatch::EMPTY,
                            }],
                        },
                        result: Slot::Present {
                            count: 1,
                            item_id: 16,
                            components: dust_protocol::components::ComponentPatch::EMPTY,
                        },
                    }),
                },
                Recipe {
                    id: id("minecraft:crafting/special/armor_dye"),
                    kind: RecipeKind::Special {
                        type_id: 2,
                        category: CraftingBookCategory::Equipment,
                    },
                },
                Recipe {
                    id: id("minecraft:baked_potato"),
                    kind: RecipeKind::Cooking {
                        type_id: 15,
                        data: dust_protocol::packets::play::containers::CookingData {
                            group: ProtocolString::new("food").expect("fits"),
                            category: CookingBookCategory::Food,
                            ingredient: Ingredient {
                                items: vec![Slot::Present {
                                    count: 1,
                                    item_id: 144,
                                    components: dust_protocol::components::ComponentPatch::EMPTY,
                                }],
                            },
                            result: Slot::Present {
                                count: 1,
                                item_id: 145,
                                components: dust_protocol::components::ComponentPatch::EMPTY,
                            },
                            experience: 0.35,
                            cooking_time: VarInt(100),
                        },
                    },
                },
            ],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::CommandSuggestions {
            id: VarInt(9),
            start: VarInt(2),
            length: VarInt(4),
            matches: vec![
                SuggestionMatch {
                    text: ProtocolString::new("give").expect("fits"),
                    tooltip: None,
                },
                SuggestionMatch {
                    text: ProtocolString::new("gamerule").expect("fits"),
                    tooltip: Some(dust_protocol::text::Component::text("the rules").italic(true)),
                },
            ],
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::ClickContainer {
            window_id: 2,
            state_id: VarInt(11),
            slot: 36,
            button: 40,
            mode: ClickType::Swap,
            changed_slots: vec![
                ChangedSlot {
                    number: 0,
                    item: Slot::Empty,
                },
                ChangedSlot {
                    number: 45,
                    item: Slot::Present {
                        count: 16,
                        item_id: 33,
                        components: dust_protocol::components::ComponentPatch::removing(&[1, 2]),
                    },
                },
            ],
            cursor_item: Slot::Empty,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::SeenAdvancements {
            body: SeenAdvancementsBody {
                action: SeenAdvancementsAction::OpenedTab,
                tab: Some(id("minecraft:recipes/root")),
            },
        }
    ));

    // ---- wave four: the serverbound families ----
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::QueryBlockNbt {
            transaction_id: VarInt(6),
            location: Position::new(-300, 100, 300),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::ChangeDifficulty {
            difficulty: DifficultyByte(Difficulty::Normal),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::AcknowledgeMessage { offset: VarInt(-2) }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::ChunkBatchReceived {
            chunks_per_tick: 3.75,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::ClientCommand {
            action: dust_protocol::packets::play::serverbound::ClientStatusAction::PerformRespawn,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::CommandSuggestionsRequest {
            transaction_id: VarInt(9),
            text: s::<32_500>("gamerule keepInv"),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::AcknowledgeConfiguration {}
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::ClickContainerButton {
            window_id: VarInt(2),
            button_id: VarInt(1),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::CloseContainer { window_id: 0 }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::SlotChangedState {
            slot_id: VarInt(4),
            screen_handler_id: VarInt(1),
            new_state: true,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::CookieResponse {
            key: id("dust:session"),
            payload: Some(PrefixedBytes(vec![9; 16])),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::EditBook {
            slot: VarInt(0),
            pages: vec![s::<8192>("once upon a time"), s::<8192>("the end")],
            title: Some(s::<128>("A Dusty Book")),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::QueryEntityNbt {
            transaction_id: VarInt(7),
            entity_id: VarInt(-11),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::InteractEntity {
            entity_id: VarInt(88),
            kind: dust_protocol::packets::play::serverbound::InteractionKind::Attack,
            sneaking: true,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::JigsawGenerate {
            location: Position::new(1, -12, -1),
            levels: VarInt(6),
            keep_jigsaws: false,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::LockDifficulty { locked: true }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::MoveVehicle {
            x: -0.25,
            y: 63.5,
            z: 0.75,
            yaw: 270.0,
            pitch: 0.125,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::PaddleBoat {
            left_paddle: true,
            right_paddle: false,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::PickItem { slot: VarInt(4) }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::PlaceRecipe {
            window_id: 1,
            recipe: id("minecraft:oak_planks"),
            craft_all: false,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::PlayerAction {
            status: dust_protocol::packets::play::serverbound::PlayerActionKind::FinishDigging,
            location: Position::new(10, 62, -10),
            face: 1,
            sequence: VarInt(77),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::PlayerCommand {
            body: dust_protocol::packets::play::serverbound::PlayerCommandBody {
                entity_id: VarInt(1),
                action_id: dust_protocol::packets::play::serverbound::PlayerCommandAction::StartJumpWithHorse,
                jump_boost: VarInt(42),
            },
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::PlayerInput {
            sideways: -0.5,
            forward: 1.0,
            flags: dust_protocol::packets::play::serverbound::InputFlags(
                dust_protocol::packets::play::serverbound::InputFlags::SNEAK,
            ),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::RecipeBookChangeSettings {
            book_category: dust_protocol::packets::play::containers::RecipeBookType::Furnace,
            gui_open: true,
            filtering_craftable: false,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::SeenRecipe {
            recipe: id("minecraft:bread"),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::RenameItem {
            item_name: s::<32_767>("Sharpness V, Sweeping III"),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::ResourcePackResponse {
            uuid: Uuid(6),
            result: dust_protocol::types::ResourcePackResult::Accepted,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::SelectTrade {
            selected_slot: VarInt(2)
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::UpdateBeacon {
            primary: Some(VarInt(1)),
            secondary: Some(VarInt(12)),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::UpdateCommandBlock {
            location: Position::new(0, 90, 0),
            command: s::<32_767>("say hi"),
            mode: dust_protocol::packets::play::serverbound::CommandBlockMode::Auto,
            flags: dust_protocol::packets::play::serverbound::CommandBlockFlags(
                dust_protocol::packets::play::serverbound::CommandBlockFlags::ALWAYS_ACTIVE,
            ),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::UpdateCommandBlockMinecart {
            entity_id: VarInt(400),
            command: s::<32_767>("effect give @p speed"),
            track_output: false,
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::SetCreativeModeSlot {
            slot: 45,
            item: Slot::Present {
                count: 64,
                item_id: 7,
                components: dust_protocol::components::ComponentPatch::EMPTY,
            },
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::UpdateJigsaw {
            location: Position::new(-5, 20, 5),
            name: id("minecraft:village/plains/houses"),
            target: id("minecraft:village/plains/terminators"),
            pool: id("minecraft:village/plains"),
            final_state: s::<32_767>("minecraft:oak_planks"),
            joint_type: s::<32_767>("roll"),
            selection_priority: VarInt(1),
            placement_priority: VarInt(0),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::UpdateStructureBlock {
            location: Position::new(3, 33, -3),
            action: dust_protocol::packets::play::serverbound::StructureBlockAction::Load,
            mode: dust_protocol::packets::play::serverbound::StructureBlockMode::Save,
            template_name: s::<32_767>("dust:ship"),
            offset_x: -2,
            offset_y: 1,
            offset_z: 0,
            size_x: 10,
            size_y: 20,
            size_z: 15,
            mirror: dust_protocol::packets::play::serverbound::StructureBlockMirror::FrontBack,
            rotation:
                dust_protocol::packets::play::serverbound::StructureBlockRotation::Clockwise90,
            metadata: s::<32_767>(""),
            integrity: 0.5,
            seed: dust_protocol::types::VarLong(-4_242_424_242),
            flags: dust_protocol::packets::play::serverbound::StructureBlockFlags(
                dust_protocol::packets::play::serverbound::StructureBlockFlags::SHOW_BOUNDING_BOX,
            ),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::UpdateSign {
            location: Position::new(-40, 70, 40),
            is_front_text: true,
            lines: [
                s::<8192>("line one"),
                s::<8192>("two"),
                s::<8192>(""),
                s::<8192>("four"),
            ],
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::SwingArm { hand: Hand::Main }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::SpectateTeleport {
            target: Uuid(0xFEED)
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::UseItemOnBlock {
            hand: Hand::Main,
            hit: dust_protocol::packets::play::serverbound::BlockHit {
                location: Position::new(15, 80, -15),
                face: 3,
                cursor_x: 0.25,
                cursor_y: 0.5,
                cursor_z: 0.0,
                inside_block: false,
            },
            sequence: VarInt(78),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::UseItem {
            hand: Hand::Off,
            sequence: VarInt(79),
            yaw: -170.0,
            pitch: 45.0,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ExperienceOrbSpawn {
            entity_id: VarInt(55),
            x: -0.5,
            y: 32.5,
            z: 100.5,
            experience: 3,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::StoreCookie {
            key: id("dust:carried"),
            payload: PrefixedBytes(vec![7; 8]),
        }
    ));
    out.push(frame!(
        sb,
        Play,
        Serverbound,
        sb::ClientInformation {
            locale: s("en_GB"),
            view_distance: 10,
            chat_mode: dust_protocol::types::ChatVisibility::Full,
            chat_colors: true,
            displayed_skin_parts: 0b0111_1111,
            main_hand: dust_protocol::types::MainHand::Right,
            text_filtering_enabled: false,
            allow_server_listings: true,
        }
    ));

    // ---- wave three: the remaining clientbound families ----
    use dust_protocol::packets::play::attributes::{AttributeModifier, AttributeProperty};
    use dust_protocol::packets::play::containers::StatisticEntry;
    use dust_protocol::packets::play::containers::{
        CookingBookCategory, MerchantOffersBody, RecipeBookAction, RecipeBookBody,
        RecipeBookSettings, TradeItem, TradeOffer,
    };
    use dust_protocol::packets::play::scoreboard::{
        CollisionRule, NameTagVisibility, ObjectiveMode, ObjectiveRenderType, ScoreboardSlot,
        TeamBody, TeamInfo, TeamMethod,
    };
    use dust_protocol::packets::play::{
        Anchor, DamageSourcePosition, Difficulty, DifficultyByte, EffectFlags, EntityLinkKind,
        ExplosionInteraction, ExplosionRecord, Gamemode, Hand, LookAtTarget, OffsetEntityId,
        RespawnFlags,
    };
    use dust_protocol::types::Angle;

    out.push(frame!(cb, Play, Clientbound, cb::BundleDelimiter {}));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Animate {
            entity_id: VarInt(31),
            animation: 0,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::AwardStats {
            statistics: vec![
                StatisticEntry {
                    category: VarInt(1),
                    statistic: VarInt(42),
                    value: VarInt(9001),
                },
                StatisticEntry {
                    category: VarInt(2),
                    statistic: VarInt(-5),
                    value: VarInt(0),
                },
            ],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::BlockChangedAck {
            sequence: VarInt(404)
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::BlockDestruction {
            entity_id: VarInt(88),
            location: Position::new(64, -51, 1000),
            destroy_stage: 7,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::BlockEntityData {
            location: Position::new(-9, 128, 9),
            kind: VarInt(13),
            data: some_nbt(),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::BlockEvent {
            location: Position::new(0, 320, -1),
            action_id: 1,
            action_parameter: 15,
            block_type: VarInt(220),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ChangeDifficulty {
            difficulty: DifficultyByte(Difficulty::Hard),
            locked: true,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ChunkBatchFinished {
            batch_size: VarInt(17)
        }
    ));
    out.push(frame!(cb, Play, Clientbound, cb::ChunkBatchStart {}));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ChunksBiomes {
            chunks: vec![dust_protocol::packets::play::chunk::ChunkBiomesEntry {
                chunk_z: -7,
                chunk_x: 33,
                data: PrefixedBytes(vec![0x11; 24]),
            }],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ClearTitles { reset: true }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ContainerClose { window_id: 12 }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ContainerSetData {
            window_id: 1,
            property: 3,
            value: 200,
        }
    ));
    // Content carries the whole window plus the cursor: an empty slot, a plain
    // stack and one with removals, because those are the three shapes `Slot`
    // has and a frame with only the middle one would not exercise the other
    // two.
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ContainerSetContent {
            window_id: 0,
            state_id: VarInt(7),
            slots: vec![
                Slot::Empty,
                Slot::Present {
                    count: 64,
                    item_id: 1,
                    components: dust_protocol::components::ComponentPatch::EMPTY,
                },
                Slot::Present {
                    count: 1,
                    item_id: 856,
                    components: dust_protocol::components::ComponentPatch::removing(&[3, 9]),
                },
            ],
            carried_item: Slot::Empty,
        }
    ));
    // A negative window id, because it is the field this packet gets wrong if
    // it is widened: -1 is the cursor.
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ContainerSetSlot {
            window_id: -1,
            state_id: VarInt(8),
            slot: 36,
            item: Slot::Present {
                count: 17,
                item_id: 14,
                components: dust_protocol::components::ComponentPatch::EMPTY,
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::CookieRequest {
            key: id("dust:session")
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Cooldown {
            item_id: VarInt(88),
            cooldown_ticks: VarInt(25),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::CustomChatCompletions {
            action: dust_protocol::packets::play::chat::ChatCompletionsAction::Add,
            entries: vec![
                ProtocolString::new("Notch").expect("fits"),
                ProtocolString::new("Dinnerbone").expect("fits"),
            ],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::DamageEvent {
            entity_id: VarInt(19),
            source_type: VarInt(37),
            source_cause: OffsetEntityId(Some(4)),
            source_direct: OffsetEntityId(None),
            source_position: Some(DamageSourcePosition {
                x: 8.5,
                y: -60.25,
                z: 512.125,
            }),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::DisguisedChat {
            message: dust_protocol::text::Component::text("[Server] hello"),
            chat_type: VarInt(6),
            sender_name: dust_protocol::text::Component::text("Console"),
            target_name: None,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::EntityEvent {
            entity_id: -40,
            status: 7,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Explode {
            x: -16.5,
            y: 65.0,
            z: 999.75,
            radius: 4.0,
            records: vec![
                ExplosionRecord { x: 1, y: 0, z: -1 },
                ExplosionRecord { x: 0, y: 2, z: 0 },
            ],
            player_motion_x: 0.5,
            player_motion_y: -0.25,
            player_motion_z: 1.5,
            block_interaction: ExplosionInteraction::DestroyWithDecay,
            small_particle: ParticleValue::None { id: 21 },
            large_particle: ParticleValue::BlockState {
                id: 1,
                state: VarInt(77),
            },
            sound: SoundId::Id(VarInt(14)),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ForgetLevelChunk {
            chunk_z: -30000,
            chunk_x: 29999,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::GameEvent {
            event: 7,
            value: 0.75,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::HorseScreenOpen {
            window_id: 20,
            slot_count: VarInt(17),
            entity_id: 123456,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::HurtAnimation {
            entity_id: VarInt(19),
            yaw: 137.5,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::InitializeBorder {
            center_x: 0.5,
            center_z: -0.5,
            old_diameter: 59_999_968.0,
            new_diameter: 10_000.0,
            lerp_speed: VarLong(-1_000),
            portal_boundary: VarInt(29_999_984),
            warning_blocks: VarInt(5),
            warning_time: VarInt(15),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::LevelEvent {
            event: 1010,
            position: Position::new(-1, 64, 1),
            data: VarInt(1129).0,
            global: false,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::LightUpdate {
            chunk_x: VarInt(-3),
            chunk_z: VarInt(19),
            light: LightData {
                sky_mask: bitset(&[0]),
                block_mask: bitset(&[1, 5]),
                empty_sky_mask: bitset(&[]),
                empty_block_mask: bitset(&[2]),
                sky_arrays: vec![],
                block_arrays: vec![LightArray(
                    vec![0x0F; dust_protocol::packets::play::chunk::LIGHT_SECTION_BYTES]
                )],
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::MerchantOffers {
            window_id: VarInt(9),
            body: MerchantOffersBody {
                offers: vec![TradeOffer::simple(
                    TradeItem {
                        item_id: VarInt(15),
                        count: VarInt(20),
                    },
                    Slot::Present {
                        count: 1,
                        item_id: 16,
                        components: dust_protocol::components::ComponentPatch::removing(&[3]),
                    },
                )],
                villager_level: VarInt(3),
                experience: VarInt(10),
                regular_villager: true,
                can_restock: true,
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::MoveVehicle {
            x: 0.0,
            y: -64.0,
            z: 128.0,
            yaw: 90.0,
            pitch: -20.0,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::OpenBook { hand: Hand::Off }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::OpenScreen {
            window_id: 3,
            menu_kind: VarInt(14),
            title: dust_protocol::text::Component::text("Chest").bold(true),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::OpenSignEditor {
            location: Position::new(5, -50, -5),
            is_front_text: false,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlaceGhostRecipe {
            window_id: 3,
            recipe: id("minecraft:oak_planks"),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerCombatEnd {
            duration_in_ticks: VarInt(240)
        }
    ));
    out.push(frame!(cb, Play, Clientbound, cb::PlayerCombatEnter {}));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerCombatKill {
            player_id: VarInt(1),
            killer_id: VarInt(999),
            message: dust_protocol::text::Component::translate("death.fell.accident", None),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::PlayerLookAt {
            anchor: Anchor::Eyes,
            x: 0.5,
            y: 70.0,
            z: 0.5,
            target: Some(LookAtTarget {
                entity_id: VarInt(17),
                anchor: Anchor::Feet,
            }),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::RecipeBookUnlock {
            body: RecipeBookBody {
                action: RecipeBookAction::Init,
                settings: RecipeBookSettings {
                    crafting_open: true,
                    crafting_filter: false,
                    ..RecipeBookSettings::default()
                },
                changed: vec![id("minecraft:oak_planks"), id("minecraft:stick")],
                highlighted: Some(vec![id("minecraft:furnace")]),
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::RemoveMobEffect {
            entity_id: VarInt(22),
            effect: VarInt(10),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ResetScore {
            entity_name: ProtocolString::new("Notch").expect("fits"),
            objective: None,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ResourcePackPop {
            uuid: Some(Uuid(5))
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ResourcePackPush {
            uuid: Uuid(6),
            url: s("https://example.invalid/p.zip"),
            hash: s("0123456789012345678901234567890123456789"),
            forced: true,
            prompt_message: Some(dust_protocol::nbt::TextComponent(some_nbt())),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Respawn {
            dimension_type: VarInt(2),
            dimension_name: id("minecraft:the_end"),
            hashed_seed: i64::MAX - 3,
            game_mode: GameModeByte(dust_protocol::packets::play::Gamemode::Spectator),
            previous_game_mode: PreviousGameMode(Some(Gamemode::Creative)),
            debug: false,
            flat: false,
            death_location: None,
            portal_cooldown: VarInt(0),
            flags: RespawnFlags(RespawnFlags::KEEP_ENTITIES),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ServerData {
            motd: dust_protocol::text::Component::text("Dust").colored(
                dust_protocol::text::Color::Named(dust_protocol::text::NamedColor::Gold)
            ),
            icon: None,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetActionBarText {
            text: dust_protocol::text::Component::text("respawning..."),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetBorderCenter {
            center_x: 100.0,
            center_z: -100.0,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetBorderLerpSize {
            old_diameter: 6000.0,
            new_diameter: 100.0,
            lerp_speed: VarLong(86_400_000),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetBorderSize {
            diameter: 59_999_968.0
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetBorderWarningDelay {
            warning_time: VarInt(30)
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetBorderWarningDistance {
            warning_blocks: VarInt(2)
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetCamera {
            camera_entity_id: VarInt(-2)
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetCenterChunk {
            chunk_z: VarInt(-187),
            chunk_x: VarInt(45),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetChunkCacheRadius {
            distance: VarInt(10)
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetDefaultSpawnPosition {
            location: Position::new(0, 64, 0),
            angle: -179.75,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::DisplayObjective {
            slot: ScoreboardSlot::TeamColor(4),
            score_name: ProtocolString::new("money").expect("fits"),
            display_text: Some(dust_protocol::text::Component::text("Coins")),
            render_type: Some(ObjectiveRenderType::Integer),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::LinkEntities {
            attached_to: -3,
            connecting_entity: 44,
            link_kind: EntityLinkKind::Ride,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetEntityMotion {
            entity_id: VarInt(17),
            velocity: EntityVelocity {
                x: i16::MIN + 1,
                y: 8000,
                z: i16::MAX,
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetExperience {
            experience_bar: 0.5,
            level: VarInt(33),
            total_experience: VarInt(1024),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetHealth {
            health: 0.5,
            food: VarInt(20),
            food_saturation: 5.0,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::UpdateObjectives {
            objective_name: ProtocolString::new("health").expect("fits"),
            body: {
                use dust_protocol::packets::play::scoreboard::UpdateObjectivesBody;
                UpdateObjectivesBody {
                    mode: ObjectiveMode::Create,
                    display_name: Some(dust_protocol::text::Component::text("Health")),
                    render_type: Some(ObjectiveRenderType::Hearts),
                    number_format: Some(None),
                }
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetPassengers {
            vehicle_id: VarInt(44),
            passengers: vec![VarInt(1), VarInt(2)],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::UpdateTeams {
            team_name: ProtocolString::new("red").expect("fits"),
            body: TeamBody {
                method: TeamMethod::Create,
                info: Some(TeamInfo {
                    display_name: dust_protocol::text::Component::text("Red Team"),
                    friendly_flags: 0x03,
                    name_tag_visibility: NameTagVisibility::HideForOtherTeams,
                    collision_rule: CollisionRule::PushOwnTeam,
                    colour: VarInt(14),
                    prefix: dust_protocol::text::Component::text("[R] "),
                    suffix: dust_protocol::text::Component::text(""),
                }),
                members: vec![
                    ProtocolString::new("Notch").expect("fits"),
                    ProtocolString::new("jeb_").expect("fits"),
                ],
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::UpdateScore {
            entity_name: ProtocolString::new("Notch").expect("fits"),
            body: {
                use dust_protocol::packets::play::scoreboard::UpdateScoreBody;
                UpdateScoreBody {
                    objective: ProtocolString::new("money").expect("fits"),
                    score: VarInt(i32::MAX),
                    display: None,
                    number_format: None,
                }
            },
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetSimulationDistance {
            distance: VarInt(8)
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetSubtitleText {
            text: dust_protocol::text::Component::text("the subtitle"),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetTime {
            world_age: -1_700_000_000_000,
            time_of_day: 18_000,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetTitleText {
            text: dust_protocol::text::Component::text("The Title"),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetTitlesAnimation {
            fade_in: 10,
            stay: 70,
            fade_out: 20,
        }
    ));
    out.push(frame!(cb, Play, Clientbound, cb::StartConfiguration {}));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::SetTabListHeaderFooter {
            header: dust_protocol::text::Component::text("header"),
            footer: dust_protocol::text::Component::text("footer"),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::TagQueryResponse {
            transaction_id: VarInt(3),
            nbt: some_nbt(),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::TakeItemEntity {
            collected_entity_id: VarInt(90),
            collector_entity_id: VarInt(1),
            pickup_item_count: VarInt(64),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::TeleportEntity {
            entity_id: VarInt(19),
            x: 30_000_000.0,
            y: -2048.0,
            z: -30_000_000.0,
            yaw: Angle(255),
            pitch: Angle(127),
            on_ground: false,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::TickingState {
            tick_rate: 20.0,
            frozen: true,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::TickStep {
            tick_steps: VarInt(3)
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::Transfer {
            host: s("elsewhere.example"),
            port: VarInt(25566),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::UpdateAttributes {
            entity_id: VarInt(120),
            properties: vec![AttributeProperty {
                attribute_id: VarInt(8),
                base: 0.1,
                modifiers: vec![AttributeModifier {
                    id: id("minecraft:effect/speed"),
                    amount: 0.3,
                    operation: 2,
                }],
            }],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ApplyMobEffect {
            entity_id: VarInt(17),
            effect_id: VarInt(1),
            amplifier: VarInt(0),
            duration: VarInt(-1),
            flags: EffectFlags(EffectFlags::SHOW_PARTICLES | EffectFlags::SHOW_ICON),
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::UpdateTags {
            registries: vec![dust_protocol::packets::common::TagRegistry {
                registry: id("minecraft:block"),
                tags: vec![dust_protocol::packets::common::Tag {
                    name: id("minecraft:mineable/axe"),
                    entries: vec![VarInt(0)],
                }],
            }],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ProjectilePower {
            entity_id: VarInt(77),
            acceleration_power: 1.6,
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::CustomReportDetails {
            details: vec![dust_protocol::packets::common::ReportDetail {
                title: s("phase"),
                description: s("wave-three"),
            }],
        }
    ));
    out.push(frame!(
        cb,
        Play,
        Clientbound,
        cb::ServerLinks {
            links: vec![dust_protocol::packets::common::ServerLink {
                label: dust_protocol::packets::common::ServerLinkLabel::BuiltIn(
                    dust_protocol::packets::common::BuiltInLinkLabel::Status
                ),
                url: s("https://example.invalid/status"),
            }],
        }
    ));
}

fn bitset(bits: &[usize]) -> BitSet {
    let mut set = BitSet::default();
    for &bit in bits {
        set.set(bit, true);
    }
    set
}

fn fixed_bits() -> FixedBitSet<20> {
    let mut set = FixedBitSet::<20>::new();
    set.set(0, true);
    set.set(19, true);
    set
}
