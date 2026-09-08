//! The commands this server runs, declared to the client out of Minecraft's
//! own graph and dispatched here.
//!
//! # Why the declaration is not written by hand
//!
//! A client does not discover commands; it is *told* them, once, as a
//! brigadier node graph, and everything it then does locally — tab
//! completion, the red underline while you type, whether `1d` is a legal
//! `/time set` argument — is that graph's doing. Writing those nodes by hand
//! would mean transcribing parser ids and property blocks from documentation
//! and then finding out from a real client which of them were wrong.
//!
//! `cargo xtask extract` already committed the whole graph as
//! [`dust_registry::commands`], from Minecraft's own command report. So what
//! happens here is a **subgraph**: [`DECLARED`] names the commands Dust can
//! actually run, and everything reachable from those nodes is copied across to
//! the wire form with its shape intact. A client's parser therefore agrees
//! with vanilla's by construction rather than by testing, and adding a command
//! is adding a name to a list.
//!
//! What that cannot do is stop the two halves drifting apart in the other
//! direction: a name in [`DECLARED`] with no arm in [`Command::parse`] is a
//! command the client offers and the server refuses. The test at the bottom of
//! this file is that guard.
//!
//! # Who may run one
//!
//! Everybody, and that is a real consequence rather than an oversight. Dust
//! has no operator list — the console can `say`, `list` and `stop`, and no
//! player has a permission level at all — so a declared command is a command
//! every player has. For `/time` on a server where any player can already
//! break any block, that is consistent; it is also the thing that has to
//! change first when the second command is one that should not be. Decision
//! record 0044 says so out loud rather than leaving it to be discovered.

use dust_protocol::packets::play;
use dust_protocol::packets::play::commands as wire;
use dust_protocol::text::{Color, Component, NamedColor};
use dust_protocol::types::{Identifier, VarInt};
use dust_registry::commands::{ArgumentProperties, CommandGraph, NodeKind};

use super::daylight::{self, WorldClock};

/// The commands declared to clients, by their path in Minecraft's own graph.
///
/// One entry, and the list is the point: a client is told exactly what this
/// server implements, so the completion it offers is the truth. A server that
/// declared vanilla's whole graph would tab-complete 1,007 runnable commands
/// and run one.
pub const DECLARED: &[&str] = &["time"];

/// Build the `minecraft:commands` packet for [`DECLARED`].
///
/// # Errors
///
/// A declared path that is not in the generated graph, or a node whose parser
/// this protocol version has no id for. Both are build-time mistakes rather
/// than runtime conditions — they cannot depend on a client — which is why
/// this is called once at boot and its result kept.
pub fn declaration() -> Result<play::clientbound::Commands, String> {
    let mut nodes = vec![wire::Node::literal(wire::NodeType::Root, None)];
    // Registry index -> packet index, so a node shared by two declared
    // commands is emitted once and both point at it.
    let mut placed: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();

    for path in DECLARED {
        let root = CommandGraph::resolve(path)
            .ok_or_else(|| format!("the generated command graph has no `{path}`"))?;
        let index = emit(root, &mut nodes, &mut placed)?;
        nodes[0]
            .children
            .push(VarInt(i32::try_from(index).map_err(|_| {
                "the command graph does not fit in an i32".to_owned()
            })?));
    }

    Ok(play::clientbound::Commands {
        body: wire::CommandsBody {
            nodes,
            root_index: VarInt(0),
        },
    })
}

