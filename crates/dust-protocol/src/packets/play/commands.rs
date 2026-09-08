//! The command graph: brigadier's nodes, flags, parsers and properties.
//!
//! # What this packet is
//!
//! `minecraft:commands` is a directed graph in two passes: an array of nodes,
//! then the index of the root. Every node names its children and redirects by
//! *index into that array*, and indices may only point backwards — the graph
//! must be declared before it is referenced. This module models the nodes and
//! their wire layout; what commands *mean* is the server's business.
//!
//! # Why the parser table lives here, hand-written
//!
//! An argument node carries a parser id, and the parser decides how many
//! property bytes follow. That table is exactly the kind of thing the
//! extractor would generate — but it lives in no report Mojang emits as
//! *ordered* data; `command_argument_type` is a registry like any other and
//! this workspace has not extracted it yet. The id list below was transcribed
//! from the community documentation for protocol 767 and cross-checked
//! against vanilla 1.21.1's own registration order, which is where two easy
//! mistakes hide: `minecraft:uuid` is registered **last** (id 53), after the
//! development-only parsers that release builds skip, and `minecraft:item_slots`
//! sits between `item_slot` and `resource_location` where pre-1.21 tables do
//! not have it at all. A parser outside the table is refused rather than
//! skipped for the reason the format itself states: the remainder of the
//! packet cannot be located past a property block of unknown size.
//!
//! The same refusal logic as everywhere in this crate, then — but with the
//! note that here even the *vanilla* client behaves identically, which makes
//! refusing the least surprising thing this module can do.

use crate::text::Component;
use crate::types::{
    read_string, write_string, Decode, Encode, Identifier, ProtocolString, VarInt,
    DEFAULT_STRING_LIMIT,
};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{var_int_enum, ProtocolVersion};

var_int_enum! {
    /// What kind of node this is, carried in the low bits of the flags byte.
    pub enum NodeType {
        Root = 0,
        Literal = 1,
        Argument = 2,
    }
}

/// A numeric argument's optional bounds, shared by float, double, int and long.
///
/// One flags byte says which ends are present; absent ends mean unbounded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericRange<F> {
    pub min: Option<F>,
    pub max: Option<F>,
}

const RANGE_MIN_FLAG: u8 = 0x01;
const RANGE_MAX_FLAG: u8 = 0x02;

impl<F: Decode + Encode + Copy> NumericRange<F> {
    fn decode_range<R: WireRead + ?Sized>(
        input: &mut R,
        read: fn(&mut R) -> Result<F, DecodeError>,
    ) -> Result<Self, DecodeError> {
        let flags = input.read_u8()?;
        let min = if flags & RANGE_MIN_FLAG != 0 {
            Some(read(input)?)
        } else {
            None
        };
        let max = if flags & RANGE_MAX_FLAG != 0 {
            Some(read(input)?)
        } else {
            None
        };
        Ok(Self { min, max })
    }

    fn encode_range<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        write: fn(&mut W, F),
    ) -> Result<(), EncodeError> {
        let mut flags = 0u8;
        if self.min.is_some() {
            flags |= RANGE_MIN_FLAG;
        }
        if self.max.is_some() {
            flags |= RANGE_MAX_FLAG;
        }
        out.write_u8(flags);
        if let Some(min) = self.min {
            write(out, min);
        }
        if let Some(max) = self.max {
            write(out, max);
        }
        Ok(())
    }
}

/// Which reading mode a string argument uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringMode {
    SingleWord = 0,
    QuotablePhrase = 1,
    GreedyPhrase = 2,
}

impl StringMode {
    pub const ALL: [Self; 3] = [Self::SingleWord, Self::QuotablePhrase, Self::GreedyPhrase];

    pub const fn discriminant(self) -> i32 {
        self as i32
    }

    pub const fn from_discriminant(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::SingleWord),
            1 => Some(Self::QuotablePhrase),
            2 => Some(Self::GreedyPhrase),
            _ => None,
        }
    }
}

