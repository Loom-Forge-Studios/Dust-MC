//! End-to-end lifecycle proofs, driven entirely on virtual time.
//!
//! These tests sit outside the crate on purpose: they see exactly what a
//! Phase 3 integrator will see — the public `dust_server` surface, nothing
//! else. A full boot, ten ticks, a simulated ctrl-C and a symmetric shutdown
//! cost microseconds here; the same scenario against a real clock would need
//! half a second of sleeping to prove less.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dust_config::DustConfig;
use dust_server::clock::{Clock, ManualClock};
use dust_server::engine::TICK_NS;
use dust_server::participant::{TickContext, TickParticipant};
use dust_server::server::{
    Direction, LiveMetrics, ParkerFactory, Phase, Server, ServerOptions, ShutdownReport,
    WatchdogSetting,
};
use dust_server::stop::{Parker, StepParker, StopHandle};

/// A world directory of this test's own.
///
/// `ServerOptions::default()` names `world`, which is *relative* — so every
/// test in this file that took the default shared one directory under the
/// crate root, and they run in parallel. Two servers saving into and removing
/// the same directory produce "the world could not be saved: No such file or
/// directory", which is a boot or a shutdown failing for a reason that has
/// nothing to do with what the test was checking.
///
/// Same shape as the bind below, and the same answer: give each run its own,
/// from one place nobody has to remember.
fn test_world_dir() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "dust-server-lifecycle-world-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
    ))
}

/// A unique temp file per call, always with the test defaults applied.
///
/// The bind is applied *here* rather than left to each caller, because one
/// caller forgot. `[server].bind` defaults to `0.0.0.0:25565`, so a test that
/// omitted it took the well-known Minecraft port — and on a machine already
/// running something there (another server, a container) the boot failed and
/// the test reported "never reached 12 ticks", which describes the symptom and
/// points nowhere. A rule every caller has to remember is a rule one of them
/// will not.
fn write_config(text: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "dust-server-lifecycle-{}-{}.toml",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
    ));
    std::fs::write(&path, with_test_defaults(text)).expect("write the temp config");
    path
}

/// The parker factory for the tick loop: every park advances virtual time by
/// `step_ns`, which turns "let the server run" into arithmetic.
fn stepping(clock: Arc<ManualClock>, step_ns: u64) -> ParkerFactory {
    Arc::new(move |_state, _clock| {
        Box::new(StepParker::new(clock.clone(), step_ns)) as Box<dyn Parker>
    })
}

/// A participant that records every tick index it is handed.
struct Counter {
    log: Arc<Mutex<Vec<u64>>>,
}

impl TickParticipant for Counter {
    fn name(&self) -> &str {
        "counter"
    }
    fn priority(&self) -> i32 {
        0
    }
    fn tick(&mut self, ctx: &TickContext) {
        self.log.lock().unwrap().push(ctx.tick_index);
    }
}

/// A config for a test: a listener that cannot collide, and no Mojang.
///
/// Booting now takes a real port. Loopback so no test opens a port to the
/// network, and port 0 so the operating system picks a free one — these tests
/// run in parallel with each other and with the crate's unit tests, and a
/// fixed port would make the suite pass alone and fail together.
///
/// **And `online_mode = false`, for a reason of the same shape.** It defaults
/// to `true`, and phase 3 answers that by loading the system root certificates
/// and generating an RSA key pair before it binds — the key pair by a prime
/// search whose own comment says it is "slow and unbounded". So every test
/// here that did not say otherwise paid seconds for a key it never used, and
/// bought a dependency on the host's certificate store along with it. Both
/// showed up: boots took four to five seconds each, occasionally overran the
/// thirty-second wait, and on a Mac whose keychain refused a trust-settings
/// read they failed outright with a certificate error two phases away from
/// anything the test was about. `protocol_conversations.rs` had already
/// written `online_mode = false` into every one of its configs by hand, which
/// is the same "a rule every caller has to remember" that the bind is here to
/// answer.
///
/// A config that names either setting itself is left alone: those tests are
/// about that setting.
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
    match config_text.strip_prefix("[server]\n") {
        Some(rest) => format!("[server]\n{settings}{rest}"),
        None => format!("[server]\n{settings}{config_text}"),
    }
}

/// Everything a test needs to watch and stop a running server.
struct Running {
    metrics: LiveMetrics,
    stop: StopHandle,
    worker: Option<std::thread::JoinHandle<Result<ShutdownReport, dust_server::ServerError>>>,
}