/// Copy one registry node and everything under it into the packet's array,
/// returning where it landed.
///
/// The node is reserved *before* its children are walked, which is what makes
/// a graph that points back at itself terminate — brigadier's `execute` is
/// nothing but such cycles, and while nothing in [`DECLARED`] has one today,
/// a function that only works on trees would fail the first time one did.
fn emit(
    index: usize,
    nodes: &mut Vec<wire::Node>,
    placed: &mut std::collections::HashMap<usize, usize>,
) -> Result<usize, String> {
    if let Some(&already) = placed.get(&index) {
        return Ok(already);
    }
    let def = CommandGraph::def(index)
        .ok_or_else(|| format!("the generated command graph has no node {index}"))?;

    let here = nodes.len();
    placed.insert(index, here);
    nodes.push(wire::Node {
        kind: match def.kind {
            NodeKind::Root => wire::NodeType::Root,
            NodeKind::Literal => wire::NodeType::Literal,
            NodeKind::Argument => wire::NodeType::Argument,
        },
        executable: def.executable,
        redirect: None,
        name: match def.kind {
            NodeKind::Root => None,
            _ => Some(def.name.to_owned()),
        },
        parser: match def.parser {
            None => None,
            Some(name) => {
                let parser = wire::parser_by_name(name).ok_or_else(|| {
                    format!("this protocol version's parser table has no `{name}`")
                })?;
                let properties = match def.properties.map(properties_of).transpose()? {
                    Some(_) if !parser.has_properties => {
                        return Err(format!(
                            "the report gives `{name}` a property block and this version's \
                             parser table says it takes none"
                        ))
                    }
                    Some(properties) => Some(properties),
                    None if parser.has_properties => Some(unbounded(name)?),
                    None => None,
                };
                Some((parser.id, properties))
            }
        },
        // The report carries no suggestion providers, so nothing here can
        // invent one; see `dust_registry::commands`.
        suggestions: None,
        children: Vec::new(),
    });

    let mut children = Vec::with_capacity(def.children.len());
    for &child in def.children {
        let at = emit(child as usize, nodes, placed)?;
        children.push(VarInt(
            i32::try_from(at).map_err(|_| "the command graph does not fit in an i32".to_owned())?,
        ));
    }
    let redirect = match def.redirect {
        None => None,
        Some(target) => Some(VarInt(
            i32::try_from(emit(target as usize, nodes, placed)?)
                .map_err(|_| "the command graph does not fit in an i32".to_owned())?,
        )),
    };
    nodes[here].children = children;
    nodes[here].redirect = redirect;
    Ok(here)
}

/// The property block a parser that requires one gets when the report gave it
/// none.
///
/// **This is a real difference between the two formats and not a tidy-up.**
/// Minecraft's command report omits `properties` entirely for a numeric
/// argument with neither bound — `/execute positioned` and every other
/// unbounded double look like parsers with no properties at all. The wire has
/// no such omission: `brigadier:double` always writes a flags byte, and a
/// client that read the next node's first byte as those flags would misparse
/// the rest of the packet. So an absent block becomes an empty one, which is
/// what vanilla writes for the same node.
///
/// Only the four numeric parsers reach here. Every other parser that takes
/// properties — string, entity, score holder, resource, time — carries them in
/// the report whenever it appears, so a missing block there is a report this
/// code has not seen before and is named rather than invented.
fn unbounded(parser: &str) -> Result<wire::ParserProperties, String> {
    Ok(match parser {
        "brigadier:float" => wire::ParserProperties::Float(wire::NumericRange {
            min: None,
            max: None,
        }),
        "brigadier:double" => wire::ParserProperties::Double(wire::NumericRange {
            min: None,
            max: None,
        }),
        "brigadier:integer" => wire::ParserProperties::Integer(wire::NumericRange {
            min: None,
            max: None,
        }),
        "brigadier:long" => wire::ParserProperties::Long(wire::NumericRange {
            min: None,
            max: None,
        }),
        other => {
            return Err(format!(
                "`{other}` takes a property block and the report gave none"
            ))
        }
    })
}

/// The report's property block, in the shape the wire wants.
///
/// Two tables that say the same thing in different vocabularies, and the whole
/// risk here is a flag bit read the wrong way round. The entity and
/// score-holder bits are the two that can be silently wrong — both are `1`
/// meaning opposite things — so both are spelled out rather than cast.
fn properties_of(properties: ArgumentProperties) -> Result<wire::ParserProperties, String> {
    Ok(match properties {
        ArgumentProperties::Integer { min, max } => {
            wire::ParserProperties::Integer(wire::NumericRange { min, max })
        }
        ArgumentProperties::Float { min, max } => {
            wire::ParserProperties::Float(wire::NumericRange { min, max })
        }
        ArgumentProperties::Double { min, max } => {
            wire::ParserProperties::Double(wire::NumericRange { min, max })
        }
        ArgumentProperties::StringKind(kind) => wire::ParserProperties::String(match kind {
            "word" => wire::StringMode::SingleWord,
            "phrase" => wire::StringMode::QuotablePhrase,
            "greedy" => wire::StringMode::GreedyPhrase,
            other => return Err(format!("`{other}` is not a string reading mode")),
        }),
        // Bit 0 is "one entity only", bit 1 is "players only" — vanilla's
        // `EntityArgument`, which writes the same two bits.
        ArgumentProperties::Entity {
            single,
            players_only,
        } => wire::ParserProperties::Entity(u8::from(single) | (u8::from(players_only) << 1)),
        // And here the bit means the *opposite* of the field: the wire's bit 0
        // is "more than one is allowed", where the report says whether it is
        // single. Reading this one straight through is the mistake this
        // function exists to make only once.
        ArgumentProperties::ScoreHolder { single } => {
            wire::ParserProperties::ScoreHolder(u8::from(!single))
        }
        ArgumentProperties::Resource { registry } => wire::ParserProperties::Registry(
            Identifier::parse(registry).map_err(|e| format!("`{registry}`: {e}"))?,
        ),
        ArgumentProperties::Time { min } => wire::ParserProperties::Time(min),
    })
}