/// A parser's property block, tagged by which parser demanded it.
///
/// Parsers whose properties are empty do not appear here at all — the
/// [`Node`] writes nothing for them — so every variant below corresponds to a
/// property block with real bytes in it.
#[derive(Debug, Clone, PartialEq)]
pub enum ParserProperties {
    Float(NumericRange<f32>),
    Double(NumericRange<f64>),
    Integer(NumericRange<i32>),
    Long(NumericRange<i64>),
    String(StringMode),
    /// Entity selectors: bit 0 limits to one entity, bit 1 to players only.
    Entity(u8),
    /// Score holders: bit 0 allows more than one.
    ScoreHolder(u8),
    /// Minimum duration in ticks.
    Time(i32),
    /// The registry whose entries or tags the argument draws from.
    Registry(Identifier),
}

/// One argument parser from the 1.21.1 table: its namespaced name and whether
/// its property block has contents.
///
/// Public because callers building command trees need to spell parser ids by
/// name, and because the closed/open boundary should be inspectable like
/// metadata's is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserDef {
    pub id: i32,
    pub name: &'static str,
    pub has_properties: bool,
}

/// The parser table for protocol 767, in id order.
///
/// Transcribed by hand from the community documentation; the test at the
/// bottom of this file pins the count and the boundary ids so that a silent
/// truncation of this table shows up as a red line rather than as refusals on
/// a real server's command packet.
pub const PARSERS: &[ParserDef] = &[
    ParserDef {
        id: 0,
        name: "brigadier:bool",
        has_properties: false,
    },
    ParserDef {
        id: 1,
        name: "brigadier:float",
        has_properties: true,
    },
    ParserDef {
        id: 2,
        name: "brigadier:double",
        has_properties: true,
    },
    ParserDef {
        id: 3,
        name: "brigadier:integer",
        has_properties: true,
    },
    ParserDef {
        id: 4,
        name: "brigadier:long",
        has_properties: true,
    },
    ParserDef {
        id: 5,
        name: "brigadier:string",
        has_properties: true,
    },
    ParserDef {
        id: 6,
        name: "minecraft:entity",
        has_properties: true,
    },
    ParserDef {
        id: 7,
        name: "minecraft:game_profile",
        has_properties: false,
    },
    ParserDef {
        id: 8,
        name: "minecraft:block_pos",
        has_properties: false,
    },
    ParserDef {
        id: 9,
        name: "minecraft:column_pos",
        has_properties: false,
    },
    ParserDef {
        id: 10,
        name: "minecraft:vec3",
        has_properties: false,
    },
    ParserDef {
        id: 11,
        name: "minecraft:vec2",
        has_properties: false,
    },
    ParserDef {
        id: 12,
        name: "minecraft:block_state",
        has_properties: false,
    },
    ParserDef {
        id: 13,
        name: "minecraft:block_predicate",
        has_properties: false,
    },
    ParserDef {
        id: 14,
        name: "minecraft:item_stack",
        has_properties: false,
    },
    ParserDef {
        id: 15,
        name: "minecraft:item_predicate",
        has_properties: false,
    },
    ParserDef {
        id: 16,
        name: "minecraft:color",
        has_properties: false,
    },
    ParserDef {
        id: 17,
        name: "minecraft:component",
        has_properties: false,
    },
    ParserDef {
        id: 18,
        name: "minecraft:style",
        has_properties: false,
    },
    ParserDef {
        id: 19,
        name: "minecraft:message",
        has_properties: false,
    },
    ParserDef {
        id: 20,
        name: "minecraft:nbt_compound_tag",
        has_properties: false,
    },
    ParserDef {
        id: 21,
        name: "minecraft:nbt_tag",
        has_properties: false,
    },
    ParserDef {
        id: 22,
        name: "minecraft:nbt_path",
        has_properties: false,
    },
    ParserDef {
        id: 23,
        name: "minecraft:objective",
        has_properties: false,
    },
    ParserDef {
        id: 24,
        name: "minecraft:objective_criteria",
        has_properties: false,
    },
    ParserDef {
        id: 25,
        name: "minecraft:operation",
        has_properties: false,
    },
    ParserDef {
        id: 26,
        name: "minecraft:particle",
        has_properties: false,
    },
    ParserDef {
        id: 27,
        name: "minecraft:angle",
        has_properties: false,
    },
    ParserDef {
        id: 28,
        name: "minecraft:rotation",
        has_properties: false,
    },
    ParserDef {
        id: 29,
        name: "minecraft:scoreboard_slot",
        has_properties: false,
    },
    ParserDef {
        id: 30,
        name: "minecraft:score_holder",
        has_properties: true,
    },
    ParserDef {
        id: 31,
        name: "minecraft:swizzle",
        has_properties: false,
    },
    ParserDef {
        id: 32,
        name: "minecraft:team",
        has_properties: false,
    },
    ParserDef {
        id: 33,
        name: "minecraft:item_slot",
        has_properties: false,
    },
    ParserDef {
        id: 34,
        name: "minecraft:item_slots",
        has_properties: false,
    },
    ParserDef {
        id: 35,
        name: "minecraft:resource_location",
        has_properties: false,
    },
    ParserDef {
        id: 36,
        name: "minecraft:function",
        has_properties: false,
    },
    ParserDef {
        id: 37,
        name: "minecraft:entity_anchor",
        has_properties: false,
    },
    ParserDef {
        id: 38,
        name: "minecraft:int_range",
        has_properties: false,
    },
    ParserDef {
        id: 39,
        name: "minecraft:float_range",
        has_properties: false,
    },
    ParserDef {
        id: 40,
        name: "minecraft:dimension",
        has_properties: false,
    },
    ParserDef {
        id: 41,
        name: "minecraft:gamemode",
        has_properties: false,
    },
    ParserDef {
        id: 42,
        name: "minecraft:time",
        has_properties: true,
    },
    ParserDef {
        id: 43,
        name: "minecraft:resource_or_tag",
        has_properties: true,
    },
    ParserDef {
        id: 44,
        name: "minecraft:resource_or_tag_key",
        has_properties: true,
    },
    ParserDef {
        id: 45,
        name: "minecraft:resource",
        has_properties: true,
    },
    ParserDef {
        id: 46,
        name: "minecraft:resource_key",
        has_properties: true,
    },
    ParserDef {
        id: 47,
        name: "minecraft:template_mirror",
        has_properties: false,
    },
    ParserDef {
        id: 48,
        name: "minecraft:template_rotation",
        has_properties: false,
    },
    ParserDef {
        id: 49,
        name: "minecraft:heightmap",
        has_properties: false,
    },
    ParserDef {
        id: 50,
        name: "minecraft:loot_table",
        has_properties: false,
    },
    ParserDef {
        id: 51,
        name: "minecraft:loot_predicate",
        has_properties: false,
    },
    ParserDef {
        id: 52,
        name: "minecraft:loot_modifier",
        has_properties: false,
    },
    ParserDef {
        id: 53,
        name: "minecraft:uuid",
        has_properties: false,
    },
];

