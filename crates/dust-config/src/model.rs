//! The configuration tree. These types are the single definition of
//! `dust.toml`; the reference documentation and the schema are generated from
//! them.

use serde::{Deserialize, Serialize};

use crate::ore::OresConfig;
use crate::{ConfigSection, Finding};

/// The whole of `dust.toml`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
pub struct DustConfig {
    /// Listener, identity and the basics a server needs to answer a ping.
    #[config(section)]
    pub server: ServerConfig,

    /// The embedded JVM that runs plugin bytecode.
    #[config(section)]
    pub jvm: JvmConfig,

    /// World generation: the engine, and Dust's knobs over it.
    #[config(section)]
    pub worldgen: WorldgenConfig,

    /// Where Minecraft's own data lives, for the parts of it Dust may not ship.
    #[config(section)]
    pub data: DataConfig,
}

/// Listener, identity and the basics a server needs to answer a ping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Address to listen on, as `host:port`.
    #[config(restart)]
    pub bind: String,

    /// Message shown in the client's server list.
    pub motd: String,

    /// Maximum concurrent players. This is the number shown in the server list,
    /// not a licence — the gateway may admit more across several backends.
    pub max_players: u32,

    /// Verify each joining player against Mojang's session servers. Turning this
    /// off means anyone may join under any name, and is only safe behind a proxy
    /// that does the check itself.
    #[config(restart)]
    pub online_mode: bool,

    /// The most ticks the server will try to repay after a stall, per pass of
    /// the tick loop. A stall longer than this is skipped past rather than
    /// caught up, which keeps a hiccup from becoming a death spiral.
    #[config(restart)]
    pub max_catchup_ticks: u32,

    /// How long, in seconds, a shutdown may take after a stop request before
    /// the watchdog ends the process by force. Grace worth having is grace
    /// worth bounding.
    #[config(restart)]
    pub shutdown_timeout_secs: u32,

    /// How many columns out from a player are streamed, in every direction. A
    /// view distance of 8 sends 289 columns on join and 2 sends 25, and the
    /// join sends them in one burst: measured against a real world in release,
    /// 25 columns take about half a second of streaming and 289 about two, so
    /// the difference is roughly five milliseconds a column. The client asks
    /// for a distance of its own and is served the smaller of the two, so this
    /// is a ceiling rather than a demand.
    #[config(restart)]
    pub view_distance: u32,

    /// How far, in blocks, a player may break or place a block from — measured
    /// from their eyes to the nearest point of the block, not to its centre.
    /// Beyond it the server ignores the action. Vanilla's own client reaches
    /// 4.5 blocks and its server refuses past 5.5, so anything at or under 5.5
    /// is a request an honest client can make; the extra half block here covers
    /// a position a tick behind the one they acted from. It used to cover a
    /// crouching player's lower eyes as well, and no longer has to: the server
    /// measures from the eye of the pose the player is actually in — see
    /// `dust_guard::Pose` and decision record 0019. Raise it for a modded
    /// client that reaches further;
    /// lowering it below about 5 starts refusing ordinary play.
    #[config(restart)]
    pub interaction_range: f64,

    /// The furthest, in blocks, a player may move in one tick before the server
    /// stops believing them and teleports them back. Measured with a real
    /// client: over 1,217 movement packets covering walking, sprinting,
    /// sprint-jumping, creative flight, a 300-block free fall and a walk through
    /// a 700 ms network stall, the largest single tick was 3.58 blocks, and free
    /// fall's own asymptote is 3.92. Ten is what vanilla's server allows a
    /// player who is not flying an elytra, and the headroom over honest play is
    /// deliberate: knockback, elytra and riptide all move a player faster than
    /// walking. Raise it for a server with movement mods; `inf` turns the check
    /// off. Below 4 it starts correcting players in an ordinary fall.
    #[config(restart)]
    pub movement_speed_limit: f64,

    /// Whether a player may walk into a block. With this on, a movement packet
    /// that puts a player's feet inside a block they were not already inside is
    /// refused and the player is teleported back to the last position they
    /// legitimately reached. A player who is already inside a block — because
    /// somebody placed one on them, or because they spawned in terrain — is
    /// never refused for moving, which is how they get out.
    ///
    /// Only blocks whose collision shape is the whole cube count, so standing
    /// on a stair, a slab, a farmland block or soul sand is not walking into
    /// one; and only the bottom 0.6 of a player is measured, so crawling
    /// through a one-block gap is not either. Turning it off is how an operator
    /// with a movement mod, or a server whose block table predates the
    /// `full_collision` column, gets the old behaviour deliberately rather than
    /// by accident.
    #[config(restart)]
    pub movement_collision: bool,

    /// The lowest severity the server logs: one of `error`, `warn`, `info`,
    /// `debug`, `trace`. Everything less severe than the chosen level is
    /// suppressed.
    #[config(restart)]
    pub log_level: LogLevel,

    /// Which game mode joining players are put in: `survival` or `creative`.
    ///
    /// It is not a cosmetic byte. A creative client removes a block locally the
    /// moment it is clicked and never sends a stop, so on a creative server a
    /// break is instant on both sides and there is nothing to time. A survival
    /// client animates the break itself and waits, so on a survival server the
    /// server times it too, against Minecraft's own hardness and tool speeds —
    /// see `dust_sim::mining` and decision record 0028.
    ///
    /// The default is creative because that is the mode Dust's content is: a
    /// survival player has no crafting table, no furnace, no hunger and no
    /// health, and the only way they get a block is by mining one. Break
    /// timing works either way; what an operator is choosing here is which of
    /// the two half-finished games their players get.
    #[config(restart)]
    pub game_mode: GameMode,

    /// Whether the sun moves. Minecraft's `doDaylightCycle` game rule, as a
    /// setting, because Dust has no game rules yet and this is the one a
    /// server operator actually reaches for.
    ///
    /// On, the world's clock advances one tick per tick and a day takes twenty
    /// minutes. Off, the clock stands where it was left and the client is told
    /// so — the protocol carries that as a negative time of day, so the sun
    /// stops rather than jittering between the packets that would otherwise
    /// keep correcting it, and a build server stays lit all night.
    ///
    /// It does not stop *time*: the world's age still counts up, so anything
    /// that measures elapsed ticks keeps measuring them. It is the sun that
    /// stops, which is what an operator turning this off is asking for.
    ///
    /// This is not a performance setting and turning it off saves nothing
    /// measurable: a running clock costs 2.5 nanoseconds a tick — the same as
    /// a stopped one, which is one atomic addition either way — and eighteen
    /// bytes per player per second, and a stopped cycle still sends the packet
    /// so that a client which has drifted is corrected.
    #[config(restart)]
    pub daylight_cycle: bool,

    /// Path to a directory of `.mca` region files to serve, or empty to
    /// generate a flat world. A column the files do not contain is generated
    /// flat, because a world is a disc in an infinite plane and a player may
    /// walk off the edge of it.
    #[config(restart)]
    pub world_source: String,

    /// Path to the icon shown beside this server in the client's list, or empty
    /// for none. Must be a 64x64 PNG; the client silently shows nothing for a
    /// picture it cannot use, so the server refuses one at boot instead.
    #[config(restart)]
    pub favicon: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:25565".to_owned(),
            motd: "A Dust server".to_owned(),
            max_players: 20,
            online_mode: true,
            max_catchup_ticks: 20,
            shutdown_timeout_secs: 10,
            // Ten, which is Minecraft's own default.
            //
            // It was eight while a join sent every column in one burst, on the
            // reasoning that 441 of them was nearly two seconds of stall where
            // 289 was one. The burst is gone: the loading screen ends after the
            // near twenty-five and the rest arrives from the play loop a batch
            // at a time, so **what a player waits for no longer depends on this
            // number at all**. Measured, screen-to-first-keep-alive at three
            // distances: 404/421 ms at 8, 396/415 ms at 10, 376/394 ms at 12.
            //
            // What it still costs is the streaming behind them — 441 columns
            // finish in about two and a half seconds against 289 in under two —
            // and the memory of holding them. That is a bill a player pays
            // while playing rather than while waiting, which is what makes
            // vanilla's number the right default now and made it the wrong one
            // before.
            view_distance: 10,
            // 5.5 is what vanilla's server refuses past — 4.5 of client reach
            // plus the 1.0 of slack it adds before checking. The extra half
            // block is Dust's own and is there because Dust does not track a
            // player's pose: a crouching player's eyes are 0.35 lower than the
            // 1.62 this measures from, and a position packet may be a tick
            // behind the click that followed it. Half a block covers both with
            // room over, and what it costs is that a cheat reaching 5.9 blocks
            // is not caught — which is not the cheat this exists to stop.
            interaction_range: 6.0,
            // Ten blocks a tick, which is 200 a second and is vanilla's own
            // number for a player who is not flying an elytra.
            //
            // What an honest client actually produces was measured rather than
            // assumed — `tools/bot/movement.js`, 1,217 packets — and the whole
            // distribution sits under 3.6, with everything that is not a free
            // fall under 1.0. So the default is 2.8 times the fastest honest
            // thing on this server today, and that margin is the setting's
            // real content: elytra, riptide, knockback and TNT boosts all move
            // a player faster than walking and none of them exist here yet. A
            // limit tuned to what Dust can do this month is a limit that starts
            // rubber-banding players the month after.
            //
            // What it costs is that a steady 9-blocks-a-tick speed hack is not
            // caught. That is the same hole vanilla has, for the same reason,
            // and it is not the cheat this exists to stop: the one the README
            // names is a client that claims to be somewhere it could not have
            // walked to.
            movement_speed_limit: 10.0,
            // On, for the reason the speed limit is on: the check was measured
            // against a real client before it was believed, and a rule nobody
            // has to turn on is a rule that protects the servers whose
            // operators never read the configuration reference. What it costs
            // is one box of at most eight block cells per movement packet, and
            // on a flat world that is an array index.
            movement_collision: true,
            // On, because a world where the sun does not move is not the game
            // — and because until this existed every Dust server was a
            // permanent midday, which is the thing this setting's default is
            // fixing rather than preserving.
            daylight_cycle: true,
            log_level: LogLevel::default(),
            game_mode: GameMode::default(),
            world_source: String::new(),
            favicon: String::new(),
        }
    }
}

