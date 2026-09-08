//! The lifecycle: ordered start, observed stop, symmetric shutdown.
//!
//! # The shape of a boot
//!
//! ```text
//! run() ─► 1. config.load      read + validate dust.toml (the Phase 0.3 path)
//!          2. world.ensure     create world directories
//!          3. network.bind*    validate/resolve [server].bind — placeholder,
//!                              dust-net will own the real socket later
//!          4. tick.loop        fixed-timestep engine over participants
//!                 ▲                    │
//!                 └── ctrl-C / watchdog-requested stop, checked BETWEEN passes
//!
//! then, in exact reverse:
//!          4. tick.stop        final stats captured
//!          3. network.release  placeholder released
//!          2. world.release    directories left on disk, noted honestly
//!          1. config.release   configuration released
//! ```
//!
//! Three properties are enforced rather than hoped for.
//!
//! **Symmetry.** Every phase whose start lands in the [`Transcript`] gets a
//! stop beside it, newest first, whether shutdown is graceful, interrupted,
//! or caused by a failure. A phase that fails does not get a start entry at
//! all, and only completed phases are torn down — you cannot release what
//! never began.
//!
//! **Between-pass stopping.** The stop flag is an atomic plus a condvar; it
//! never interrupts anyone. The loop checks it between passes, so a tick
//! batch always finishes whole. Worst-case shutdown latency is one batch;
//! that trade is deliberate, because corrupting a half-written world update
//! to save 50 ms is not a trade anyone would sign twice.
//!
//! **No leaked threads.** Every thread spawned here is tracked by name and
//! joined before `run` returns; the report says how many were joined and
//! whether any died. The guarantee has teeth because it is measured.
//!
//! All time — deadlines, grace periods, uptime — reads from the injected
//! [`Clock`](crate::clock::Clock). Production injects a monotonic clock;
//! tests inject a manual one, and the code cannot tell the difference.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;

use dust_config::model::LogLevel;
use dust_config::{ConfigError, DustConfig};

use crate::clock::{Clock, ManualClock, MonotonicClock};
use crate::engine::TickEngine;
use crate::histogram::TimingStats;
use crate::logging::{Level, Logger};
use crate::participant::{ParticipantSet, TickParticipant};
use crate::stop::{
    watch_dog, CondvarParker, Parker, StopHandle, StopState, ThreadTracker, WatchdogHarness,
    WatchdogPolicy,
};
use crate::tasks;

/// Default configuration file, relative to the working directory.
pub const DEFAULT_CONFIG_PATH: &str = "dust.toml";
/// Default world directory, relative to the working directory.
///
/// Placeholder until the schema gains a level-name setting; the constant
/// exists so there is exactly one place to change.
pub const DEFAULT_WORLD_DIR: &str = "world";

/// The runtime settings phases 4 and 5 consume, extracted from the loaded
/// configuration in one place.
///
/// Phase 1 loads the file; everything downstream reads these numbers instead
/// of reaching back into the config tree. One extraction point means the
/// mapping from setting to behaviour is written once and testable alone.
#[derive(Debug, Clone, Copy)]
struct RuntimeSettings {
    /// `[server].max_catchup_ticks`, verbatim.
    catchup_cap: u32,
    /// `[server].shutdown_timeout_secs`, converted to nanoseconds on whatever
    /// clock the run uses.
    shutdown_grace_ns: u64,
}

impl RuntimeSettings {
    fn from_config(config: &DustConfig) -> Self {
        Self {
            catchup_cap: config.server.max_catchup_ticks,
            shutdown_grace_ns: u64::from(config.server.shutdown_timeout_secs) * 1_000_000_000,
        }
    }
}

/// Map a configured log level onto the logger's severity scale.
fn log_level_of(level: LogLevel) -> Level {
    match level {
        LogLevel::Error => Level::Error,
        LogLevel::Warn => Level::Warn,
        LogLevel::Info => Level::Info,
        LogLevel::Debug => Level::Debug,
        LogLevel::Trace => Level::Trace,
    }
}

/// One named stage of the boot sequence, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    ConfigLoad,
    WorldDirs,
    NetworkBind,
    TickLoop,
}

impl Phase {
    /// Short name used in transcripts and logs.
    pub fn label(self) -> &'static str {
        match self {
            Self::ConfigLoad => "config",
            Self::WorldDirs => "world-dirs",
            Self::NetworkBind => "network",
            Self::TickLoop => "tick-loop",
        }
    }

    /// What tearing this phase down means, for transcripts. The wording is
    /// deliberately honest about what a placeholder does and does not do.
    fn teardown_detail(self, ticks_run: u64) -> String {
        match self {
            Self::ConfigLoad => "configuration released".to_owned(),
            Self::WorldDirs => "directories left on disk".to_owned(),
            // Overridden by the teardown, which knows the address and the
            // counters. This is the wording for a caller that unwinds a phase
            // list without a listener in hand.
            Self::NetworkBind => "listener released".to_owned(),
            Self::TickLoop => format!("stopped after {ticks_run} tick(s)"),
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Which way a transcript line faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The phase began (and succeeded — failures never get a start line).
    Start,
    /// The phase was torn down.
    Stop,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Start => "start",
            Self::Stop => "stop",
        })
    }
}

/// One line of the lifecycle transcript.
///
/// Transcripts exist because ordering claims are cheap to make and expensive
/// to trust. "Shutdown is symmetric" is a sentence; a transcript reading
/// `start config … stop config` is evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub phase: Phase,
    pub direction: Direction,
    /// What actually happened, for humans reading the report.
    pub detail: String,
}

impl fmt::Display for TranscriptEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}: {}", self.direction, self.phase, self.detail)
    }
}

/// Everything that can end a boot badly.
///
/// The phase failures carry the transcript as it stood when they fired, so a
/// caller can show (or a test can assert) exactly which phases ran and were
/// unwound. Configuration failure carries none because configuration is the
/// first phase: nothing has started, so nothing can have been unwound.
#[derive(Debug)]
pub enum ServerError {
    /// The configuration failed to load or validate.
    Config(ConfigError),
    /// The world directories could not be created.
    WorldDirs {
        path: PathBuf,
        source: std::io::Error,
        transcript: Vec<TranscriptEntry>,
    },
    /// `[server] bind` did not describe a usable address.
    NetworkBind {
        bind: String,
        message: String,
        transcript: Vec<TranscriptEntry>,
    },
    /// A tracked thread died during shutdown.
    ThreadPanic(Vec<String>),
}

impl ServerError {
    /// The boot transcript at the moment of failure: every phase that had
    /// started, plus the stop entries written while unwinding it.
    pub fn transcript(&self) -> &[TranscriptEntry] {
        match self {
            Self::Config(_) | Self::ThreadPanic(_) => &[],
            Self::WorldDirs { transcript, .. } | Self::NetworkBind { transcript, .. } => transcript,
        }
    }

    /// Attach the boot transcript to a phase failure. Called by [`Server::run`]
    /// *after* unwinding, so the error carries the stops as well as the starts.
    fn attach(&mut self, transcript: Vec<TranscriptEntry>) {
        match self {
            Self::Config(_) | Self::ThreadPanic(_) => {}
            Self::WorldDirs {
                transcript: slot, ..
            }
            | Self::NetworkBind {
                transcript: slot, ..
            } => *slot = transcript,
        }
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(e) => write!(f, "{e}"),
            Self::WorldDirs { path, source, .. } => {
                write!(f, "could not prepare world directory {path:?}: {source}")
            }
            Self::NetworkBind { bind, message, .. } => {
                write!(f, "[server] bind = {bind:?}: {message}")
            }
            Self::ThreadPanic(names) => {
                write!(
                    f,
                    "thread(s) panicked during shutdown: {}",
                    names.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for ServerError {}

/// Live counters an outside observer can poll while the server runs.
///
/// This is what lets a test say "wait until three ticks have happened"
/// without sleeping, guessing or reaching into internals.
#[derive(Clone)]
pub struct LiveMetrics {
    ticks_observed: Arc<AtomicU64>,
    stop: Arc<StopState>,
    bound: Arc<OnceLock<SocketAddr>>,
}

impl LiveMetrics {
    /// Ticks completed so far, published by the loop between passes.
    pub fn ticks_observed(&self) -> u64 {
        self.ticks_observed.load(Ordering::SeqCst)
    }

    /// Whether anyone has requested a stop.
    pub fn is_stop_requested(&self) -> bool {
        self.stop.is_stopped()
    }

    /// The address the listener actually took, once phase 3 has completed.
    ///
    /// Not the same as `[server].bind`, and the difference is the reason this
    /// exists: `0.0.0.0:0` and `127.0.0.1:0` both mean "any free port", and the
    /// number the operating system chose is knowable only after the bind. An
    /// operator wants it in the log and a test wants it to connect to; both
    /// would otherwise have to parse it back out of a log line.
    ///
    /// A `OnceLock` because a listener binds once per run and never rebinds, so
    /// "not yet" and "never" are the same answer and neither is a lock anybody
    /// contends for.
    pub fn bound_addr(&self) -> Option<SocketAddr> {
        self.bound.get().copied()
    }
}

impl fmt::Debug for LiveMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveMetrics")
            .field("ticks_observed", &self.ticks_observed())
            .field("is_stop_requested", &self.is_stop_requested())
            .field("bound_addr", &self.bound_addr())
            .finish()
    }
}

/// How the tick loop's parkers are built, per run.
///
/// Factories rather than instances because each run needs its own parker,
/// two different threads need two of them, and some parkers carry state.
pub type ParkerFactory =
    Arc<dyn Fn(Arc<StopState>, Arc<dyn Clock>) -> Box<dyn Parker> + Send + Sync>;

/// What the watchdog thread should be, per run.
///
/// The default is [`WatchdogSetting::FromConfig`]: the grace period is
/// `[server].shutdown_timeout_secs` from the loaded file, which is how that
/// setting reaches a thread that starts after configuration has been read.
/// Tests and embedded hosts either name an explicit policy or switch the
/// watchdog off entirely.
#[derive(Clone, Debug, Default)]
pub enum WatchdogSetting {
    /// Build the policy from the loaded configuration's timeout.
    #[default]
    FromConfig,
    /// No watchdog. Nothing enforces the shutdown deadline; only do this when
    /// something else owns it.
    Disabled,
    /// An explicit policy, overriding whatever the file says.
    Custom(WatchdogPolicy),
}

/// Everything configurable about a server run.
///
/// Defaults describe a normal production boot: monotonic clock, condvar
/// parking, a config-driven process-exiting watchdog, info-level logs on
/// stdout until the file says otherwise. Tests override pieces individually
/// and leave the rest honest.
pub struct ServerOptions {
    pub config_path: PathBuf,
    pub world_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    /// Builds the parker the tick loop owns.
    pub loop_parker: ParkerFactory,
    /// Builds the parker the watchdog thread owns.
    pub watchdog_parker: ParkerFactory,
    /// What watchdog to run across the tick loop and teardown.
    pub watchdog: WatchdogSetting,
    pub logger: Logger,
    /// Participants registered on top of the ones built from configuration.
    pub extra_tasks: Vec<Box<dyn TickParticipant>>,
    /// When set, config-built participants that simulate work charge *this*
    /// clock. It must be the same manual clock as `clock`, otherwise the
    /// measurements are lies. Production leaves it `None`: real work charges
    /// real time without help.
    pub virtual_work_clock: Option<Arc<ManualClock>>,
}

impl fmt::Debug for ServerOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerOptions")
            .field("config_path", &self.config_path)
            .field("world_dir", &self.world_dir)
            .field(
                "watchdog",
                match &self.watchdog {
                    WatchdogSetting::Disabled => &"disabled",
                    WatchdogSetting::FromConfig => &"from-config",
                    WatchdogSetting::Custom(_) => &"custom",
                },
            )
            .field("extra_tasks", &self.extra_tasks.len())
            .finish_non_exhaustive()
    }
}