/// Boot a virtual-time server around one config file and one tick period of
/// parking per pass. The watchdog runs with an explicit recording policy at a
/// grace far beyond anything these tests spend: what they exercise is the
/// graceful path, and the timeout's *value* reaching the watchdog is asserted
/// by the unit tests inside the crate.
fn start(config_text: &str, extras: Vec<Box<dyn TickParticipant>>) -> Running {
    let clock = Arc::new(ManualClock::new());
    let options = ServerOptions {
        config_path: write_config(config_text),
        world_dir: test_world_dir(),
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        loop_parker: stepping(clock, TICK_NS),
        watchdog: WatchdogSetting::Custom(dust_server::WatchdogPolicy::custom(
            600_000_000_000,
            |_| {},
        )),
        extra_tasks: extras,
        ..ServerOptions::default()
    };
    let server = Server::new(options);
    Running {
        metrics: server.metrics(),
        stop: server.stop_handle(),
        worker: Some(std::thread::spawn(move || server.run())),
    }
}

/// Wait for progress, panicking if the server stalls short of `minimum`.
///
/// The same two corrections its sibling [`wait_for_ticks`] already carries,
/// applied here because this one was missed both times.
///
/// The deadline is in **seconds and not in iterations**. The earlier version
/// spun fifty million times on `yield_now`, which measures patience in yields
/// — and a yield costs almost nothing on an idle machine and a scheduling
/// quantum on a busy one, so the wait was *shortest* when nothing else was
/// running. Thirty seconds is patience with the OS scheduler, not tolerance
/// for a slow loop: this loop parks on a virtual clock and does no real work.
///
/// And a boot that failed is **joined and reported**, not described. A server
/// that never started leaves the tick count at zero, which to any waiting loop
/// is indistinguishable from one that is merely slow; noticing the thread has
/// finished turns a thirty-second timeout into a failure in milliseconds, and
/// joining it turns "the boot failed" into the phase error that actually
/// happened. Saying only the former cost an evening: the cause was two phases
/// away, in a certificate load this test had no idea it was doing.
fn wait_for(running: &mut Running, minimum_ticks: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if running.metrics.ticks_observed() >= minimum_ticks {
            return;
        }
        if running
            .worker
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            let outcome = running.worker.take().map(std::thread::JoinHandle::join);
            panic!(
                "the run thread exited before reaching {minimum_ticks} tick(s); \
                 the boot failed rather than stalled: {outcome:?}"
            );
        }
        // Sleeping rather than yielding: this thread has nothing to do until
        // the loop has run, and a spin denies it the core it is waiting on.
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!(
        "the server never reached {minimum_ticks} tick(s) in 30s; stuck at {}",
        running.metrics.ticks_observed()
    );
}

fn finish(mut running: Running) -> ShutdownReport {
    running.stop.request_stop();
    running
        .worker
        .take()
        .expect("the worker is only taken once")
        .join()
        .expect("the run thread finishes")
        .expect("the run is clean")
}

#[test]
fn boot_ticks_simulated_ctrl_c_and_shutdown_follow_the_documented_order() {
    let tick_log = Arc::new(Mutex::new(Vec::<u64>::new()));
    let counter: Box<dyn TickParticipant> = Box::new(Counter {
        log: Arc::clone(&tick_log),
    });
    let mut running = start("[server]\nshutdown_timeout_secs = 600\n", vec![counter]);
    wait_for(&mut running, 10);

    // A simulated ctrl-C is indistinguishable from the real keypress: the
    // same handle, the same flag, the same between-ticks observation.
    assert!(running.stop.request_stop(), "the first stop request wins");
    let report = finish(running);

    // The phase transcript is the whole point: four starts, four stops,
    // perfectly mirrored.
    assert_eq!(
        report.transcript_pairs(),
        vec![
            (Phase::ConfigLoad, Direction::Start),
            (Phase::WorldDirs, Direction::Start),
            (Phase::NetworkBind, Direction::Start),
            (Phase::TickLoop, Direction::Start),
            (Phase::TickLoop, Direction::Stop),
            (Phase::NetworkBind, Direction::Stop),
            (Phase::WorldDirs, Direction::Stop),
            (Phase::ConfigLoad, Direction::Stop),
        ]
    );
    assert!(report.transcript.iter().all(|e| !e.detail.is_empty()));

    // Clean means all of it: nothing interrupted, nothing panicked, the
    // single spawned thread retired.
    assert!(report.is_clean());
    assert!(!report.interrupted);
    assert!(report.thread_panics.is_empty());
    assert_eq!(report.threads_joined, 1, "only the watchdog was spawned");
    assert!(!report.watchdog_fired);
    assert!(report.uptime_ns > 0);

    // Ticks are deterministic under virtual time: contiguous indexes, every
    // one delivered, none invented.
    let ticks = report.ticks_run;
    assert!(ticks >= 10, "{ticks}");
    assert_eq!(
        *tick_log.lock().unwrap(),
        (0..ticks).collect::<Vec<_>>(),
        "the counter saw exactly ticks 0..{}",
        ticks
    );

    // Configuration reached the registry: the two config-gated placeholders
    // and the injected participant all ran, in priority order.
    //
    // `daylight`, `updates`, `items` and `furnaces` are between the probe and
    // the counter and none of them is a placeholder — they are the world's
    // clock, the world reacting to being changed, the item entities' physics
    // and the furnaces' burn timers, and they are the only real work the tick
    // loop has been given. A tick that reported the list without them would be
    // one where the sun never moved, nothing dropped on the ground ever moved,
    // no sand ever fell and nothing ever smelted, which is why the list is
    // spelled out here rather than counted. **The order is asserted and not
    // just the membership**: the clock moves before anything that could read
    // it, and a block that breaks this tick has to be broken before the items
    // pass runs, or its drops sit still for a tick before they pop.
    assert_eq!(
        report.participants,
        vec![
            "status-probe",
            "daylight",
            "updates",
            "items",
            "furnaces",
            "counter",
            "jvm-placeholder",
            "ore-workload"
        ]
    );
}