impl ServerConfig {
    /// Everything wrong with this section. See [`DustConfig::check`].
    ///
    /// Both bounds reject only what makes the setting mean the opposite of its
    /// purpose: a zero-tick catch-up allowance skips every stall including the
    /// ordinary ones between two passes, and a zero-second shutdown grace fires
    /// the watchdog before the graceful path has run its first line.
    pub fn check(&self, path: &str, findings: &mut Vec<Finding>) {
        if self.max_catchup_ticks == 0 {
            findings.push(Finding::error(
                format!("{path}.max_catchup_ticks"),
                "must be at least 1; at 0 the loop repays no time at all and \
                 every pass surrenders",
            ));
        }
        if self.shutdown_timeout_secs == 0 {
            findings.push(Finding::error(
                format!("{path}.shutdown_timeout_secs"),
                "must be at least 1; at 0 seconds nothing graceful can finish",
            ));
        }
        if self.view_distance == 0 {
            findings.push(Finding::error(
                format!("{path}.view_distance"),
                "must be at least 1; at 0 a player is sent the column they are                  standing in and nothing else, and the world ends at the chunk                  border",
            ));
        }
        // The floor is where the setting starts refusing ordinary play rather
        // than cheating, and it is not a taste either: a standing player's eyes
        // are 1.62 above their feet, so under 1.63 they cannot break the ground
        // they are standing on. Five is the first whole number that leaves a
        // player their full vanilla reach in every direction.
        if !self.interaction_range.is_finite() || self.interaction_range < 5.0 {
            findings.push(Finding::error(
                format!("{path}.interaction_range"),
                "must be a number of at least 5; below that the server starts                  refusing blocks a player can legitimately reach, and at 1.62                  or less they cannot break the ground under their own feet",
            ));
        }
        // The floor is free fall. A player who steps off a cliff reaches 3.92
        // blocks a tick and stays there, so a limit under that corrects
        // somebody for falling — which is the exact failure this setting is
        // most able to cause and least able to be forgiven for. Four is the
        // first whole number above it. `inf` is deliberately allowed, and is
        // how an operator turns the check off.
        if self.movement_speed_limit.is_nan() || self.movement_speed_limit < 4.0 {
            findings.push(Finding::error(
                format!("{path}.movement_speed_limit"),
                "must be a number of at least 4, or inf to turn the check off; \
                 a falling player moves 3.92 blocks a tick, so anything less \
                 teleports players back for falling",
            ));
        }
        // A ceiling rather than a taste. 32 is vanilla's own maximum and 65x65
        // columns is already four thousand in one burst; past it the number is
        // not a view distance, it is a way to make a join never finish.
        if self.view_distance > 32 {
            findings.push(Finding::error(
                format!("{path}.view_distance"),
                "must be at most 32, which is Minecraft's own maximum; beyond                  that a join sends more columns than it can finish",
            ));
        }
    }
}