impl Default for ServerOptions {
    fn default() -> Self {
        let clock: Arc<dyn Clock> = Arc::new(MonotonicClock::new());
        // Anchored once, here: the monotonic clock reads from process start,
        // and log lines should show the calendar, not the uptime.
        let logger = Logger::to_stdout(Level::Info, Arc::clone(&clock)).anchored_to_unix_now();
        let loop_parker: ParkerFactory =
            Arc::new(|state, clock| Box::new(CondvarParker::new(state, clock)));
        Self {
            config_path: PathBuf::from(DEFAULT_CONFIG_PATH),
            world_dir: PathBuf::from(DEFAULT_WORLD_DIR),
            loop_parker: Arc::clone(&loop_parker),
            watchdog_parker: loop_parker,
            watchdog: WatchdogSetting::default(),
            logger,
            extra_tasks: Vec::new(),
            virtual_work_clock: None,
            clock,
        }
    }
}

/// Shared mutable state between the server and its helper threads.
struct Shared {
    stop: Arc<StopState>,
    complete: Arc<AtomicBool>,
    ticks_observed: Arc<AtomicU64>,
    watchdog_fired: Arc<AtomicBool>,
    /// Published by phase 3 the moment the socket is taken. See
    /// [`LiveMetrics::bound_addr`].
    bound: Arc<OnceLock<SocketAddr>>,
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field(
                "ticks_observed",
                &self.ticks_observed.load(Ordering::SeqCst),
            )
            .finish_non_exhaustive()
    }
}

/// A configured-but-not-yet-run server.
///
/// Construct once, clone the [`StopHandle`] out to whoever should be allowed
/// to stop it, then hand the whole thing to a thread (or run it inline) with
/// [`run`](Server::run).
pub struct Server {
    options: ServerOptions,
    /// Loaded by phase 1, consumed by phase 4. The slot exists so each phase
    /// reads exactly what earlier phases produced, in order.
    config: Option<DustConfig>,
    shared: Shared,
    stop_handle: StopHandle,
    tracker: Arc<ThreadTracker>,
    /// Set by phase 3, released by the teardown at phase 3's position. Held on
    /// the server rather than in a local so that the failure paths and the
    /// happy path release it through the same code.
    listener: Option<crate::net::ListenerHandle>,
    /// The world and the player positions, kept so the teardown can write them
    /// down. `None` until phase 3 builds them.
    saveable: Option<Saveable>,
    /// Item physics, built by phase 3 because it needs the world and the
    /// roster, and inserted into the tick loop by phase 4 because that is
    /// where ticking happens. The slot is what lets those be different phases.
    item_ticker: Option<Box<dyn crate::participant::TickParticipant>>,
    /// The furnaces' ticker, stashed for the same reason: it needs the world,
    /// which the bind phase builds, and it runs in the tick phase.
    furnace_ticker: Option<Box<dyn crate::participant::TickParticipant>>,
    /// Block updates: what cannot stay, what falls, and what lands. Built and
    /// inserted in the same two phases and for the same reason as the item
    /// physics beside it.
    world_ticker: Option<Box<dyn crate::participant::TickParticipant>>,
    /// The world's clock. Same two phases again: it is restored from the save
    /// the bind phase reads, and it only moves once there is a tick loop.
    daylight_ticker: Option<Box<dyn crate::participant::TickParticipant>>,
}

/// What the teardown has to write out.
struct Saveable {
    world: crate::net::SharedWorld,
    positions: crate::net::save::SharedPositions,
    inventories: crate::net::save::SharedInventories,
    furnaces: std::sync::Arc<crate::net::furnaces::Furnaces>,
    clock: std::sync::Arc<crate::net::daylight::WorldClock>,
    world_dir: PathBuf,
    /// The region directory this server was pointed at, when it was pointed at
    /// one. `level.dat` sits beside it, and the clock is written back into it
    /// so that a world Dust served for a week opens in vanilla at the time it
    /// was left rather than the time it was imported.
    region_dir: Option<PathBuf>,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("shared", &self.shared)
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Prepare a server around `options`. Construction wires handles together
    /// and touches nothing else — no files are opened until `run`.
    pub fn new(options: ServerOptions) -> Self {
        // Before anything can decode a packet. `dust-protocol` sits below
        // `dust-registry` in the dependency graph and cannot name a data
        // component type on its own, so the name lookup is handed to it here,
        // out of the registry the operator's own jar produced. Until it is,
        // every component on the wire is refused by number rather than
        // guessed at.
        crate::net::inventory::install_component_types();
        let shared = Shared {
            stop: Arc::new(StopState::default()),
            complete: Arc::new(AtomicBool::new(false)),
            ticks_observed: Arc::new(AtomicU64::new(0)),
            watchdog_fired: Arc::new(AtomicBool::new(false)),
            bound: Arc::new(OnceLock::new()),
        };
        let stop_handle = StopHandle::new(Arc::clone(&shared.stop));
        Self {
            options,
            config: None,
            shared,
            stop_handle,
            tracker: Arc::new(ThreadTracker::default()),
            listener: None,
            saveable: None,
            item_ticker: None,
            furnace_ticker: None,
            world_ticker: None,
            daylight_ticker: None,
        }
    }

    /// The handle a signal handler (or a test playing one) uses to request
    /// shutdown.
    pub fn stop_handle(&self) -> StopHandle {
        self.stop_handle.clone()
    }

    /// Counters an outside thread can poll while `run` is in flight.
    pub fn metrics(&self) -> LiveMetrics {
        LiveMetrics {
            ticks_observed: Arc::clone(&self.shared.ticks_observed),
            stop: Arc::clone(&self.shared.stop),
            bound: Arc::clone(&self.shared.bound),
        }
    }