/// Look up a parser definition by id, or `None` if this version has no such
/// parser.
pub fn parser_by_id(id: i32) -> Option<&'static ParserDef> {
    PARSERS.iter().find(|parser| parser.id == id)
}

/// Look up a parser definition by its namespaced name.
///
/// The direction a *builder* needs. Minecraft's own command report names
/// parsers — `minecraft:time`, `brigadier:integer` — and the wire carries
/// their ids, so anything turning that report into a packet has to cross this
/// table in the opposite direction from the decoder. Writing the id out by
/// hand at the call site is the alternative and is exactly the transcription
/// mistake the table above exists to keep in one place.
pub fn parser_by_name(name: &str) -> Option<&'static ParserDef> {
    PARSERS.iter().find(|parser| parser.name == name)
}

impl ParserProperties {
    fn decode_for<R: WireRead + ?Sized>(
        parser_id: i32,
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        match parser_id {
            1 => Ok(Self::Float(NumericRange::decode_range(input, |input| {
                input.read_f32()
            })?)),
            2 => Ok(Self::Double(NumericRange::decode_range(input, |input| {
                input.read_f64()
            })?)),
            3 => Ok(Self::Integer(NumericRange::decode_range(input, |input| {
                input.read_i32()
            })?)),
            4 => Ok(Self::Long(NumericRange::decode_range(input, |input| {
                input.read_i64()
            })?)),
            5 => {
                let raw = input.read_var_int()?;
                let mode =
                    StringMode::from_discriminant(raw).ok_or(DecodeError::UnknownVariant {
                        name: "StringMode",
                        value: raw,
                    })?;
                Ok(Self::String(mode))
            }
            6 => Ok(Self::Entity(input.read_u8()?)),
            // 30, not 31. `minecraft:score_holder` sits immediately after
            // `minecraft:scoreboard_slot` in vanilla's registration order, and
            // 31 is `minecraft:swizzle`, which takes no properties at all — so
            // this arm used to be unreachable and every real `/scoreboard` or
            // `/trigger` node in a vanilla `commands` packet was refused as
            // "known but not modelled". The test below pins each of these arms
            // against [`PARSERS`] by name, which is the only reason the number
            // and the table cannot drift apart again.
            30 => Ok(Self::ScoreHolder(input.read_u8()?)),
            42 => Ok(Self::Time(input.read_i32()?)),
            43..=46 => Ok(Self::Registry(Identifier::decode(input, version)?)),
            other => Err(DecodeError::Unsupported {
                field: "command parser",
                why: match parser_by_id(other) {
                    Some(_) => "this parser's properties are known but not modelled",
                    None => {
                        "this parser id is not in this version's table, and its property \
                             block's length cannot be guessed"
                    }
                },
            }),
        }
    }