/// Which game mode joining players are put in.
///
/// An enum for the reason [`LogLevel`] is one: a typo fails at parse time
/// naming the field rather than at play time as a server that quietly serves
/// the wrong game.
///
/// The two modes Dust serves are the two whose *break* behaviour differs. A
/// creative client breaks locally and tells the server after the fact; a
/// survival client animates a break of a length it computes itself and expects
/// the server to have computed the same one. Adventure and spectator are not
/// here because neither is a mode Dust can honour yet — adventure needs
/// `can_break` on a stack's components and spectator needs a camera that
/// passes through blocks — and a byte that says a mode the server does not
/// implement is worse than no setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GameMode {
    /// Blocks take time to break, a wrong tool costs that time, and what a
    /// break yields is the loot table's answer.
    Survival,
    /// Every block comes away on the first click and yields nothing, which is
    /// what a creative client draws locally before the server is asked.
    #[default]
    Creative,
}

impl GameMode {
    /// The byte the join and respawn packets carry.
    #[must_use]
    pub fn wire_id(self) -> u8 {
        match self {
            Self::Survival => 0,
            Self::Creative => 1,
        }
    }
}

impl std::fmt::Display for GameMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Survival => "survival",
            Self::Creative => "creative",
        })
    }
}

/// How loudly the server logs.
///
/// An enum rather than a free string so a typo fails at parse time naming the
/// field, exactly like every other typed setting; a string would move the same
/// mistake into validation where it reads as a second-class value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        };
        f.write_str(name)
    }
}