    /// Execute the full lifecycle, blocking until shutdown completes.
    ///
    /// `Ok(report)` means the lifecycle ran through to the end — inspect the
    /// report for whether it was clean. `Err` means startup failed and every
    /// phase that had completed was unwound in reverse before returning.
    pub fn run(mut self) -> Result<ShutdownReport, ServerError> {
        let mut transcript: Vec<TranscriptEntry> = Vec::new();
        let mut completed: Vec<Phase> = Vec::new();
        let started_at = self.options.clock.now_ns();

        // ---- Phase 1: configuration ------------------------------------
        match self.start_config_load(&mut transcript) {
            Ok(()) => {}
            Err(e) => return Err(e), // nothing has started yet; nothing to undo
        }
        completed.push(Phase::ConfigLoad);

        // Configuration is in; everything downstream takes its numbers from
        // it. The log filter applies from here on — phases 2 and later speak
        // at the loudness the file asked for.
        let staged = self.config.as_ref().expect("config staged by phase 1");
        let runtime = RuntimeSettings::from_config(staged);
        self.options.logger = self
            .options
            .logger
            .with_filter(log_level_of(staged.server.log_level));
        if self.shared.stop.is_stopped() {
            let transcript = std::mem::take(&mut transcript);
            return Ok(self.abort_startup(completed, transcript, started_at));
        }

        // ---- Phase 2: world directories --------------------------------
        if let Err(mut e) = self.start_world_dirs(&mut transcript) {
            let mut listener = self.listener.take();
            let mut saveable = self.saveable.take();
            self.teardown(completed, &mut transcript, 0, &mut listener, &mut saveable);
            e.attach(std::mem::take(&mut transcript));
            return Err(e);
        }
        completed.push(Phase::WorldDirs);
        if self.shared.stop.is_stopped() {
            {
                let transcript = std::mem::take(&mut transcript);
                return Ok(self.abort_startup(completed, transcript, started_at));
            }
        }

        // ---- Phase 3: bind and serve -----------------------------------
        if let Err(mut e) = self.start_network(&mut transcript) {
            let mut listener = self.listener.take();
            let mut saveable = self.saveable.take();
            self.teardown(completed, &mut transcript, 0, &mut listener, &mut saveable);
            e.attach(std::mem::take(&mut transcript));
            return Err(e);
        }
        completed.push(Phase::NetworkBind);
        if self.shared.stop.is_stopped() {
            {
                let transcript = std::mem::take(&mut transcript);
                return Ok(self.abort_startup(completed, transcript, started_at));
            }
        }

        // ---- Phase 4: the tick loop ------------------------------------
        let config = self.config.take().expect("config staged by phase 1");
        let mut participants = tasks::registry_from_config(
            &config,
            &tasks::WorkCharger::from_option(self.options.virtual_work_clock.clone()),
        );
        if let Some(ticker) = self.world_ticker.take() {
            participants.insert(ticker);
        }
        if let Some(ticker) = self.item_ticker.take() {
            participants.insert(ticker);
        }
        if let Some(ticker) = self.furnace_ticker.take() {
            participants.insert(ticker);
        }
        if let Some(ticker) = self.daylight_ticker.take() {
            participants.insert(ticker);
        }
        for extra in std::mem::take(&mut self.options.extra_tasks) {
            participants.insert(extra);
        }
        let participant_names = participants.names();
        transcript.push(TranscriptEntry {
            phase: Phase::TickLoop,
            direction: Direction::Start,
            detail: format!("{} participant(s)", participant_names.len()),
        });

        // From here on the watchdog watches: armed by the stop request, it
        // enforces the deadline across the loop and the whole teardown. The
        // grace period comes from configuration unless the run was given an
        // explicit policy or told to go without.
        let policy = match &self.options.watchdog {
            WatchdogSetting::Disabled => None,
            WatchdogSetting::Custom(policy) => Some(policy.clone()),
            WatchdogSetting::FromConfig => {
                Some(WatchdogPolicy::exit_process(runtime.shutdown_grace_ns))
            }
        };
        if let Some(policy) = &policy {
            let harness = WatchdogHarness {
                stop: Arc::clone(&self.shared.stop),
                complete: Arc::clone(&self.shared.complete),
                fired: Arc::clone(&self.shared.watchdog_fired),
                ticks_run: Arc::clone(&self.shared.ticks_observed),
                clock: Arc::clone(&self.options.clock),
                parker: (self.options.watchdog_parker)(
                    Arc::clone(&self.shared.stop),
                    Arc::clone(&self.options.clock),
                ),
                policy: policy.clone(),
            };
            let tracker = Arc::clone(&self.tracker);
            tracker.spawn("dust-watchdog", move || watch_dog(harness));
        }

        let summary = self.run_tick_loop(&mut participants, runtime.catchup_cap);
        completed.push(Phase::TickLoop);

        // ---- Shutdown: exact reverse of everything completed ------------
        let mut listener = self.listener.take();
        let mut saveable = self.saveable.take();
        self.teardown(
            completed,
            &mut transcript,
            summary.ticks_run,
            &mut listener,
            &mut saveable,
        );

        // Release the watchdog first so a not-yet-fired one retires quietly;
        // one that already fired has already done its worst. The extra wake
        // matters under virtual time: nothing else moves the clock after the
        // loop exits, so without this the watcher would sleep out its whole
        // grace period on a clock that will never reach it.
        self.shared.complete.store(true, Ordering::SeqCst);
        self.shared.stop.broadcast_stop();
        let (joined, panicked) = self.tracker.join_all();

        Ok(ShutdownReport {
            interrupted: false,
            ticks_run: summary.ticks_run,
            uptime_ns: self.options.clock.now_ns().saturating_sub(started_at),
            surrendered_batches: summary.surrendered_batches,
            overall_timing: summary.overall_timing,
            participant_timing: summary.participant_timing,
            participants: participant_names,
            transcript,
            watchdog_fired: self.shared.watchdog_fired.load(Ordering::SeqCst),
            shutdown_grace_ns: policy.map(|_| runtime.shutdown_grace_ns),
            threads_joined: joined.len(),
            thread_panics: panicked,
        })
    }

    // ---- phases ----------------------------------------------------------

    fn start_config_load(
        &mut self,
        transcript: &mut Vec<TranscriptEntry>,
    ) -> Result<(), ServerError> {
        match DustConfig::load(&self.options.config_path) {
            Ok(config) => {
                let warnings = config
                    .check()
                    .into_iter()
                    .filter(|f| f.severity == dust_config::Severity::Warning)
                    .count();
                let origin = self.options.config_path.display();
                let server = &config.server;
                transcript.push(TranscriptEntry {
                    phase: Phase::ConfigLoad,
                    direction: Direction::Start,
                    detail: format!(
                        "loaded {origin} ({warnings} warning(s)); defaults applied \
                         for anything unset; catch-up capped at {} tick(s), \
                         shutdown grace {}s, logs at {}, bind {}",
                        server.max_catchup_ticks,
                        server.shutdown_timeout_secs,
                        server.log_level,
                        server.bind,
                    ),
                });
                self.config = Some(config);
                Ok(())
            }
            Err(e) => {
                self.options.logger.error("dust::server", format!("{e}"));
                Err(ServerError::Config(e))
            }
        }
    }

    fn start_world_dirs(&self, transcript: &mut Vec<TranscriptEntry>) -> Result<(), ServerError> {
        let dir = self.options.world_dir.clone();
        match std::fs::create_dir_all(&dir) {
            Ok(()) => {
                transcript.push(TranscriptEntry {
                    phase: Phase::WorldDirs,
                    direction: Direction::Start,
                    detail: format!("{} ready", dir.display()),
                });
                Ok(())
            }
            Err(source) => {
                self.options.logger.error(
                    "dust::server",
                    format!("world directory {} failed: {source}", dir.display()),
                );
                Err(ServerError::WorldDirs {
                    path: dir,
                    source,
                    transcript: Vec::new(),
                })
            }
        }
    }