    fn encode_with<W: WireWrite + ?Sized>(
        &self,
        parser_id: i32,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        // The pairing this asserts is checked by the caller: `Node`'s encoder
        // refuses a properties/no-properties mismatch before reaching here, so
        // in debug builds writing under an id that takes none is a bug of ours.
        //
        // Asked of [`PARSERS`] rather than of a literal id list, and that is
        // the fix rather than the style: the list used to be written out here
        // as `1..=6 | 31 | 42..=46`, and `minecraft:score_holder` is id **30**.
        // So every debug build that encoded a score-holder argument's property
        // block — `/scoreboard players set`, `/trigger` — tripped an assertion
        // about code that was correct. A guard with its own copy of the table
        // it guards is a guard that can disagree with it.
        debug_assert!(
            parser_by_id(parser_id).is_some_and(|def| def.has_properties),
            "properties written under a parser id that takes none: {parser_id}"
        );
        match self {
            Self::Float(range) => range.encode_range(out, |out, value| out.write_f32(value)),
            Self::Double(range) => range.encode_range(out, |out, value| out.write_f64(value)),
            Self::Integer(range) => range.encode_range(out, |out, value| out.write_i32(value)),
            Self::Long(range) => range.encode_range(out, |out, value| out.write_i64(value)),
            Self::String(mode) => {
                out.write_var_int(mode.discriminant());
                Ok(())
            }
            Self::Entity(flags) | Self::ScoreHolder(flags) => {
                out.write_u8(*flags);
                Ok(())
            }
            Self::Time(ticks) => {
                out.write_i32(*ticks);
                Ok(())
            }
            Self::Registry(registry) => registry.encode(out, version),
        }
    }
}