/// A command this server understands, already parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `/time set <ticks>` — absolute.
    TimeSet(u64),
    /// `/time add <ticks>`.
    TimeAdd(u64),
    /// `/time query <what>`.
    TimeQuery(Query),
}

/// Which of the clock's three readings `/time query` was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// The sun's position within its day, `0..24_000`.
    Daytime,
    /// Every tick this world has run.
    Gametime,
    /// Which day the world is on.
    Day,
}

impl Command {
    /// Parse the text of a `chat_command` packet — no leading slash.
    ///
    /// # Errors
    ///
    /// The message a client should be shown. Vanilla's wording where vanilla
    /// has one, because the player reading it has met it before.
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut words = input.split_whitespace();
        match words.next() {
            Some("time") => {}
            _ => return Err(UNKNOWN.to_owned()),
        }
        let rest: Vec<&str> = words.collect();
        match rest.as_slice() {
            ["set", value] => Ok(Self::TimeSet(named_or_ticks(value)?)),
            ["add", value] => Ok(Self::TimeAdd(ticks(value, 0)?)),
            ["query", "daytime"] => Ok(Self::TimeQuery(Query::Daytime)),
            ["query", "gametime"] => Ok(Self::TimeQuery(Query::Gametime)),
            ["query", "day"] => Ok(Self::TimeQuery(Query::Day)),
            _ => Err(UNKNOWN.to_owned()),
        }
    }

    /// Run it against the world's clock and say what to tell the player.
    ///
    /// Feedback is a plain text component carrying vanilla's own English
    /// wording rather than the translation key vanilla sends. The key would be
    /// better — a French client would read French — but `commands.time.set` is
    /// `"Set the time to %s"`, and a translated component's arguments live in
    /// a `with` list that [`Component`] does not model yet. A key with no
    /// arguments renders the `%s` literally, which is worse than English.
    #[must_use]
    pub fn run(self, clock: &WorldClock) -> Component {
        match self {
            Self::TimeSet(value) => {
                clock.set_day_time(value);
                // Vanilla answers with the *reduced* time, not the total it
                // stored: `/time set 30000` replies 6000.
                Component::text(format!("Set the time to {}", clock.time_of_day()))
            }
            Self::TimeAdd(delta) => {
                clock.add_day_time(delta);
                Component::text(format!("Set the time to {}", clock.time_of_day()))
            }
            Self::TimeQuery(what) => {
                let value = match what {
                    Query::Daytime => clock.time_of_day(),
                    // Vanilla reduces game time modulo `i32::MAX` before
                    // printing it, because the command's return value is an
                    // int. Sixty-eight years of uptime away, and copied
                    // anyway: the alternative is a number that disagrees with
                    // vanilla's for no reason anybody could find later.
                    Query::Gametime => clock.game_time() % (i32::MAX as u64),
                    Query::Day => clock.day() % (i32::MAX as u64),
                };
                Component::text(format!("The time is {value}"))
            }
        }
    }
}

/// What a client is told when nothing here can run what it typed.
///
/// Vanilla's first line verbatim. Vanilla follows it with the input and a
/// `<--[HERE]` marker pointing at the character that failed; that needs a
/// parser which reports *where* it stopped, and this one does not, so the
/// marker is left off rather than pointed at a guess.
const UNKNOWN: &str = "Unknown or incomplete command, see below for error";

/// `/time set` takes four words as well as a number.
fn named_or_ticks(word: &str) -> Result<u64, String> {
    match word {
        "day" => Ok(daylight::DAY),
        "noon" => Ok(daylight::NOON),
        "night" => Ok(daylight::NIGHT),
        "midnight" => Ok(daylight::MIDNIGHT),
        other => ticks(other, 0),
    }
}