    /// Resolve `[server] bind`, take the port, and start serving on it.
    ///
    /// The socket is bound **here**, synchronously, inside the ordered boot —
    /// not inside the task that accepts on it. That is the whole point of this
    /// phase existing where it does. A port already in use, an address the
    /// machine does not have, a privileged port without the privilege: each is
    /// an error that stops the boot with the setting named, the earlier phases
    /// unwound in reverse, and a non-zero exit. Bound from inside a spawned
    /// task instead, every one of them would produce a server that started
    /// cleanly, ticked forever and answered nothing.
    ///
    /// The favicon is read here too, for the same reason and one more: a client
    /// shows *nothing* for a picture it cannot use, which is indistinguishable
    /// from a server that set none. An operator who points the setting at the
    /// wrong file has to be told, and boot is the only moment anybody is
    /// listening.
    fn start_network(&mut self, transcript: &mut Vec<TranscriptEntry>) -> Result<(), ServerError> {
        let config = self
            .config
            .as_ref()
            .expect("config staged before network phase");
        let bind = config.server.bind.clone();
        let motd = config.server.motd.clone();
        let max_players = config.server.max_players;
        let favicon_path = config.server.favicon.clone();
        let world_source = config.server.world_source.clone();
        let online_mode = config.server.online_mode;
        let view_distance = config.server.view_distance;
        let reach = dust_guard::Reach::new(config.server.interaction_range);
        let speed = dust_guard::SpeedLimit::new(config.server.movement_speed_limit);
        let collision = config.server.movement_collision;
        let daylight_cycle = config.server.daylight_cycle;
        let data_path = config.data.path.clone();
        let configured_seed = config.worldgen.seed;

        let fail = |message: String| -> ServerError {
            ServerError::NetworkBind {
                bind: bind.clone(),
                message,
                transcript: Vec::new(),
            }
        };

        let addr = resolve_bind(&bind).map_err(&fail)?;

        let favicon = if favicon_path.is_empty() {
            None
        } else {
            let icon = crate::net::Favicon::load(std::path::Path::new(&favicon_path))
                .map_err(|e| fail(e.to_string()))?;
            Some(icon)
        };

        // The one version this server speaks. It is resolved by name from the
        // generated table rather than written as a constant, so that the day
        // there are two, this line is where the choice becomes visible instead
        // of being spread through the code that assumed one.
        let version = dust_protocol::version::V1_21_1;

        // Both of these are done *before* the socket exists, because both can
        // fail and a failure after the bind leaves a port taken by a server
        // that is about to stop. Key generation in particular is slow and
        // unbounded — RSA prime search has no worst case — which is exactly
        // why it happens once here and never on a login.
        let authority = if online_mode {
            let transport = dust_net::session::TlsTransport::mojang().map_err(|e| {
                fail(format!(
                    "online mode needs to reach Mojang's session server and could not: {e}. \
                     Set [server] online_mode = false to run without verification, knowing \
                     that anyone may then join under any name"
                ))
            })?;
            let key = dust_net::login::ServerKey::generate()
                .map_err(|e| fail(format!("online mode needs a server key pair: {e}")))?;
            crate::net::Authority::Online {
                session: std::sync::Arc::new(dust_net::session::HttpSessionServer::new(transport)),
                key: std::sync::Arc::new(key),
            }
        } else {
            self.options.logger.warn(
                "dust::server",
                "[server] online_mode = false: nobody is verified and anyone may join \
                 under any name. Safe only behind a proxy that checks for you.",
            );
            crate::net::Authority::Offline
        };

        // The world, and the two registry positions the join packet quotes.
        // Both are looked up in the same synced tables the configuration state
        // sends, because an id here is a *position in what the client was
        // told* — a constant would be a second answer to a question the sync
        // already answers, and the two would disagree the day a registry gains
        // an entry.
        let palette = crate::net::world::Palette::resolve().map_err(|e| fail(e.to_string()))?;
        let biomes = dust_registry::synced::by_name("minecraft:worldgen/biome")
            .ok_or_else(|| fail("the synced registries have no biome registry".to_owned()))?;
        let plains = biomes.id_of("minecraft:plains").ok_or_else(|| {
            fail("the biome registry has no minecraft:plains to build a flat world from".to_owned())
        })? as u32;
        let dimension_types = dust_registry::synced::by_name("minecraft:dimension_type")
            .ok_or_else(|| fail("the synced registries have no dimension types".to_owned()))?;
        let overworld = dimension_types
            .id_of("minecraft:overworld")
            .ok_or_else(|| fail("the dimension types have no overworld".to_owned()))?
            as u32;
        let flat = crate::net::world::FlatWorld::new(palette, plains, biomes.entries.len() as u32);

        // What Minecraft says about every block state — opacity, emission, and
        // which heightmaps count it — if the operator put a table beside their
        // data. Read before the world is built because the world is both lit
        // and heightmapped with it, and read here rather than at the first
        // column for the same reason the region directory is checked here: a
        // mistake in it should stop a server starting, not surprise the first
        // player who looks at an ocean.
        //
        // Decision record 0008 is why this arrives from `[data] path` rather
        // than from a table in this repository, and `crate::registries::constants`
        // is where the route itself is argued.
        let constants = match data_path.as_deref() {
            None => None,
            Some(path) => {
                crate::registries::constants::beside(path).map_err(|e| fail(format!("{e}")))?
            }
        };
        match &constants {
            Some(table) => self.options.logger.info(
                "dust::data",
                format!(
                    "block constants: Minecraft's own answers for {} block states, \
                     {} of them emitting, {} heightmap predicate(s), {}, {}, {}, and {}",
                    table.len(),
                    table.emitting(),
                    table.flags().count(),
                    // Both ends of this one read as a sentence rather than a
                    // number, because "everything" is what an absent column
                    // means and no version of Minecraft says it.
                    if table.has_replaceable() {
                        format!("{} a placement replaces", table.replaceable_count())
                    } else {
                        "no replaceable column, so a placement replaces whatever it \
                         lands on"
                            .to_owned()
                    },
                    // Named apart from the rest because the answer is not
                    // always yes: a table extracted before the sound columns
                    // existed is still a whole table and still lights the world
                    // correctly, and the operator whose blocks go down in
                    // silence needs to be told which of the two things is
                    // missing — the file, or three columns of it.
                    if table.has_place_sounds() {
                        format!("{} block sound group(s)", table.sound_groups())
                    } else {
                        "no place sounds, so a placed block is silent; re-run \
                         `cargo xtask extract --only constants` for those"
                            .to_owned()
                    },
                    // The same shape again, and for the same reason: a table
                    // without this column says nothing is solid, and a server
                    // that printed "0 states are solid" would be reporting
                    // Minecraft rather than the operator's own file.
                    match crate::net::collide::solid_states(table) {
                        Some(solid) => format!("{solid} a player cannot walk into"),
                        None => "no full_collision column, so nothing stops a \
                                 player walking through a wall; re-run \
                                 `cargo xtask extract --only constants` for it"
                            .to_owned(),
                    },
                    // The same shape a fourth time. Without the column no
                    // block asks for a tool, which is a server where a bare
                    // hand gets cobblestone out of stone — visible in the
                    // first ten seconds of play and invisible in any log that
                    // did not say this.
                    match table.flag("requires_tool") {
                        Some(flag) => format!(
                            "{} that yield nothing to the wrong tool",
                            (0..table.len() as u32)
                                .filter(|state| table.is_set(flag, *state))
                                .count()
                        ),
                        None => "no requires_tool column, so a bare hand gets \
                                 cobblestone out of stone; re-run `cargo xtask \
                                 extract --only constants` for it"
                            .to_owned(),
                    }
                ),
            ),
            // Information and not a warning, said once at boot. A server with
            // no table lights the way Dust has always lit — and the numbers
            // beside it are `cargo xtask harness light`'s, so the sentence says
            // what it costs instead of only that it costs something.
            None => self.options.logger.info(
                "dust::data",
                format!(
                    "no {} under [data] path, so a served world's sky light treats \
                     every block but air as a wall, its sky floor sits above the \
                     grass, a placed block makes no sound, and nothing stops a \
                     player walking through a wall: the light is measured at \
                     0.6% of cells inland and 3.5% over ocean",
                    crate::registries::constants::FILE
                ),
            ),
        }
        // The second table beside the same data: which block each item places.
        // Read here rather than at the first right-click, for the reason the
        // one above is: a mistake in a file the operator wrote should stop a
        // server starting.
        let item_blocks = match data_path.as_deref() {
            None => None,
            Some(path) => crate::registries::constants::items_beside(path)
                .map_err(|e| fail(format!("{e}")))?,
        };
        match &item_blocks {
            Some(table) => self.options.logger.info(
                "dust::data",
                format!(
                    "item placements: {} item(s), {} of which place a block",
                    table.len(),
                    table.placing()
                ),
            ),
            // Information and said once, like the light table's. The
            // consequence is one sentence and it is a visible one, so it is
            // spelled out rather than left as "some feature is off".
            None => self.options.logger.info(
                "dust::data",
                format!(
                    "no {} under [data] path, so a right-click places the world's \
                     own surface block whatever the player is holding",
                    crate::registries::constants::ITEMS_FILE
                ),
            ),
        }
        let item_blocks = item_blocks.map(std::sync::Arc::new);

        // Which loot table each block draws from, which is a Java constant and
        // so comes from the oracle rather than from the data pack. Read before
        // the tables themselves, because it is what says which blocks a file
        // serves. See decision record 0027.
        let block_loot = match data_path.as_deref() {
            None => None,
            Some(path) => crate::registries::constants::blocks_beside(path)
                .map_err(|e| fail(format!("{e}")))?,
        };
        match &block_loot {
            Some(table) => self.options.logger.info(
                "dust::data",
                format!(
                    "block loot: {} block(s) over {} table(s), {} drawing from a table \
                     named after another block",
                    table.len(),
                    table.tables(),
                    table.elsewhere()
                ),
            ),
            None => self.options.logger.info(
                "dust::data",
                format!(
                    "no {} under [data] path, so a loot table is matched to the block \
                     of its own name and about sixty wall blocks drop nothing",
                    crate::registries::constants::BLOCKS_FILE
                ),
            ),
        }

        // The third thing read out of `[data] path`, and the only one that is
        // Minecraft's own file rather than a table extracted from its jar:
        // what a broken block yields. See `registries::drops`.
        let (drops, drops_report) = match data_path.as_deref() {
            None => (dust_sim::drops::Tables::default(), None),
            Some(path) => {
                let (tables, report) = crate::registries::drops::beside(path, block_loot.as_ref());
                (tables, Some(report))
            }
        };
        match &drops_report {
            Some(report) if report.files > 0 => {
                self.options.logger.info("dust::data", report.summary())
            }
            // Said once and spelled out, like the two tables above it: the
            // consequence is that mining gives nothing, which is the whole
            // survival game, and an operator should not have to guess why.
            _ => self.options.logger.info(
                "dust::data",
                "no loot tables under [data] path, so a broken block drops nothing".to_owned(),
            ),
        }
        let drops = std::sync::Arc::new(drops);

        // And the fourth: what a grid of items makes, and what a fire turns
        // one item into. Same directory, same argument, decision record 0033.
        let (recipes, cooking, cutting, smithing, recipes_report) = match data_path.as_deref() {
            None => (
                dust_sim::crafting::Recipes::default(),
                dust_sim::cooking::Cooking::new(),
                dust_sim::cutting::Cutting::new(),
                dust_sim::smithing::Smithing::new(),
                None,
            ),
            Some(path) => {
                let (recipes, cooking, cutting, smithing, report) =
                    crate::registries::recipes::beside(path);
                (recipes, cooking, cutting, smithing, Some(report))
            }
        };
        match &recipes_report {
            Some(report) if report.files > 0 => {
                self.options.logger.info("dust::data", report.summary());
                // Per fire and not just a total. Three of the four are read
                // from different files and any one of them can be empty while
                // the others are full — a smoker with no recipes and a furnace
                // with seventy is a server where half the food does not cook,
                // and one number could not say so.
                let per_fire = crate::registries::recipes::per_fire(&cooking)
                    .into_iter()
                    .map(|(fire, pairs)| format!("{} {pairs}", fire.block()))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.options
                    .logger
                    .info("dust::data", format!("cooking pairs: {per_fire}"));
            }
            _ => self.options.logger.info(
                "dust::data",
                "no recipes under [data] path, so nothing crafts".to_owned(),
            ),
        }
        let recipes = std::sync::Arc::new(recipes);
        let cooking = std::sync::Arc::new(cooking);
        let cutting = std::sync::Arc::new(cutting);
        let smithing = std::sync::Arc::new(smithing);
        let items: std::sync::Arc<crate::net::items::ItemWorld> = std::sync::Arc::default();
        let falling: std::sync::Arc<crate::net::falling::FallingWorld> = std::sync::Arc::default();

        let opacity = crate::net::world::opacity_of(palette.air, constants.as_ref());
        // Shared rather than moved: the world lights and heightmaps with this
        // table, and a session reads the sound a placed block makes out of the
        // same one. Two readers of one file, not two copies of it.
        let constants = constants.map(std::sync::Arc::new);

        // Where columns come from. An empty setting means the flat world; a
        // path means a world Minecraft wrote, with the flat one kept as the
        // fallback for columns it does not contain.
        //
        // The path is checked here rather than at the first column, because a
        // mistyped one otherwise produces a server that starts, serves flat
        // terrain, and never says why.
        // The world's own spawn point, which is not in the region files: it
        // is in `level.dat` beside them. Read here for the same reason the
        // directory is checked here — a world whose spawn this server cannot
        // read is one it would silently serve from the origin instead.
        let mut world_spawn = None;
        // The generator, if the operator's own data can build one. A world
        // file supplies its own seed; a server with no world file takes the
        // one its configuration names.
        //
        // A seed is read from `level.dat` rather than configured for a saved
        // world because the columns off the edge of a disc have to continue
        // the world that is there. Generating them from a different seed would
        // put a cliff where the file ends, which is a wrong answer that looks
        // like a right one — worse than the plain that ran on before, which at
        // least says out loud that it is not the world.
        let world_directory = std::path::PathBuf::from(&world_source);
        let seed = if world_source.is_empty() {
            Some(configured_seed)
        } else {
            crate::net::level::seed_beside(&world_directory)
        };
        let generated = match (seed, data_path.as_deref()) {
            (Some(seed), Some(path)) => crate::net::generated::beside(
                std::path::Path::new(path),
                seed,
                flat.clone(),
                opacity.clone(),
                plains,
                biomes.entries.len() as u32,
                constants.clone(),
            )
            .map_err(fail)?
            .map(|(world, report)| (world, report, seed)),
            _ => None,
        };
        match &generated {
            Some((_, report, seed)) => self
                .options
                .logger
                .info("dust::worldgen", report.summary(*seed)),
            None if data_path.is_none() => self.options.logger.info(
                "dust::worldgen",
                "no [data] path, so every column is the flat world: bedrock, three                  rows of dirt and grass at y -60"
                    .to_owned(),
            ),
            None if seed.is_none() => self.options.logger.info(
                "dust::worldgen",
                format!(
                    "no seed in the level.dat beside {world_source}, so a column that                      world does not contain is the flat world rather than the terrain                      its seed would have generated"
                ),
            ),
            None => self.options.logger.info(
                "dust::worldgen",
                format!(
                    "no {} under [data] path, so every column is the flat world;                      `cargo xtask extract --only worldgen` writes it",
                    dust_gen::biome::FILE
                ),
            ),
        }
        let source = if world_source.is_empty() {
            match generated {
                Some((world, _, _)) => crate::net::source::Source::Generated(Box::new(
                    crate::net::source::GeneratedColumns::new(world),
                )),
                None => crate::net::source::Source::Flat(Box::new(flat)),
            }
        } else {
            let directory = std::path::PathBuf::from(&world_source);
            if !crate::net::source::AnvilWorld::is_region_directory(&directory) {
                return Err(fail(format!(
                    "[server] world_source = {world_source:?} holds no .mca files; it should \
                     be a world's `region` directory"
                )));
            }
            let names = crate::net::source::RegistryNames::new().ok_or_else(|| {
                fail(
                    "the synced registries have no biome registry to resolve names against"
                        .to_owned(),
                )
            })?;
            world_spawn = crate::net::level::spawn_beside(&directory).map_err(fail)?;
            self.options.logger.info(
                "dust::server",
                match world_spawn {
                    Some(point) => format!(
                        "serving the world at {}, spawning at x {}, z {}",
                        directory.display(),
                        point.x,
                        point.z
                    ),
                    None => format!(
                        "serving the world at {}, with no level.dat beside it to give a \
                         spawn point; spawning at the origin",
                        directory.display()
                    ),
                },
            );
            crate::net::source::Source::Anvil(Box::new(match generated {
                Some((world, _, _)) => crate::net::source::AnvilWorld::generating(
                    directory,
                    names,
                    world,
                    opacity,
                    constants.clone(),
                ),
                None => crate::net::source::AnvilWorld::new(
                    directory,
                    names,
                    flat,
                    opacity,
                    constants.clone(),
                ),
            }))
        };

        // The constants go into the world as well as into the session: the
        // session needs the sound a block makes, and the world needs which of
        // a block's faces are full, which is what says whether a fence beside
        // it grows an arm.
        let world = std::sync::Arc::new(
            crate::net::edits::EditedWorld::new(source).with_constants(constants.clone()),
        );

        // What players changed last time, and where they were standing. A
        // world that has never been played has no file and that is not an
        // error; a file this build cannot read *is* one, because starting with
        // an empty world beside a save that exists would quietly discard it on
        // the next write.
        let world_dir = self.options.world_dir.clone();
        let positions: crate::net::save::SharedPositions = std::sync::Arc::default();
        let inventories: crate::net::save::SharedInventories = std::sync::Arc::default();
        let furnaces = std::sync::Arc::new(crate::net::furnaces::Furnaces::new());
        // The region directory, when there is one. `level.dat` is beside it,
        // and it is both where an imported world's clock is read from and
        // where this server's clock is written back.
        let region_dir = (!world_source.is_empty()).then(|| world_directory.clone());
        // Where the sun is, before the save is read. Three sources in
        // decreasing authority, and each one is a different world:
        //
        //  1. Dust's own save file — the record this server wrote last, and
        //     the only one guaranteed to exist for every world it serves.
        //  2. `level.dat` — a world imported from vanilla, or one whose Dust
        //     save has been deleted. It is what makes a world dropped into
        //     `world_source` open at the hour its owner left it.
        //  3. Dawn on day zero, which is where Minecraft starts a new world.
        //
        // The order matters in exactly one case and it is worth stating: if
        // the `level.dat` write at shutdown failed — a read-only directory, a
        // file open in an editor — then it holds an older time than the save
        // does, and a server that preferred it would walk the world backwards
        // a little on every restart.
        let mut clock_source = "a fresh world";
        let mut restored_time = None;
        match crate::net::save::load(&world_dir) {
            Ok(Some(saved)) => {
                let (blocks, unknown) = crate::net::save::resolve(&saved.blocks);
                let applied = world.restore(blocks);
                *positions.lock().expect("not poisoned") = crate::net::save::positions(&saved);
                let (carried, unknown_items, dropped_components) =
                    crate::net::save::inventories(&saved);
                let stacks: usize = carried
                    .values()
                    .map(crate::net::save::stacks_of)
                    .map(|s| s.len())
                    .sum();
                let carrying = carried.len();
                *inventories.lock().expect("not poisoned") = carried;
                self.options.logger.info(
                    "dust::server",
                    format!(
                        "restored {applied} block change(s), {} player position(s) and \
                         {stacks} stack(s) across {carrying} inventory/ies",
                        saved.players.len()
                    ),
                );
                let (saved_furnaces, unknown_fires, furnace_components) =
                    crate::net::save::furnaces(&saved);
                let restored_furnaces =
                    furnaces.restore(saved_furnaces, Some(&cooking), item_blocks.as_deref());
                if restored_furnaces > 0 {
                    self.options.logger.info(
                        "dust::server",
                        format!(
                            "restored {restored_furnaces} furnace(s), {} of them alight",
                            furnaces.active()
                        ),
                    );
                }
                if !unknown_fires.is_empty() {
                    self.options.logger.warn(
                        "dust::server",
                        format!(
                            "the save names {} furnace(s) whose block or items this build has \
                             no entry for, and they were dropped: {}",
                            unknown_fires.len(),
                            unknown_fires.join(", ")
                        ),
                    );
                }
                let dropped_components = dropped_components + furnace_components;
                if dropped_components > 0 {
                    // Named rather than swallowed: the stacks came back and
                    // their names, enchantments and contents did not, and a
                    // player is about to find that out by looking.
                    self.options.logger.warn(
                        "dust::server",
                        format!(
                            "{dropped_components} stack(s) came back without their components: \
                             the save wrote them for {}, this build reads {}",
                            saved.components.as_deref().unwrap_or("no version"),
                            dust_registry::generated::registries::DATA_VERSION
                        ),
                    );
                }
                if !unknown_items.is_empty() {
                    // Named for the same reason a block is: an operator who
                    // changed Minecraft version needs to know which item their
                    // players just lost.
                    self.options.logger.warn(
                        "dust::server",
                        format!(
                            "the save names {} item(s) this build has no entry for, and the \
                             stacks holding them were dropped: {}",
                            unknown_items.len(),
                            unknown_items.join(", ")
                        ),
                    );
                }
                if !unknown.is_empty() {
                    // Named, not counted. An operator who renamed a block or
                    // changed Minecraft version needs to know *which* block
                    // stopped existing, and a number tells them only that
                    // something did.
                    self.options.logger.warn(
                        "dust::server",
                        format!(
                            "the save names {} block(s) this build has no entry for, and they \
                             were dropped: {}",
                            unknown.len(),
                            unknown.join(", ")
                        ),
                    );
                }
                if let Some(time) = saved.time {
                    restored_time = Some((time.game_time, time.day_time));
                    clock_source = "this server's own save";
                }
            }
            Ok(None) => {}
            Err(e) => return Err(fail(format!("{e}"))),
        }
        if restored_time.is_none() {
            if let Some(time) = region_dir
                .as_deref()
                .and_then(crate::net::level::time_beside)
            {
                restored_time = Some((time.game_time, time.day_time));
                clock_source = "the world's own level.dat";
            }
        }
        let clock = std::sync::Arc::new(match restored_time {
            Some((game_time, day_time)) => {
                crate::net::daylight::WorldClock::new(game_time, day_time, daylight_cycle)
            }
            None => crate::net::daylight::WorldClock::fresh(daylight_cycle),
        });
        self.options.logger.info(
            "dust::server",
            format!(
                "the world is on day {} at tick {} of it, from {clock_source}; the daylight \
                 cycle is {}",
                clock.day(),
                clock.time_of_day(),
                if daylight_cycle { "running" } else { "stopped" }
            ),
        );

        // Built once at boot because it cannot change while the server runs,
        // and because every joining player is sent the same bytes. A graph
        // this server cannot build is a server that would tab-complete
        // nothing, which is a boot failure rather than a surprise per join.
        let commands = std::sync::Arc::new(
            crate::net::commands::declaration()
                .map_err(|e| fail(format!("the command graph could not be built: {e}")))?,
        );

        // Minecraft's own registry contents, if the operator pointed at a
        // copy. Loaded before the listener binds rather than on first use: a
        // data directory with a mistake in it should stop a server starting,
        // not surprise the first client that needs it half an hour later.
        let registry_contents = match data_path.as_deref() {
            None => crate::registries::Loaded::default(),
            Some(path) => match crate::registries::load(path) {
                Ok(loaded) => {
                    for (registry, count) in loaded.summary() {
                        self.options.logger.info(
                            "dust::data",
                            format!("{registry}: {count} entries from {path}"),
                        );
                    }
                    loaded
                }
                Err(e) => return Err(fail(format!("[data] path = {path:?}: {e}"))),
            },
        };
        if registry_contents.is_empty() {
            // Said once at boot rather than once per refused client, and said
            // as information rather than a warning: a server nobody points a
            // bot at is not misconfigured.
            self.options.logger.info(
                "dust::data",
                "no [data] path is set, so clients that acknowledge no data packs                  (most bots and proxies) cannot be served"
                    .to_owned(),
            );
        }

        // Shared between the accept loop and every session on it: the accept
        // loop counts connections, and the sessions count the players inside
        // them, because only a session knows when somebody has actually
        // arrived.
        let counters = std::sync::Arc::new(crate::net::Counters::default());

        // Everybody connected, shared with the console so `list` and `say`
        // reach the same players the sessions do.
        let roster: std::sync::Arc<crate::net::players::Roster> = std::sync::Arc::default();

        let listener = crate::net::Listener::bind(addr).map_err(|e| fail(e.to_string()))?;
        let bound = listener.addr();

        let ctx = std::sync::Arc::new(crate::net::SessionContext {
            version,
            status: crate::net::StatusPolicy::new(version, motd, max_players, favicon),
            conn: dust_net::io::ConnConfig::default(),
            auth: authority,
            world: std::sync::Arc::clone(&world),
            world_spawn,
            view_distance,
            reach,
            speed,
            collision,
            overworld_dimension_type: overworld,
            blocks: crate::net::PlaceableBlocks {
                air: palette.air,
                placeable: palette.grass,
            },
            logger: self.options.logger.clone(),
            positions: std::sync::Arc::clone(&positions),
            inventories: std::sync::Arc::clone(&inventories),
            roster: std::sync::Arc::clone(&roster),
            player_entity_type: crate::net::play::player_entity_type().ok_or_else(|| {
                fail("the generated entity table has no minecraft:player".to_owned())
            })?,
            counters: std::sync::Arc::clone(&counters),
            constants: constants.clone(),
            // Resolved once here rather than per break. `None` when the table
            // predates the column, which reads as "no block wants a tool" —
            // the generous direction, and the server the operator had.
            requires_tool: constants
                .as_deref()
                .and_then(|table| table.flag("requires_tool")),
            game_mode: config.server.game_mode,
            item_blocks,
            registry_contents,
            items: std::sync::Arc::clone(&items),
            drops: std::sync::Arc::clone(&drops),
            recipes: std::sync::Arc::clone(&recipes),
            cooking: std::sync::Arc::clone(&cooking),
            declared_recipes: crate::registries::recipes::declaration(&cutting, &smithing, version),
            cutting: std::sync::Arc::clone(&cutting),
            smithing: std::sync::Arc::clone(&smithing),
            furnaces: std::sync::Arc::clone(&furnaces),
            clock: std::sync::Arc::clone(&clock),
            commands: std::sync::Arc::clone(&commands),
            item_entity_type: crate::net::play::item_entity_type().ok_or_else(|| {
                fail("the generated entity table has no minecraft:item".to_owned())
            })?,
            falling_entity_type: crate::net::play::falling_entity_type().ok_or_else(|| {
                fail("the generated entity table has no minecraft:falling_block".to_owned())
            })?,
            falling: std::sync::Arc::clone(&falling),
        });

        // Built here because it needs the world and the roster, both of which
        // are phase 3's; inserted into the tick loop by phase 4.
        self.furnace_ticker = Some(Box::new(crate::net::furnaces::FurnaceTicker::new(
            std::sync::Arc::clone(&furnaces),
            std::sync::Arc::clone(&world),
            Some(std::sync::Arc::clone(&cooking)),
            ctx.item_blocks.clone(),
        )));
        // One addition per tick, and it is a participant of its own so that the
        // clock cannot stop because something else in the tick did. See
        // `net::daylight` for what it costs, which is a nanosecond.
        self.daylight_ticker = Some(Box::new(crate::net::daylight::DaylightTicker::new(
            std::sync::Arc::clone(&clock),
        )));
        self.item_ticker = Some(Box::new(crate::net::items::ItemTicker::new(
            std::sync::Arc::clone(&items),
            std::sync::Arc::clone(&world),
            std::sync::Arc::clone(&roster),
            constants.clone(),
        )));
        // The world reacting to itself, built here for the same reason: it
        // needs the world, the roster, the loot tables and both entity worlds,
        // and every one of those is phase 3's.
        self.world_ticker = Some(Box::new(crate::net::updates::WorldTicker::new(
            std::sync::Arc::clone(&world),
            std::sync::Arc::clone(&items),
            std::sync::Arc::clone(&falling),
            std::sync::Arc::clone(&roster),
            std::sync::Arc::clone(&drops),
            constants,
            palette.air,
        )));

        let handle = listener
            .serve(ctx, counters, self.options.logger.clone())
            .map_err(|e| fail(format!("could not start serving: {e}")))?;

        transcript.push(TranscriptEntry {
            phase: Phase::NetworkBind,
            direction: Direction::Start,
            detail: format!(
                "listening on {bound} for protocol {} ({}), {} mode",
                version.number(),
                version.name(),
                if online_mode { "online" } else { "offline" }
            ),
        });
        // The operator's line in, started once there is something to talk to.
        // It reads on its own thread because a blocking read on a terminal
        // cannot be polled, and it is detached because a shutdown must not
        // wait for somebody to press return.
        crate::console::spawn(self.options.logger.clone(), {
            let console = crate::console::Console {
                stop: self.stop_handle.clone(),
                roster: std::sync::Arc::clone(&roster),
                logger: self.options.logger.clone(),
            };
            move |command| console.run(command)
        });

        // Published only after the handle exists, so an observer that sees an
        // address knows there is something accepting on it.
        let _ = self.shared.bound.set(bound);
        self.listener = Some(handle);
        self.saveable = Some(Saveable {
            world: std::sync::Arc::clone(&world),
            positions: std::sync::Arc::clone(&positions),
            inventories: std::sync::Arc::clone(&inventories),
            furnaces: std::sync::Arc::clone(&furnaces),
            clock: std::sync::Arc::clone(&clock),
            world_dir,
            region_dir,
        });
        Ok(())
    }