/// One node of the command graph, exactly as it travels.
///
/// Children and redirect are **indices** into the packet's own node array;
/// resolving them to anything is the caller's job, and this layer stores them
/// raw so a graph that references itself is representable — brigadier's own
/// graphs contain such cycles.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub kind: NodeType,
    /// Whether the path down to this node forms a runnable command.
    pub executable: bool,
    /// Index of the node this one redirects to, when present.
    pub redirect: Option<VarInt>,
    /// The literal's word or the argument's name. Absent for the root.
    /// The literal's word or the argument's name. Absent for the root.
    ///
    /// A plain `String` at the protocol's default bound rather than
    /// [`crate::types::ProtocolString`]: a node's name is data this crate is
    /// happy to hold loosely, and a bounded wrapper here would make every
    /// caller construct through `new` for a check encode already performs.
    pub name: Option<String>,
    /// The argument's parser id and its property block.
    pub parser: Option<(i32, Option<ParserProperties>)>,
    /// A suggestions-type identifier overriding the parser's default.
    pub suggestions: Option<Identifier>,
    /// Indices into the packet's node array.
    pub children: Vec<VarInt>,
}

const NODE_EXECUTABLE: u8 = 0x04;
const NODE_REDIRECT: u8 = 0x08;
const NODE_SUGGESTIONS: u8 = 0x10;

impl Node {
    /// A root or literal node with no extras.
    pub fn literal(kind: NodeType, name: Option<&str>) -> Self {
        Self {
            kind,
            executable: false,
            redirect: None,
            name: name.map(str::to_owned),
            parser: None,
            suggestions: None,
            children: Vec::new(),
        }
    }
}

impl Decode for Node {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let flags = input.read_u8()?;
        let kind = NodeType::from_discriminant(i32::from(flags & 0x03)).ok_or(
            DecodeError::UnknownVariant {
                name: "NodeType",
                value: i32::from(flags & 0x03),
            },
        )?;
        let children = Vec::<VarInt>::decode(input, version)?;
        let redirect = if flags & NODE_REDIRECT != 0 {
            Some(VarInt::decode(input, version)?)
        } else {
            None
        };
        let name = if kind != NodeType::Root {
            Some(read_string(input, DEFAULT_STRING_LIMIT)?)
        } else {
            None
        };
        let parser = if kind == NodeType::Argument {
            let id = input.read_var_int()?;
            if parser_by_id(id).is_none() {
                return Err(DecodeError::Unsupported {
                    field: "command parser",
                    why: "this parser id is not in this version's table, and the length of \
                          what follows cannot be guessed",
                });
            }
            let has_properties = parser_by_id(id).is_some_and(|def| def.has_properties);
            let properties = if has_properties {
                Some(ParserProperties::decode_for(id, input, version)?)
            } else {
                None
            };
            Some((id, properties))
        } else {
            None
        };
        let suggestions = if flags & NODE_SUGGESTIONS != 0 {
            if kind != NodeType::Argument {
                return Err(DecodeError::Unsupported {
                    field: "command node suggestions",
                    why: "only argument nodes carry a suggestions override",
                });
            }
            Some(Identifier::decode(input, version)?)
        } else {
            None
        };
        Ok(Self {
            kind,
            executable: flags & NODE_EXECUTABLE != 0,
            redirect,
            name,
            parser,
            suggestions,
            children,
        })
    }
}