/// The embedded JVM that runs plugin bytecode.
///
/// Turning this off must leave every other feature working. That is not a
/// courtesy, it is the standing test that keeps game logic out of Java — see
/// decision record 0005.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
pub struct JvmConfig {
    /// Start the embedded JVM and load plugins. With this off, Dust runs with no
    /// JVM in the process at all and everything except plugins still works.
    #[config(restart)]
    pub enabled: bool,

    /// Heap ceiling for the embedded JVM, in mebibytes.
    #[config(restart)]
    pub max_heap_mib: u32,
}

impl Default for JvmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_heap_mib: 1024,
        }
    }
}

/// World generation: the engine, and Dust's knobs over it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
pub struct WorldgenConfig {
    /// The seed a generated world is built from.
    ///
    /// Only used when there is no world to read a seed out of: a server with
    /// `[server] world_source` set takes the seed from that world's own
    /// `level.dat`, because generating the columns off its edge from a
    /// different one would put a cliff where the disc ends.
    ///
    /// The default is zero rather than a random number. Two servers started
    /// from the same configuration should serve the same world, and an
    /// operator who wants a different one should be able to say which.
    #[config(restart)]
    pub seed: i64,

    /// How common each ore is, and where it generates.
    #[config(section)]
    pub ores: OresConfig,
}

/// Where Minecraft's own data lives.
///
/// Dust ships no Mojang content. The names of the datapack registries are
/// facts about a protocol and are generated into the build; the *contents* of
/// an entry — a biome's colours, a dimension's height — are Mojang's, and they
/// come from a copy the operator already has. Decision record 0007 has the
/// reasoning and record 0006 has the precedent.
///
/// Leaving this unset costs two things. A client that acknowledges no data
/// packs cannot be served, because it has no copy of its own to fall back on —
/// vanilla clients acknowledge `minecraft:core` and are unaffected. And a
/// served world's sky light stops at the surface of an ocean and under a tree,
/// because the block-state light table is read from here too.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ConfigSection)]
#[serde(default, deny_unknown_fields)]
pub struct DataConfig {
    /// Directory holding Minecraft's data in the usual datapack layout — the
    /// one containing `minecraft/`, which is `data/` inside a datapack. Unset
    /// means Dust has no registry contents to send.
    ///
    /// Dust also looks here for the two tables `cargo xtask extract --only
    /// constants` writes out of your own server jar, both of them things
    /// Minecraft keeps as Java code rather than as data.
    /// `dust-constants.tsv` says what a block state does — how much light it
    /// stops, how much it gives off, which heightmaps count it, whether a block
    /// placed there goes into it, and what it sounds like going down. `dust-items.tsv` says which block each item
    /// puts down. Both optional: without the first, every block but air stops
    /// sky light, the sky floor sits above the grass, a placed block is silent,
    /// and a right-click replaces whatever is on the face it clicked; without the second, a right-click places the world's own
    /// surface block whatever the player is holding. With them, the sky light
    /// Dust serves is the light Minecraft computes — exactly on an ocean world
    /// and within 0.02% of cells inland — and a player places what they are
    /// holding. A file that is there and unreadable stops the server.
    #[config(restart)]
    pub path: Option<String>,
}