    /// The hot loop. Checks stop **between** passes; a batch in flight always
    /// finishes whole.
    ///
    /// `catchup_cap` is the per-burst repayment allowance, from
    /// `[server].max_catchup_ticks`; it is an argument rather than a field so
    /// the loop cannot run without the configuration phase having supplied it.
    fn run_tick_loop(&self, participants: &mut ParticipantSet, catchup_cap: u32) -> EngineSummary {
        let parker = (self.options.loop_parker)(
            Arc::clone(&self.shared.stop),
            Arc::clone(&self.options.clock),
        );
        let mut engine =
            TickEngine::new(Arc::clone(&self.options.clock)).with_catchup_cap(catchup_cap);
        while !self.shared.stop.is_stopped() {
            engine.advance(participants, &self.options.logger);
            self.shared
                .ticks_observed
                .store(engine.ticks_run(), Ordering::SeqCst);
            if self.shared.stop.is_stopped() {
                break;
            }
            if let Some(deadline) = engine.next_deadline() {
                parker.park_until(deadline);
            }
        }
        EngineSummary {
            ticks_run: engine.ticks_run(),
            surrendered_batches: engine.surrendered_batches(),
            overall_timing: engine.overall_timing(),
            participant_timing: engine
                .accounted_participants()
                .into_iter()
                .filter_map(|name| engine.participant_timing(&name).map(|stats| (name, stats)))
                .collect(),
        }
    }