impl Encode for Node {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        let mut flags = self.kind.discriminant() as u8;
        if self.executable {
            flags |= NODE_EXECUTABLE;
        }
        if self.redirect.is_some() {
            flags |= NODE_REDIRECT;
        }
        if self.suggestions.is_some() {
            flags |= NODE_SUGGESTIONS;
        }
        out.write_u8(flags);
        self.children.encode(out, version)?;
        if let Some(redirect) = &self.redirect {
            redirect.encode(out, version)?;
        }
        if let Some(name) = &self.name {
            write_string(out, name, DEFAULT_STRING_LIMIT)?;
        }
        if let Some((id, properties)) = &self.parser {
            out.write_var_int(*id);
            // A parser that takes properties must be given them: writing none
            // would leave the peer reading the next node's bytes as properties.
            match (parser_by_id(*id).map(|def| def.has_properties), properties) {
                (Some(true), Some(properties)) => properties.encode_with(*id, out, version)?,
                (Some(true), None) => {
                    return Err(EncodeError::Unsupported {
                        field: "command parser properties",
                        why: "this parser's property block is required and none was given",
                    });
                }
                (Some(false), Some(_)) => {
                    return Err(EncodeError::Unsupported {
                        field: "command parser properties",
                        why: "this parser takes no properties and some were given",
                    });
                }
                _ => {}
            }
        }
        if let Some(suggestions) = &self.suggestions {
            suggestions.encode(out, version)?;
        }
        Ok(())
    }
}

/// Everything after the packet id: the node array and the root index.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandsBody {
    pub nodes: Vec<Node>,
    /// Index into [`Self::nodes`] of the graph's root.
    pub root_index: VarInt,
}

impl Decode for CommandsBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            nodes: Vec::<Node>::decode(input, version)?,
            root_index: VarInt::decode(input, version)?,
        })
    }
}