/// Minecraft's `minecraft:time` argument: a number and an optional unit.
///
/// `d` is a day, `s` a second, `t` a tick, and no unit is ticks. The number is
/// a float in vanilla and rounded after multiplying, which is what makes
/// `0.5d` a legal twelve thousand ticks — so it is a float here too. A unit
/// this table does not have is refused rather than treated as ticks, because
/// `/time set 100x` meaning `/time set 100` is a typo that silently works.
fn ticks(word: &str, minimum: i64) -> Result<u64, String> {
    let split = word
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(word.len());
    let (number, unit) = word.split_at(split);
    // The number first and the unit second, which is the order vanilla's
    // `TimeArgument` reads them in and therefore the order its errors come in:
    // `/time set banana` splits into an empty number and a unit of "banana",
    // and complaining about the unit would be answering the second question
    // when the first one already failed.
    let value: f64 = number
        .parse()
        .map_err(|_| format!("Expected float, got \"{number}\""))?;
    if !value.is_finite() {
        return Err(format!("Expected float, got \"{number}\""));
    }
    let per_unit: f64 = match unit {
        "" | "t" => 1.0,
        "s" => 20.0,
        "d" => f64::from(u32::try_from(daylight::DAY_TICKS).expect("a day fits in a u32")),
        other => return Err(format!("Invalid unit \"{other}\"")),
    };
    #[allow(clippy::cast_possible_truncation)]
    let ticks = (value * per_unit).round() as i64;
    if ticks < minimum {
        return Err(format!("Tick count must not be less than {minimum}"));
    }
    u64::try_from(ticks).map_err(|_| format!("Tick count must not be less than {minimum}"))
}