    // ---- teardown --------------------------------------------------------

    /// Push stop entries for everything in `completed`, newest first.
    ///
    /// This is the symmetric-shutdown guarantee in six lines, which is why it
    /// takes the completed list rather than re-deriving it: one list, one
    /// order, no archaeology.
    fn teardown(
        &self,
        completed: Vec<Phase>,
        transcript: &mut Vec<TranscriptEntry>,
        ticks_run: u64,
        listener: &mut Option<crate::net::ListenerHandle>,
        saveable: &mut Option<Saveable>,
    ) {
        for phase in completed.into_iter().rev() {
            // The listener is released *at the position the transcript claims
            // it is*, rather than wherever the handle happens to go out of
            // scope. Symmetry that is only true of the log is not symmetry.
            let detail = if phase == Phase::NetworkBind {
                match listener.take() {
                    Some(handle) => {
                        let addr = handle.addr();
                        let stats = handle.stats();
                        handle.shutdown();
                        format!(
                            "released {addr} after {} connection(s): {} ping(s), \
                             {} login(s), {} login(s) refused, {} failed, \
                             {} still online",
                            stats.accepted,
                            stats.status_served,
                            stats.logins,
                            stats.logins_failed,
                            stats.failed,
                            stats.online
                        )
                    }
                    // The bind failed, so the phase never completed and this
                    // arm is unreachable from `run`. Written honestly rather
                    // than as an unwrap, because a future caller could unwind a
                    // list this function did not build.
                    None => "nothing was listening".to_owned(),
                }
            } else if phase == Phase::WorldDirs {
                // The world is written *here*, after the listener is released
                // and so after every session has stopped changing it. Writing
                // it while connections were still live would save a world that
                // was still moving, and the last edit would be the one lost.
                self.save_world(saveable.take())
            } else {
                phase.teardown_detail(ticks_run)
            };
            transcript.push(TranscriptEntry {
                phase,
                direction: Direction::Stop,
                detail,
            });
        }
    }