impl Encode for CommandsBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.nodes.encode(out, version)?;
        self.root_index.encode(out, version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version;
    use crate::wire::{Reader, Writer};

    fn v() -> ProtocolVersion {
        version::V1_21_1
    }

    #[test]
    fn the_parser_table_is_complete_and_ends_where_the_version_ends() {
        assert_eq!(PARSERS.first().expect("non-empty").id, 0);
        let last = PARSERS.last().expect("non-empty");
        // `minecraft:uuid` is registered after the development-only parsers,
        // which release builds skip, so it is the last id a release client can
        // meet — pinning the name as well as the number catches a table built
        // from a snapshot's registry instead of the release's.
        assert_eq!(last.name, "minecraft:uuid");
        assert_eq!(last.id, 53);
        // Ids are contiguous: a hole would mean a transcription slip.
        for (position, parser) in PARSERS.iter().enumerate() {
            assert_eq!(parser.id, position as i32, "{}", parser.name);
        }
    }

    #[test]
    fn every_property_shape_is_decoded_under_the_id_the_table_gives_it() {
        // Each arm of `decode_for` is a number written next to a shape, and a
        // number written by hand beside a table that already holds it is a
        // number that can be wrong — `minecraft:score_holder` was decoded
        // under 31 for the whole life of this file, where the table says 30.
        // This asks the table for each one.
        for (name, expected) in [
            ("brigadier:float", 1),
            ("brigadier:double", 2),
            ("brigadier:integer", 3),
            ("brigadier:long", 4),
            ("brigadier:string", 5),
            ("minecraft:entity", 6),
            ("minecraft:score_holder", 30),
            ("minecraft:time", 42),
            ("minecraft:resource_or_tag", 43),
            ("minecraft:resource_or_tag_key", 44),
            ("minecraft:resource", 45),
            ("minecraft:resource_key", 46),
        ] {
            let def = parser_by_name(name).expect("in the table");
            assert_eq!(def.id, expected, "{name}");
            assert!(def.has_properties, "{name}");
        }
        // And the converse: every parser the table says carries properties has
        // an arm above, so a thirteenth would fail here rather than at a
        // client.
        let with_properties: Vec<i32> = PARSERS
            .iter()
            .filter(|parser| parser.has_properties)
            .map(|parser| parser.id)
            .collect();
        assert_eq!(
            with_properties,
            vec![1, 2, 3, 4, 5, 6, 30, 42, 43, 44, 45, 46]
        );
    }

    #[test]
    fn a_literal_tree_round_trips_with_its_flags() {
        let mut greet = Node::literal(NodeType::Literal, Some("greet"));
        greet.executable = true;
        greet.children.push(VarInt(1));
        let mut root = Node::literal(NodeType::Root, None);
        root.children.push(VarInt(1));
        let body = CommandsBody {
            nodes: vec![root, greet],
            root_index: VarInt(0),
        };
        let mut writer = Writer::new();
        body.encode(&mut writer, v()).expect("encodes");
        let back = CommandsBody::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
        assert_eq!(back, body);
        assert!(back.nodes[1].executable);
        assert_eq!(back.nodes[1].children, vec![VarInt(1)]);
    }

    #[test]
    fn an_argument_node_carries_its_parser_and_properties() {
        let mut node = Node::literal(NodeType::Argument, Some("speed"));
        node.parser = Some((
            3,
            Some(ParserProperties::Integer(NumericRange {
                min: Some(1),
                max: None,
            })),
        ));
        node.suggestions = Some(Identifier::parse("minecraft:ask_server").expect("valid"));
        let mut writer = Writer::new();
        node.encode(&mut writer, v()).expect("encodes");
        let back = Node::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
        assert_eq!(back, node);

        // And a parser whose properties were forgotten is refused, not
        // written as a frame the client misparses.
        let mut hollow = Node::literal(NodeType::Argument, Some("speed"));
        hollow.parser = Some((3, None));
        assert!(matches!(
            hollow.encode(&mut Writer::new(), v()),
            Err(EncodeError::Unsupported { .. })
        ));
    }

    #[test]
    fn an_unknown_parser_stops_the_decode_named_and_immediately() {
        // Id 999 is outside the table; the bytes behind it could be anything,
        // which is why the refusal comes before any property read.
        let mut writer = Writer::new();
        writer.write_u8(0x02); // argument
        writer.write_var_int(0); // no children
        writer.write_var_int(5); // the name "speed", length-prefixed
        writer.write_slice(b"speed");
        writer.write_var_int(999);
        writer.write_slice(&[0xFF; 16]);
        assert!(matches!(
            Node::decode(&mut Reader::new(writer.as_bytes()), v()),
            Err(DecodeError::Unsupported {
                field: "command parser",
                ..
            })
        ));
    }

    #[test]
    fn suggestions_only_ride_on_arguments() {
        let mut root = Node::literal(NodeType::Root, None);
        root.suggestions = Some(Identifier::parse("minecraft:ask_server").expect("valid"));
        let mut writer = Writer::new();
        root.encode(&mut writer, v()).expect("encodes");
        assert!(matches!(
            Node::decode(&mut Reader::new(writer.as_bytes()), v()),
            Err(DecodeError::Unsupported { .. })
        ));
    }

    #[test]
    fn string_modes_are_a_closed_set() {
        for mode in StringMode::ALL {
            let mut node = Node::literal(NodeType::Argument, Some("text"));
            node.parser = Some((5, Some(ParserProperties::String(mode))));
            let mut writer = Writer::new();
            node.encode(&mut writer, v()).expect("encodes");
            assert_eq!(
                Node::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
                node
            );
        }
        let mut writer = Writer::new();
        writer.write_var_int(9);
        assert_eq!(
            StringMode::from_discriminant(9),
            None,
            "an invented mode is refused"
        );
    }
}

/// One tab-completion match: the text to insert, and an optional tooltip.
///
/// The text travels as a plain bounded string — a completion is inserted
/// verbatim, so there is nothing for a component to style.
#[derive(Debug, Clone, PartialEq)]
pub struct SuggestionMatch {
    pub text: ProtocolString,
    pub tooltip: Option<Component>,
}

impl Decode for SuggestionMatch {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            text: ProtocolString::decode(input, version)?,
            tooltip: Option::<Component>::decode(input, version)?,
        })
    }
}

impl Encode for SuggestionMatch {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.text.encode(out, version)?;
        self.tooltip.encode(out, version)
    }
}