/// Colour a refusal red, the way vanilla colours a command error.
#[must_use]
pub fn refusal(why: &str) -> Component {
    Component::text(why.to_owned()).colored(Color::Named(NamedColor::Red))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_protocol::types::{Decode, Encode};

    fn v() -> dust_protocol::ProtocolVersion {
        dust_protocol::ProtocolVersion::from_name("1.21.1").expect("the target version")
    }

    fn text_of(component: &Component) -> String {
        match &component.body {
            dust_protocol::text::Body::Text(text) => text.clone(),
            other => panic!("expected plain text, got {other:?}"),
        }
    }

    #[test]
    fn every_declared_command_is_one_the_server_can_actually_run() {
        // The drift this file's header names: a client is told about what is
        // in `DECLARED`, and offering a completion for a command the server
        // answers "unknown" to is worse than not offering it.
        for path in DECLARED {
            assert!(
                CommandGraph::resolve(path).is_some(),
                "`{path}` is declared and is not in Minecraft's own graph"
            );
            assert!(
                Command::parse(&format!("{path} query gametime")).is_ok()
                    || Command::parse(path).is_ok(),
                "`{path}` is declared and nothing here parses it"
            );
        }
    }

    #[test]
    fn the_declaration_carries_the_whole_time_subtree_and_round_trips() {
        let packet = declaration().expect("builds");
        let body = packet.body;
        // Root plus `time`; `add` and its argument; `query` and its three
        // literals; `set`, its four named times and its argument. Counted
        // rather than asserted loosely, because the number is what says the
        // walk reached the leaves.
        assert_eq!(body.nodes.len(), 14, "{body:#?}");
        assert_eq!(body.root_index, VarInt(0));
        assert_eq!(body.nodes[0].kind, wire::NodeType::Root);
        assert_eq!(body.nodes[0].children, vec![VarInt(1)]);
        assert_eq!(body.nodes[1].name.as_deref(), Some("time"));
        assert!(
            !body.nodes[1].executable,
            "`/time` on its own is not a command"
        );

        let names: Vec<&str> = body
            .nodes
            .iter()
            .filter_map(|node| node.name.as_deref())
            .collect();
        for expected in [
            "time", "add", "query", "day", "daytime", "gametime", "set", "midnight", "night",
            "noon",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} is missing: {names:?}"
            );
        }

        // The argument nodes carry Minecraft's own parser and its minimum,
        // which is what makes a client accept `1d` and refuse `-5`.
        let time_arguments: Vec<&wire::Node> = body
            .nodes
            .iter()
            .filter(|node| node.kind == wire::NodeType::Argument)
            .collect();
        assert_eq!(time_arguments.len(), 2, "set, add and nothing else");
        for node in time_arguments {
            let (id, properties) = node.parser.clone().expect("an argument has a parser");
            assert_eq!(
                wire::parser_by_id(id).map(|p| p.name),
                Some("minecraft:time")
            );
            assert_eq!(properties, Some(wire::ParserProperties::Time(0)));
        }

        let mut out = dust_protocol::wire::Writer::new();
        body.encode(&mut out, v()).expect("encodes");
        let back =
            wire::CommandsBody::decode(&mut dust_protocol::wire::Reader::new(out.as_bytes()), v())
                .expect("decodes");
        assert_eq!(back, body);
    }

    #[test]
    fn the_whole_of_minecrafts_command_graph_converts_and_encodes() {
        // The conversion table above is between two vocabularies, and the only
        // honest test of a translation is the whole dictionary. Every one of
        // the 1,763 nodes Minecraft's report produced goes across here — every
        // parser, every property shape, every redirect and the cycles they
        // make — and then through the encoder, so a parser this version has no
        // id for or a property block written at the wrong width is a red line
        // rather than a client that silently drops the packet.
        let mut nodes = vec![wire::Node::literal(wire::NodeType::Root, None)];
        let mut placed = std::collections::HashMap::new();
        let root = CommandGraph::def(CommandGraph::ROOT).expect("a root");
        for &child in root.children {
            let at = emit(child as usize, &mut nodes, &mut placed).expect("converts");
            nodes[0].children.push(VarInt(at as i32));
        }
        assert_eq!(
            nodes.len(),
            // Every node but the report's own root, which this rebuilds.
            CommandGraph::len(),
            "the walk did not reach every node"
        );
        let body = wire::CommandsBody {
            nodes,
            root_index: VarInt(0),
        };
        let mut out = dust_protocol::wire::Writer::new();
        body.encode(&mut out, v()).expect("the whole graph encodes");
        let back =
            wire::CommandsBody::decode(&mut dust_protocol::wire::Reader::new(out.as_bytes()), v())
                .expect("and decodes");
        assert_eq!(back, body);
    }

    #[test]
    fn the_four_named_times_are_minecrafts_own_numbers() {
        assert_eq!(Command::parse("time set day"), Ok(Command::TimeSet(1_000)));
        assert_eq!(Command::parse("time set noon"), Ok(Command::TimeSet(6_000)));
        assert_eq!(
            Command::parse("time set night"),
            Ok(Command::TimeSet(13_000))
        );
        assert_eq!(
            Command::parse("time set midnight"),
            Ok(Command::TimeSet(18_000))
        );
    }

    #[test]
    fn a_time_argument_carries_its_unit() {
        assert_eq!(Command::parse("time add 100"), Ok(Command::TimeAdd(100)));
        assert_eq!(Command::parse("time add 100t"), Ok(Command::TimeAdd(100)));
        assert_eq!(Command::parse("time add 1s"), Ok(Command::TimeAdd(20)));
        assert_eq!(Command::parse("time add 1d"), Ok(Command::TimeAdd(24_000)));
        // Vanilla reads a float and rounds after multiplying, which is the
        // only reason half a day is expressible at all.
        assert_eq!(
            Command::parse("time add 0.5d"),
            Ok(Command::TimeAdd(12_000))
        );
        // A unit that is not one of the three is refused rather than ignored.
        assert_eq!(
            Command::parse("time add 100x"),
            Err("Invalid unit \"x\"".to_owned())
        );
        // And a word with no number in it fails on the number, which is the
        // question vanilla asks first.
        assert_eq!(
            Command::parse("time set banana"),
            Err("Expected float, got \"\"".to_owned())
        );
        assert!(Command::parse("time add -1").is_err());
    }

    #[test]
    fn a_query_reads_and_does_not_move_the_clock() {
        let clock = WorldClock::new(500, 3 * daylight::DAY_TICKS + daylight::NOON, true);
        let before = clock.day_time();
        assert_eq!(
            text_of(
                &Command::parse("time query daytime")
                    .expect("parses")
                    .run(&clock)
            ),
            "The time is 6000"
        );
        assert_eq!(
            text_of(
                &Command::parse("time query gametime")
                    .expect("parses")
                    .run(&clock)
            ),
            "The time is 500"
        );
        assert_eq!(
            text_of(
                &Command::parse("time query day")
                    .expect("parses")
                    .run(&clock)
            ),
            "The time is 3"
        );
        assert_eq!(clock.day_time(), before);
    }

    #[test]
    fn setting_answers_with_the_reduced_time_the_way_vanilla_does() {
        let clock = WorldClock::new(0, 0, true);
        let said = Command::parse("time set 30000")
            .expect("parses")
            .run(&clock);
        assert_eq!(text_of(&said), "Set the time to 6000");
        assert_eq!(clock.day_time(), 30_000, "and the total is what is stored");
    }

    #[test]
    fn an_unknown_command_gets_vanillas_own_first_line() {
        assert_eq!(Command::parse("weather clear"), Err(UNKNOWN.to_owned()));
        assert_eq!(Command::parse(""), Err(UNKNOWN.to_owned()));
        assert_eq!(Command::parse("time"), Err(UNKNOWN.to_owned()));
        assert_eq!(Command::parse("time query"), Err(UNKNOWN.to_owned()));
    }
}