    /// Write the world down, and say what happened either way.
    ///
    /// A failure is reported into the transcript rather than returned, because
    /// this runs during teardown and there is nothing left to abort. It is
    /// still loud: an operator whose disk filled needs to know the world did
    /// not survive, and a silent failure here is the one that is discovered by
    /// the blocks being gone.
    fn save_world(&self, saveable: Option<Saveable>) -> String {
        let Some(saveable) = saveable else {
            // The bind failed, so the phase never completed and nothing was
            // ever built to save.
            return "directories left on disk".to_owned();
        };

        let blocks: Vec<crate::net::save::SavedBlock> = saveable
            .world
            .snapshot()
            .into_iter()
            .filter_map(|(position, state)| {
                crate::net::save::name_of(state).map(|block| crate::net::save::SavedBlock {
                    x: position.x,
                    y: position.y,
                    z: position.z,
                    block: block.to_owned(),
                })
            })
            .collect();
        let players: Vec<crate::net::save::SavedPlayer> = {
            let held = saveable.positions.lock().expect("not poisoned");
            let carried = saveable.inventories.lock().expect("not poisoned");
            let mut players: Vec<_> = held
                .iter()
                .map(|(id, (x, y, z))| crate::net::save::SavedPlayer {
                    id: crate::net::save::hyphenated(id),
                    x: *x,
                    y: *y,
                    z: *z,
                    inventory: carried
                        .get(id)
                        .map(crate::net::save::stacks_of)
                        .unwrap_or_default(),
                    selected: carried.get(id).map_or(0, |c| c.selected),
                    experience: carried.get(id).map_or(0, |c| c.experience),
                })
                .collect();
            // Ordered, so two saves of one world are the same file.
            players.sort_by(|a, b| a.id.cmp(&b.id));
            players
        };

        // Snapshotted here rather than streamed, and it is safe to: this runs
        // in the `WorldDirs` teardown arm, which is after the listener is
        // released and after the tick loop has stopped, so nothing can be
        // mutating a furnace while it is written.
        let furnaces: Vec<crate::net::save::SavedFurnace> = saveable
            .furnaces
            .snapshot()
            .into_iter()
            .map(|(at, furnace)| crate::net::save::saved_furnace(at, &furnace))
            .collect();

        let stacks: usize = players.iter().map(|p| p.inventory.len()).sum();
        let burning = furnaces.iter().filter(|f| f.lit > 0).count();
        // Read once, here, and written to both places from the one reading, so
        // that the save file and `level.dat` cannot disagree by a tick.
        let time = crate::net::save::SavedTime {
            game_time: saveable.clock.game_time(),
            day_time: saveable.clock.day_time(),
        };
        let counts = format!(
            "{} block change(s), {} player position(s), {stacks} carried stack(s), \
             {} furnace(s), {burning} of them alight, and the clock on day {} at tick {}",
            blocks.len(),
            players.len(),
            furnaces.len(),
            time.day_time / crate::net::daylight::DAY_TICKS,
            time.day_time % crate::net::daylight::DAY_TICKS
        );
        // Into the world's own file as well, when there is one. Best effort by
        // design: the clock is already in the save above, so a `level.dat`
        // that could not be rewritten costs an operator the time of day *in
        // vanilla* and costs this server nothing. Failing a shutdown over it
        // would be the wrong trade — it would leave the block edits unwritten
        // too.
        if let Some(region_dir) = saveable.region_dir.as_deref() {
            match crate::net::level::store_time_beside(
                region_dir,
                crate::net::level::WorldTime {
                    game_time: time.game_time,
                    day_time: time.day_time,
                },
            ) {
                Ok(true) | Ok(false) => {}
                Err(e) => self.options.logger.warn(
                    "dust::server",
                    format!(
                        "the world's own level.dat could not be given the time back ({e}); \
                         this server will still restore it from its own save, but opening \
                         the world in Minecraft will show the time it was imported at"
                    ),
                ),
            }
        }
        let save = crate::net::save::Save {
            version: crate::net::save::SAVE_VERSION,
            blocks,
            components: crate::net::save::components_version(&players, &furnaces)
                .map(std::borrow::ToOwned::to_owned),
            players,
            furnaces,
            time: Some(time),
        };
        match crate::net::save::store(&saveable.world_dir, &save) {
            Ok(()) => format!("saved {counts}"),
            Err(e) => {
                self.options
                    .logger
                    .error("dust::server", format!("the world could not be saved: {e}"));
                format!("FAILED to save {counts}: {e}")
            }
        }
    }

    /// Graceful abort: stop arrived during startup. Everything completed is
    /// torn down in reverse and the report says so.
    fn abort_startup(
        mut self,
        completed: Vec<Phase>,
        mut transcript: Vec<TranscriptEntry>,
        started_at: u64,
    ) -> ShutdownReport {
        // No helper threads exist on this path: the watchdog spawns only
        // once the tick loop begins.
        let mut listener = self.listener.take();
        let mut saveable = self.saveable.take();
        self.teardown(completed, &mut transcript, 0, &mut listener, &mut saveable);
        ShutdownReport {
            interrupted: true,
            ticks_run: 0,
            uptime_ns: self.options.clock.now_ns().saturating_sub(started_at),
            surrendered_batches: 0,
            overall_timing: TimingStats::default(),
            participant_timing: BTreeMap::new(),
            participants: Vec::new(),
            transcript,
            watchdog_fired: false,
            shutdown_grace_ns: None,
            threads_joined: 0,
            thread_panics: Vec::new(),
        }
    }
}