#[test]
fn a_stalled_loop_honours_the_configured_catchup_cap() {
    // max_catchup_ticks = 3, and every pass parks across thirty seconds of
    // virtual time. Each pass must repay exactly three ticks, surrender the
    // rest, and resynchronise — forever, without falling behind further.
    let clock = Arc::new(ManualClock::new());
    let options = ServerOptions {
        config_path: write_config("[server]\nmax_catchup_ticks = 3\n"),
        world_dir: test_world_dir(),
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        loop_parker: stepping(clock, 600 * TICK_NS),
        watchdog: WatchdogSetting::Disabled,
        ..ServerOptions::default()
    };
    let server = Server::new(options);
    let metrics = server.metrics();
    let stop = server.stop_handle();
    let worker = std::thread::spawn(move || server.run());

    wait_for_ticks(&metrics, 12, &worker);
    assert!(stop.request_stop());
    let report = worker.join().expect("finishes").expect("clean");

    assert!(
        report.surrendered_batches >= 4,
        "{:?}",
        report.surrendered_batches
    );
    assert_eq!(
        report.ticks_run,
        report.surrendered_batches * 3,
        "every pass repaid exactly the configured three ticks"
    );
    assert!(report.overall_timing.window_samples > 0, "timing survived");
}

/// Wait until the loop has run `minimum` ticks, giving up when the server
/// thread has ended or the deadline passes.
///
/// **It watches the worker, and that is the part worth keeping.** Waiting on a
/// counter alone means waiting on a server that may already have failed to
/// start, and the report is then "never reached 12 ticks" — which describes
/// the symptom and points nowhere. A server that could not bind its port ends
/// its thread immediately; noticing that turns a timeout into an accurate
/// failure in milliseconds.
///
/// The deadline is in seconds rather than in iterations for a related reason.
/// The earlier version spun fifty million times calling `yield_now`, which
/// measures patience in yields — and a yield costs almost nothing on an idle
/// machine and a scheduling quantum on a busy one, so the wait was *shortest*
/// when nothing else was running. Thirty seconds is patience with the OS
/// scheduler and not tolerance for a slow loop: the loop under test parks on a
/// virtual clock and does no real work, so a healthy one is done in
/// milliseconds.
fn wait_for_ticks<T>(metrics: &LiveMetrics, minimum: u64, worker: &std::thread::JoinHandle<T>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if metrics.ticks_observed() >= minimum {
            return;
        }
        assert!(
            !worker.is_finished(),
            "the server stopped after {} tick(s) without reaching {minimum}; \
             it most likely never started — join the worker for the error",
            metrics.ticks_observed()
        );
        // Sleeping rather than yielding: this thread has nothing to do until
        // the loop has run, and a spin denies it the core it is waiting on.
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!(
        "never reached {minimum} ticks in 30s; the loop ran {}",
        metrics.ticks_observed()
    );
}

#[test]
fn an_environment_override_reaches_the_operator_visible_surface() {
    // Container operators configure by environment; the dry-run summary is
    // the operator-visible face of configuration, so an override must be
    // indistinguishable there from a typed line in the file. The consumption
    // side — these values becoming engine cap, grace and log filter — is
    // proven by unit tests inside the crate, which can reach the runtime
    // structs without touching process state.
    let config = DustConfig::from_toml_and_env(
        "[server]\nmax_players = 40\n",
        "test",
        [
            ("DUST__SERVER__MAX_CATCHUP_TICKS".to_owned(), "7".to_owned()),
            (
                "DUST__SERVER__SHUTDOWN_TIMEOUT_SECS".to_owned(),
                "42".to_owned(),
            ),
            ("DUST__SERVER__LOG_LEVEL".to_owned(), "debug".to_owned()),
        ],
    )
    .expect("valid");
    let summary = dust_server::cli::render_summary(&config);
    assert!(summary.contains("max_catchup_ticks     7"), "{summary}");
    assert!(summary.contains("shutdown_timeout_secs 42s"), "{summary}");
    assert!(summary.contains("log_level             debug"), "{summary}");
}