/// Resolve a `host:port` bind string to the address the real listener will
/// take.
///
/// IP literals take the fast path; hostnames go through the platform
/// resolver. An empty resolution is an error, not a shrug: a bind that
/// resolves to nothing must stop the boot now rather than at first listen.
pub fn resolve_bind(bind: &str) -> Result<SocketAddr, String> {
    if let Ok(addr) = bind.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let (host, port) = bind
        .rsplit_once(':')
        .ok_or_else(|| format!("expected host:port, got {bind:?} with no port"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("port {port:?} is not a u16"))?;
    (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve host {host:?}: {e}"))?
        .next()
        .ok_or_else(|| format!("host {host:?} resolved to no addresses"))
}

/// Summary of the finished loop, captured exactly where the numbers stop
/// being live.
struct EngineSummary {
    ticks_run: u64,
    surrendered_batches: u64,
    overall_timing: TimingStats,
    participant_timing: BTreeMap<String, TimingStats>,
}

/// What a completed run hands back.
///
/// Everything an integration test — or an operator's post-mortem — wants:
/// what ran, in what order, how many ticks, what the timing looked like,
/// whether the watchdog had to intervene and whether any thread died.
#[derive(Debug)]
pub struct ShutdownReport {
    /// Stop arrived during startup phases, before any tick ran.
    pub interrupted: bool,
    pub ticks_run: u64,
    pub uptime_ns: u64,
    /// Bursts that hit the catch-up cap and resynchronised.
    pub surrendered_batches: u64,
    pub overall_timing: TimingStats,
    pub participant_timing: BTreeMap<String, TimingStats>,
    /// Participant names in execution order.
    pub participants: Vec<String>,
    pub transcript: Vec<TranscriptEntry>,
    pub watchdog_fired: bool,
    /// The grace period the watchdog ran with, in nanoseconds — `None` when
    /// the run went without one. Recorded because "the timeout came from the
    /// file" is a claim a report can carry and a reader can check.
    pub shutdown_grace_ns: Option<u64>,
    pub threads_joined: usize,
    pub thread_panics: Vec<String>,
}

impl ShutdownReport {
    /// Whether the run ended the way a clean run should: uninterrupted, all
    /// threads retired, nobody panicking.
    pub fn is_clean(&self) -> bool {
        !self.interrupted && self.thread_panics.is_empty()
    }

    /// Transcript lines as `(phase, direction)` pairs, ready to compare.
    pub fn transcript_pairs(&self) -> Vec<(Phase, Direction)> {
        self.transcript
            .iter()
            .map(|e| (e.phase, e.direction))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TICK_NS;
    use crate::stop::StepParker;
    use std::io::Write;
    use std::sync::Mutex;

    /// A world directory of this run's own; see the note where it is used.
    fn test_world_dir() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "dust-server-unit-world-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst),
        ))
    }

    /// A unique temp file per call: tests run in parallel in one process, and
    /// two of them sharing a config path would share a fate.
    fn write_config(text: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "dust-server-test-{}-{}.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::write(&path, text).expect("write the temp config");
        path
    }

    /// Bytes collected from the logger, shareable with the test thread.
    #[derive(Clone, Default)]
    struct SinkBytes(Arc<Mutex<Vec<u8>>>);

    impl Write for SinkBytes {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A parker factory whose parks advance virtual time by `step_ns`, so a
    /// full lifecycle costs no real time.
    fn stepping(clock: Arc<ManualClock>, step_ns: u64) -> ParkerFactory {
        Arc::new(move |_state, _clock| {
            Box::new(StepParker::new(clock.clone(), step_ns)) as Box<dyn Parker>
        })
    }

    /// A server wired for virtual time around one config file, plus the
    /// handles a test needs to watch and stop it.
    struct Run {
        metrics: LiveMetrics,
        stop: StopHandle,
        sink: Arc<Mutex<Vec<u8>>>,
    }

    /// A config for a test: a listener that cannot collide, and no Mojang.
    ///
    /// Every boot now takes a real port, and two things follow that the tests
    /// have to say out loud. The bind is **loopback**, so a unit test never
    /// opens a port to the network; and it is **port 0**, so the operating
    /// system picks a free one and two tests running in parallel — which is
    /// the default — cannot fight over it. Without this, the suite would pass
    /// alone and fail together, which is the shape of flakiness that costs the
    /// most to diagnose.
    ///
    /// `online_mode` is set for a reason of the same shape and a bigger bill.
    /// It defaults to `true`, and [`Server::start_network`] answers that by
    /// loading the system root certificates and generating an RSA key pair
    /// *before* it binds — by a prime search its own comment calls "slow and
    /// unbounded". Every test here that did not say otherwise was paying
    /// seconds for a key pair it never used and taking on a dependency on the
    /// host's certificate store to get it. Both arrived: these boots ran four
    /// to five seconds each, four of them overran the thirty-second wait on a
    /// busy machine and reported "stuck at 0" with an empty log, and on a Mac
    /// whose keychain refused a trust-settings read they failed outright with
    /// a certificate error two phases away from anything under test.
    ///
    /// A config text that names either setting itself is left alone: those
    /// tests are about that setting.
    fn with_test_defaults(config_text: &str) -> String {
        let mut settings = String::new();
        if !config_text.contains("bind") {
            settings.push_str("bind = \"127.0.0.1:0\"\n");
        }
        if !config_text.contains("online_mode") {
            settings.push_str("online_mode = false\n");
        }
        if settings.is_empty() {
            return config_text.to_owned();
        }
        if let Some(rest) = config_text.strip_prefix("[server]\n") {
            format!("[server]\n{settings}{rest}")
        } else {
            format!("[server]\n{settings}{config_text}")
        }
    }

    /// Build (but do not start) a virtual-time run of `dust server`.
    fn boot(config_text: &str, configure: impl FnOnce(&mut ServerOptions)) -> (Run, Server) {
        let clock = Arc::new(ManualClock::new());
        let sink = Arc::new(Mutex::new(Vec::<u8>::new()));
        let logger = Logger::new(
            Arc::new(Mutex::new(SinkBytes(Arc::clone(&sink)))),
            Level::Info,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );
        let config_path = write_config(&with_test_defaults(config_text));
        let mut options = ServerOptions {
            config_path: config_path.clone(),
            // A directory of this run's own. The default is `world`, which is
            // *relative*, so every test that took it shared one directory
            // under the crate root — and they run in parallel. Two servers
            // saving into and removing the same one produce "the world could
            // not be saved: No such file or directory", which is a shutdown
            // failing for a reason unrelated to what the test was checking.
            world_dir: test_world_dir(),
            clock: Arc::clone(&clock) as Arc<dyn Clock>,
            // The tick loop parks by advancing virtual time; the watchdog
            // deliberately keeps the default condvar park. A stepper would
            // let the watcher manufacture its own grace expiry faster than
            // the other threads could report completion — the watchdog may
            // observe time, never fabricate it.
            loop_parker: stepping(Arc::clone(&clock), TICK_NS),
            logger,
            ..ServerOptions::default()
        };
        configure(&mut options);
        let server = Server::new(options);
        let run = Run {
            metrics: server.metrics(),
            stop: server.stop_handle(),
            sink,
        };
        (run, server)
    }

    /// Wait for the loop to run `minimum` ticks, or give up after a deadline.
    ///
    /// **In seconds and not in iterations**, for the same reason its sibling in
    /// `tests/lifecycle.rs` is: this spun ten million times on `yield_now`,
    /// which measures patience in yields — and a yield costs almost nothing on
    /// an idle machine and a scheduling quantum on a busy one. A wait that is
    /// shortest when nothing else is running is a wait that fails on exactly
    /// the machine where nothing else is wrong.
    ///
    /// Thirty seconds is patience with the OS scheduler and not tolerance for
    /// a slow loop: the loop under test parks on a virtual clock and does no
    /// real work, so a healthy one is done in milliseconds. Sleeping rather
    /// than spinning, because this thread has nothing to do until the loop has
    /// run and a spin denies it the core it is waiting on.
    fn wait_until_ticks(run: &Run, minimum: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if run.metrics.ticks_observed() >= minimum {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // **The log, printed rather than referred to.** A boot that failed
        // leaves the tick count at zero and says why in the log this test is
        // already capturing — and a panic that told somebody to go and read it
        // cost three separate diagnoses on the day it was written. It is four
        // lines to print and it turns "stuck at 0" into a cause.
        let log = run
            .sink
            .lock()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_else(|_| "<the log sink was poisoned>".to_owned());
        panic!(
            "the server never reached {minimum} tick(s) in 30s; stuck at {}. \
             A boot that failed leaves this at zero, and its log is:\n{}",
            run.metrics.ticks_observed(),
            if log.trim().is_empty() {
                "<nothing was logged, so the boot did not get as far as its \
                 first line>"
                    .to_owned()
            } else {
                log
            }
        );
    }

    #[test]
    fn an_unresolvable_bind_unwinds_the_completed_phases_in_reverse() {
        let (_run, server) = boot("[server]\nbind = \"no port here\"\n", |_| {});
        let err = server.run().expect_err("a bad bind stops the boot");
        assert!(matches!(err, ServerError::NetworkBind { .. }), "{err}");
        assert!(err.to_string().contains("no port here"), "{err}");
        // Config and world started and were both released; network never
        // started, so it never appears — not even as a failure line.
        assert_eq!(
            err.transcript()
                .iter()
                .map(|e| (e.phase, e.direction))
                .collect::<Vec<_>>(),
            vec![
                (Phase::ConfigLoad, Direction::Start),
                (Phase::WorldDirs, Direction::Start),
                (Phase::WorldDirs, Direction::Stop),
                (Phase::ConfigLoad, Direction::Stop),
            ]
        );
    }

    #[test]
    fn an_invalid_config_stops_the_boot_before_anything_started() {
        let (_run, server) = boot("[server]\nmotdd = \"typo\"\n", |_| {});
        let err = server.run().expect_err("an unknown key is refused");
        assert!(matches!(err, ServerError::Config(_)), "{err}");
        assert!(err.transcript().is_empty(), "{:?}", err.transcript());
    }

    #[test]
    fn the_watchdog_thread_is_spawned_once_and_joined_once() {
        // This is the lifecycle-level half of the no-leaked-threads
        // guarantee; the tracker's own books are checked in `stop`'s tests.
        // What the run adds: the one thread spawned during phase 4 retires
        // through the same books instead of being detached, and a graceful
        // shutdown always beats it to completion.
        let (run, server) = boot("[server]\nshutdown_timeout_secs = 600\n", |options| {
            options.watchdog =
                WatchdogSetting::Custom(WatchdogPolicy::custom(600_000_000_000, |_| {}));
        });
        let worker = std::thread::spawn(move || server.run());
        wait_until_ticks(&run, 2);
        assert!(run.stop.request_stop());
        let report = worker.join().expect("run finishes").expect("clean");
        assert_eq!(report.thread_panics, Vec::<String>::new());
        assert_eq!(report.threads_joined, 1, "the watchdog thread retires");
        assert!(!report.watchdog_fired);
    }

    #[test]
    fn the_configured_shutdown_timeout_reaches_the_watchdog() {
        let (run, server) = boot("[server]\nshutdown_timeout_secs = 600\n", |_| {});
        let worker = std::thread::spawn(move || server.run());
        wait_until_ticks(&run, 3);
        assert!(run.stop.request_stop());
        let report = worker.join().expect("run finishes").expect("clean");
        assert_eq!(report.shutdown_grace_ns, Some(600_000_000_000));
        assert!(!report.watchdog_fired, "graceful shutdown wins the race");
        assert_eq!(report.thread_panics, Vec::<String>::new());
    }

    #[test]
    fn a_stop_before_boot_aborts_with_a_symmetric_transcript_and_no_ticks() {
        let (run, server) = boot("", |_| {});
        run.stop.request_stop();
        let report = server.run().expect("an early stop is not an error");
        assert!(report.interrupted);
        assert_eq!(report.ticks_run, 0);
        assert_eq!(
            report.transcript_pairs(),
            vec![
                (Phase::ConfigLoad, Direction::Start),
                (Phase::ConfigLoad, Direction::Stop),
            ]
        );
    }

    #[test]
    fn the_configured_log_level_silences_the_heartbeat() {
        // At warn, the info-level heartbeat never reaches the sink...
        let (run, server) = boot("[server]\nlog_level = \"warn\"\n", |_| {});
        let worker = std::thread::spawn(move || server.run());
        wait_until_ticks(&run, 2);
        run.stop.request_stop();
        let report = worker.join().expect("finishes").expect("clean");
        assert!(report.is_clean());
        let text = String::from_utf8(run.sink.lock().unwrap().clone()).expect("utf8");
        assert!(!text.contains("heartbeat"), "{text}");

        // ...and at the default it does, proving silence was the setting and
        // not a broken sink.
        let (run, server) = boot("", |_| {});
        let worker = std::thread::spawn(move || server.run());
        wait_until_ticks(&run, 2);
        run.stop.request_stop();
        worker.join().expect("finishes").expect("clean");
        let text = String::from_utf8(run.sink.lock().unwrap().clone()).expect("utf8");
        assert!(text.contains("heartbeat"), "{text}");
    }

    #[test]
    fn jvm_disabled_keeps_its_placeholder_out_of_the_participant_list() {
        let (run, server) = boot("[jvm]\nenabled = false\n", |_| {});
        let worker = std::thread::spawn(move || server.run());
        wait_until_ticks(&run, 1);
        run.stop.request_stop();
        let report = worker.join().expect("finishes").expect("clean");
        assert!(
            !report.participants.contains(&"jvm-placeholder".to_owned()),
            "{:?}",
            report.participants
        );
        assert!(report.participants.contains(&"status-probe".to_owned()));
    }

    #[test]
    fn an_environment_override_reaches_the_runtime_knobs() {
        // The env layer sits between file and types; these are the settings
        // this crate consumes, arriving exactly as a container would set
        // them. Pure function, no process environment touched.
        let config = DustConfig::from_toml_and_env(
            "",
            "test",
            [
                ("DUST__SERVER__MAX_CATCHUP_TICKS".to_owned(), "7".to_owned()),
                (
                    "DUST__SERVER__SHUTDOWN_TIMEOUT_SECS".to_owned(),
                    "9".to_owned(),
                ),
                ("DUST__SERVER__LOG_LEVEL".to_owned(), "debug".to_owned()),
            ],
        )
        .expect("valid");
        let runtime = RuntimeSettings::from_config(&config);
        assert_eq!(runtime.catchup_cap, 7);
        assert_eq!(runtime.shutdown_grace_ns, 9_000_000_000);
        assert_eq!(log_level_of(config.server.log_level), Level::Debug);
    }
}
