//! The conversations a client can have with this server, checked by speaking
//! the protocol.
//!
//! # Why the client here is hand-written
//!
//! This test writes VarInts and reads length prefixes by hand instead of using
//! `dust-net`'s encoder. That is deliberate and it is the only reason the test
//! is worth running.
//!
//! `dust-net` and the server share one framing implementation. A client built
//! on it agrees with the server about where a frame starts by construction, so
//! it would pass under any self-consistent convention — including a wrong one —
//! and prove only that the code agrees with itself. The bytes below are written
//! from the protocol as a third party states it: a VarInt length, then a VarInt
//! packet id, then the body. If Dust's framing drifted, this is the test that
//! notices, because nothing it does was compiled from the same source.
//!
//! What it still cannot prove is that a *real* client is happy — a vanilla
//! client renders a document this test would accept, and that is the
//! differential harness's job, not this file's.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dust_server::clock::{Clock, ManualClock};
use dust_server::engine::TICK_NS;
use dust_server::stop::{Parker, StepParker, StopHandle};
use dust_server::{LiveMetrics, Server, ServerOptions, WatchdogSetting};

// ---------------------------------------------------------------------------
// A client that shares no code with the server
// ---------------------------------------------------------------------------

fn write_var_int(mut value: i32, out: &mut Vec<u8>) {
    // The protocol's own definition, written out rather than called: seven bits
    // per byte, low group first, the high bit meaning "another byte follows".
    // A `u32` cast so that a negative number shifts in zeros, which is what
    // makes -1 five bytes rather than an infinite loop.
    let mut bits = value as u32;
    loop {
        let byte = (bits & 0x7f) as u8;
        bits >>= 7;
        if bits == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    let _ = &mut value;
}

fn read_var_int(stream: &mut TcpStream) -> i32 {
    let mut result: i32 = 0;
    for shift in 0..5 {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).expect("a VarInt byte");
        result |= i32::from(byte[0] & 0x7f) << (shift * 7);
        if byte[0] & 0x80 == 0 {
            return result;
        }
    }
    panic!("a VarInt longer than five bytes is not one");
}

fn write_string(text: &str, out: &mut Vec<u8>) {
    write_var_int(text.len() as i32, out);
    out.extend_from_slice(text.as_bytes());
}

/// Send one uncompressed frame: length, then id, then body.
fn send_frame(stream: &mut TcpStream, id: i32, body: &[u8]) {
    let mut payload = Vec::new();
    write_var_int(id, &mut payload);
    payload.extend_from_slice(body);
    let mut frame = Vec::new();
    write_var_int(payload.len() as i32, &mut frame);
    frame.extend_from_slice(&payload);
    stream.write_all(&frame).expect("write a frame");
}

/// Receive one uncompressed frame, returning its id and body.
fn recv_frame(stream: &mut TcpStream) -> (i32, Vec<u8>) {
    let len = read_var_int(stream);
    assert!(len > 0, "a frame is at least its packet id");
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).expect("the frame body");
    let mut cursor = 0usize;
    let mut id: i32 = 0;
    for shift in 0..5 {
        let byte = payload[cursor];
        cursor += 1;
        id |= i32::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            break;
        }
    }
    (id, payload[cursor..].to_vec())
}

/// Read a length-prefixed string off the front of a slice, returning it and
/// the rest.
fn read_string_at(bytes: &[u8]) -> (&str, &[u8]) {
    let (len, rest) = read_var_int_from(bytes);
    let len = len as usize;
    (
        std::str::from_utf8(&rest[..len]).expect("the wire is UTF-8"),
        &rest[len..],
    )
}

/// Read a VarInt out of a slice, returning it and the rest.
fn read_var_int_from(bytes: &[u8]) -> (i32, &[u8]) {
    let mut result: i32 = 0;
    for (shift, i) in (0..5).enumerate() {
        let byte = bytes[i];
        result |= i32::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            return (result, &bytes[i + 1..]);
        }
    }
    panic!("a VarInt longer than five bytes is not one");
}

/// Receive a frame after Set Compression.
///
/// The compressed format inserts one field: an uncompressed-length VarInt after
/// the frame length. Zero means "the rest is not compressed", which is what a
/// server sends for anything under the threshold — and every packet this test
/// receives after the switch is under 256 bytes, so the zero case is the one
/// exercised. That is not a gap being papered over: it is the case a real
/// client meets for keepalives and acknowledgements, and getting it wrong is
/// how a server appears to work until somebody sends a chunk.
fn recv_compressed_frame(stream: &mut TcpStream) -> (i32, Vec<u8>) {
    let len = read_var_int(stream);
    assert!(len > 0, "a frame is at least its uncompressed-length field");
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).expect("the frame body");
    let (uncompressed_len, rest) = read_var_int_from(&payload);
    // Zero means "the rest is not compressed", which is what the server sends
    // for anything under the threshold. Above it, the rest is a zlib stream and
    // this field is what it inflates to — checked rather than trusted, because
    // that length is the only thing standing between a decompressor and a peer
    // that lies about how much it is about to produce.
    let inflated = if uncompressed_len == 0 {
        rest.to_vec()
    } else {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(rest)
            .read_to_end(&mut out)
            .expect("the frame is a zlib stream");
        assert_eq!(
            out.len(),
            uncompressed_len as usize,
            "the announced uncompressed length must be the real one"
        );
        out
    };
    let (id, body) = read_var_int_from(&inflated);
    (id, body.to_vec())
}

/// Send a frame after Set Compression, below the threshold.
fn send_compressed_frame(stream: &mut TcpStream, id: i32, body: &[u8]) {
    let mut payload = Vec::new();
    write_var_int(0, &mut payload); // uncompressed: this packet is small
    write_var_int(id, &mut payload);
    payload.extend_from_slice(body);
    let mut frame = Vec::new();
    write_var_int(payload.len() as i32, &mut frame);
    frame.extend_from_slice(&payload);
    stream.write_all(&frame).expect("write a frame");
}

/// The handshake, addressed to `addr`, asking for `next_state`.
fn handshake(stream: &mut TcpStream, protocol: i32, addr: SocketAddr, next_state: i32) {
    let mut body = Vec::new();
    write_var_int(protocol, &mut body);
    write_string(&addr.ip().to_string(), &mut body);
    body.extend_from_slice(&addr.port().to_be_bytes());
    write_var_int(next_state, &mut body);
    send_frame(stream, 0x00, &body);
}

/// Read a length-prefixed string that is the whole of `body`.
fn read_string(body: &[u8]) -> String {
    let mut cursor = 0usize;
    let mut len: i32 = 0;
    for shift in 0..5 {
        let byte = body[cursor];
        cursor += 1;
        len |= i32::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            break;
        }
    }
    let end = cursor + len as usize;
    assert_eq!(end, body.len(), "the string is the whole body");
    String::from_utf8(body[cursor..end].to_vec()).expect("the text is UTF-8")
}

fn read_status_json(stream: &mut TcpStream) -> String {
    let (id, body) = recv_frame(stream);
    assert_eq!(id, 0x00, "status_response is id 0 clientbound in status");
    read_string(&body)
}

// ---------------------------------------------------------------------------
// A server, on a port the operating system chose
// ---------------------------------------------------------------------------

struct Running {
    addr: SocketAddr,
    stop: StopHandle,
    worker: Option<
        std::thread::JoinHandle<Result<dust_server::ShutdownReport, dust_server::ServerError>>,
    >,
    #[allow(dead_code)]
    metrics: LiveMetrics,
}

impl Running {
    fn finish(mut self) -> dust_server::ShutdownReport {
        self.stop.request_stop();
        self.worker
            .take()
            .expect("taken once")
            .join()
            .expect("the run thread finishes")
            .expect("the run is clean")
    }
}

fn write_config(text: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "dust-ping-test-{}-{}.toml",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&path, text).expect("write the config");
    path
}

fn stepping(clock: Arc<ManualClock>, step: u64) -> dust_server::server::ParkerFactory {
    Arc::new(move |_, _| Box::new(StepParker::new(Arc::clone(&clock), step)) as Box<dyn Parker>)
}

/// Boot a server on loopback, port 0, and wait until it says which port it took.
///
/// The port comes from the server rather than from a socket this test bound
/// first. Anything this test bound would have to be released before the server
/// could take it, and the gap between the release and the bind is a race with
/// every other process on the machine — the classic way a port-picking test
/// becomes the flaky one.
fn start(extra_config: &str) -> Running {
    let dir = std::env::temp_dir().join(format!(
        "dust-world-{}-{}",
        std::process::id(),
        ICON_SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    start_in(&dir, extra_config)
}

/// A server whose world lives in `world_dir`, so two runs can share one.
///
/// Every test gets its own directory by default: they run in parallel, and a
/// shared world would make one test's blocks appear in another's.
fn start_in(world_dir: &std::path::Path, extra_config: &str) -> Running {
    let clock = Arc::new(ManualClock::new());
    // A view distance of two, said out loud — unless the caller named one, in
    // which case theirs stands and a second key would be a TOML error rather
    // than an override. The default is eight, which is 289 columns per join: a
    // megabyte of packets and a second of lighting, multiplied by every test in
    // this file. Two is twenty-five, which is what the assertions below count,
    // and a test that counts twenty-five columns should be the thing that asked
    // for them.
    let distance = if extra_config.contains("view_distance") {
        ""
    } else {
        "view_distance = 2\n"
    };
    let config =
        format!("[server]\nbind = \"127.0.0.1:0\"\nonline_mode = false\n{distance}{extra_config}");

    let options = ServerOptions {
        config_path: write_config(&config),
        world_dir: world_dir.to_path_buf(),
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        loop_parker: stepping(Arc::clone(&clock), TICK_NS),
        watchdog: WatchdogSetting::Custom(dust_server::WatchdogPolicy::custom(
            600_000_000_000,
            |_| {},
        )),
        ..ServerOptions::default()
    };
    let server = Server::new(options);
    let metrics = server.metrics();
    let stop = server.stop_handle();
    let worker = std::thread::spawn(move || server.run());

    let mut addr = None;
    for _ in 0..50_000_000 {
        if let Some(bound) = metrics.bound_addr() {
            addr = Some(bound);
            break;
        }
        assert!(
            !worker.is_finished(),
            "the run thread exited before binding: the boot failed rather than stalled"
        );
        std::thread::yield_now();
    }
    let addr = addr.expect("the listener publishes the address it took");
    assert_ne!(addr.port(), 0, "port 0 means 'choose one', never stays 0");

    Running {
        addr,
        stop,
        worker: Some(worker),
        metrics,
    }
}

fn connect(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("connect to the listener");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("a read timeout");
    stream
}

/// A client that has reached Play, and what it has been told since.
///
/// Counters rather than a log of packets: the assertions are about how many
/// columns crossed the wire, and keeping every chunk packet to count them
/// later would be a hundred megabytes to answer a question three integers
/// answer.
struct Joined {
    chunks: usize,
    forgets: usize,
    centres: usize,
    /// The chunk packets themselves, for a test that needs to look inside
    /// them. Kept only because one does — twenty-five columns is a megabyte,
    /// which is fine for a test and would not be for a client.
    chunk_bodies: Vec<Vec<u8>>,
    /// Bodies this client has been told to render.
    spawned_entities: usize,
    /// Tab-list rows it has been given.
    player_infos: usize,
    /// World effects — block-break particles and the like.
    level_events: usize,
    /// Entity metadata updates: how somebody is standing.
    postures: usize,
    /// Where the server teleported the player on arrival. Captured during the
    /// join rather than waited for afterwards, because the join has already
    /// read past it by the time it returns — a later `wait_for` would block
    /// until the read timeout and then say the packet never came.
    spawned_at: Option<(f64, f64, f64)>,
}

impl Joined {
    /// Read whatever is waiting, and stop when nothing is.
    ///
    /// A short read timeout is the stopping condition, which is a wall-clock
    /// dependency and therefore worth naming: it is not a claim about how fast
    /// the server is, only about the socket being empty *now*. The counts this
    /// test asserts are cumulative, so a drain that stopped early is corrected
    /// by the next one; only the final `drain_until_quiet` has to be complete,
    /// and it waits longer for exactly that reason.
    fn drain(&mut self, stream: &mut TcpStream) {
        self.read_for(stream, Duration::from_millis(50));
    }

    /// Read until a packet of `id` arrives, returning its body.
    ///
    /// Bounded by the socket's read timeout rather than by a count, so a
    /// packet that never comes fails as a `None` here instead of hanging the
    /// suite.
    fn wait_for(&mut self, stream: &mut TcpStream, id: i32) -> Option<Vec<u8>> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a read timeout");
        while let Some((got, body)) = try_recv_compressed_frame(stream) {
            // Counted *before* the match on `id`, so the counters stay a
            // complete record of everything this client has been told. The
            // first version returned the awaited packet without counting it,
            // and a test that both waited for a body and asserted how many
            // bodies had arrived was then off by exactly one — with the
            // assertion reading as though the packet had never come.
            self.count(&body, got, stream);
            if got == id {
                return Some(body);
            }
        }
        None
    }

    /// Fold one packet into the counters, answering a keep-alive on the way.
    fn count(&mut self, body: &[u8], id: i32, stream: &mut TcpStream) {
        match id {
            39 => {
                self.chunks += 1;
                self.chunk_bodies.push(body.to_vec());
            }
            33 => self.forgets += 1,
            84 => self.centres += 1,
            1 => self.spawned_entities += 1,
            62 => self.player_infos += 1,
            40 => self.level_events += 1,
            88 => self.postures += 1,
            64 => {
                self.spawned_at = Some((
                    f64::from_be_bytes(body[0..8].try_into().expect("eight bytes")),
                    f64::from_be_bytes(body[8..16].try_into().expect("eight bytes")),
                    f64::from_be_bytes(body[16..24].try_into().expect("eight bytes")),
                ));
            }
            // Answered so the connection survives a test that outlasts the
            // keep-alive period.
            38 => send_compressed_frame(stream, 24, body),
            29 => panic!("the server disconnected"),
            _ => {}
        }
    }

    /// Read until `done` is satisfied, or give up and say so.
    ///
    /// The bounded form of draining, and the one every count in this file
    /// should be taken after. A plain drain stops at the first quiet moment,
    /// which is a claim about the socket and not about the server; a
    /// `wait_for` cannot be used when the packet in question may already have
    /// been read by an earlier drain. This waits for a *condition on what has
    /// been seen*, which is the thing the caller actually means.
    ///
    /// **The bound is in seconds and not in passes.** It was forty passes, and
    /// a pass is "read until fifty milliseconds of silence" — so the patience
    /// was denominated in the server's own gaps, and a server that was merely
    /// slow ran out of them and reported a shortfall as though the columns had
    /// never been sent. Thirty seconds is a stall guard and not a timing
    /// assumption: what these tests wait for arrives in milliseconds on an
    /// idle machine, and a run that reaches the deadline has a server that
    /// stopped sending — a failure worth reporting as itself.
    fn drain_until(&mut self, stream: &mut TcpStream, done: impl Fn(&Self) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if done(self) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            self.drain(stream);
        }
    }

    /// Read until three quarters of a second of silence.
    ///
    /// **For asserting an absence, and nothing else.** Silence means nothing
    /// has arrived *yet*, and on a slow server "yet" includes "before it got
    /// round to it" — so every count this file takes after a quiet drain was a
    /// count of whatever had happened to arrive by then. Three of them failed
    /// that way under load, one of them on CI. They wait for what they assert
    /// now, with [`Self::drain_until`].
    ///
    /// What is left is the case that has no positive event to wait for: a
    /// packet that must **not** come. There the passage of quiet time is the
    /// evidence, and the two remaining callers say which absence they mean.
    fn drain_until_quiet(&mut self, stream: &mut TcpStream) {
        self.read_for(stream, Duration::from_millis(750));
    }

    fn read_for(&mut self, stream: &mut TcpStream, quiet: Duration) {
        stream
            .set_read_timeout(Some(quiet))
            .expect("a read timeout");
        while let Some((id, body)) = try_recv_compressed_frame(stream) {
            self.count(&body, id, stream);
        }
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("a read timeout");
    }
}

/// Run the whole join, ending with the client in Play and the arrival counted.
///
/// The sequence is asserted in its own test; here it is walked through so a
/// later test can start from a joined player without repeating it.
fn join(stream: &mut TcpStream, addr: SocketAddr) -> Joined {
    join_as(stream, addr, "Walker")
}

/// Join under a chosen name, so two clients in one test are two players.
fn join_as(stream: &mut TcpStream, addr: SocketAddr, name: &str) -> Joined {
    join_inner(stream, addr, name, None)
}

/// Join, telling the server how far this client wants to see.
///
/// The settings packet is written by hand like every other packet in this
/// file: a client that built it out of the server's own definitions would
/// agree with the server about a layout neither of them checked.
fn join_asking_for_view_distance(
    stream: &mut TcpStream,
    addr: SocketAddr,
    name: &str,
    view_distance: u8,
) -> Joined {
    join_inner(stream, addr, name, Some(view_distance))
}

fn join_inner(
    stream: &mut TcpStream,
    addr: SocketAddr,
    name: &str,
    view_distance: Option<u8>,
) -> Joined {
    handshake(stream, 767, addr, 2);
    let mut body = Vec::new();
    write_string(name, &mut body);
    body.extend_from_slice(&[0u8; 16]);
    send_frame(stream, 0x00, &body);

    let (id, _) = recv_frame(stream);
    assert_eq!(id, 0x03, "set_compression");
    let (id, _) = recv_compressed_frame(stream);
    assert_eq!(id, 0x02, "login_finished");
    send_compressed_frame(stream, 0x03, &[]);

    let mut counted = Joined {
        chunks: 0,
        forgets: 0,
        centres: 0,
        chunk_bodies: Vec::new(),
        spawned_entities: 0,
        player_infos: 0,
        level_events: 0,
        postures: 0,
        spawned_at: None,
    };
    if let Some(distance) = view_distance {
        // `client_information`: locale, view distance, chat mode, colours,
        // skin parts, main hand, text filtering, server listings.
        let mut settings = Vec::new();
        write_string("en_gb", &mut settings);
        settings.push(distance);
        write_var_int(0, &mut settings); // chat mode: enabled
        settings.push(1); // chat colours
        settings.push(0x7f); // every skin part
        write_var_int(1, &mut settings); // main hand: right
        settings.push(0); // text filtering off
        settings.push(1); // allow server listings
        send_compressed_frame(stream, 0x00, &settings);
    }
    loop {
        let (id, body) = recv_compressed_frame(stream);
        match id {
            0x0e => send_compressed_frame(stream, 0x07, &body),
            0x03 => {
                send_compressed_frame(stream, 0x03, &[]);
                break;
            }
            _ => {}
        }
    }
    // Play: the join packet, the position, then the columns and the event.
    loop {
        let (id, body) = recv_compressed_frame(stream);
        match id {
            39 => {
                counted.chunks += 1;
                counted.chunk_bodies.push(body.clone());
            }
            33 => counted.forgets += 1,
            84 => counted.centres += 1,
            1 => counted.spawned_entities += 1,
            62 => counted.player_infos += 1,
            40 => counted.level_events += 1,
            88 => counted.postures += 1,
            64 => {
                counted.spawned_at = Some((
                    f64::from_be_bytes(body[0..8].try_into().expect("eight bytes")),
                    f64::from_be_bytes(body[8..16].try_into().expect("eight bytes")),
                    f64::from_be_bytes(body[16..24].try_into().expect("eight bytes")),
                ));
            }
            34 => break, // the loading screen is over
            38 => send_compressed_frame(stream, 24, &body),
            _ => {}
        }
    }
    counted
}

/// One frame, or `None` if the socket went quiet inside its read timeout.
fn try_recv_compressed_frame(stream: &mut TcpStream) -> Option<(i32, Vec<u8>)> {
    let mut first = [0u8; 1];
    match stream.read_exact(&mut first) {
        Ok(()) => {}
        Err(_) => return None,
    }
    // The length prefix, continued by hand because one byte of it is already
    // read and a VarInt does not say up front how long it is.
    let mut len = i32::from(first[0] & 0x7f);
    let mut shift = 7;
    let mut byte = first[0];
    while byte & 0x80 != 0 {
        let mut next = [0u8; 1];
        stream.read_exact(&mut next).expect("a VarInt byte");
        byte = next[0];
        len |= i32::from(byte & 0x7f) << shift;
        shift += 7;
    }

    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).expect("the frame body");
    let (uncompressed_len, rest) = read_var_int_from(&payload);
    let inflated = if uncompressed_len == 0 {
        rest.to_vec()
    } else {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(rest)
            .read_to_end(&mut out)
            .expect("a zlib stream");
        out
    };
    let (id, body) = read_var_int_from(&inflated);
    Some((id, body.to_vec()))
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

#[test]
fn a_client_speaking_raw_protocol_gets_the_server_list_entry() {
    let running = start("motd = \"A test server\"\nmax_players = 42\n");
    let addr = running.addr;

    let mut stream = connect(addr);
    handshake(&mut stream, 767, addr, 1);
    send_frame(&mut stream, 0x00, &[]);
    let json = read_status_json(&mut stream);

    assert!(json.contains(r#""protocol":767"#), "{json}");
    assert!(json.contains(r#""name":"1.21.1""#), "{json}");
    assert!(json.contains(r#""max":42"#), "{json}");
    assert!(json.contains(r#""online":0"#), "{json}");
    assert!(json.contains("A test server"), "{json}");

    // The ping the client uses to measure its round trip. The eight bytes come
    // back unexamined, which is the whole contract.
    let payload: i64 = 0x0123_4567_89ab_cdef;
    send_frame(&mut stream, 0x01, &payload.to_be_bytes());
    let (id, body) = recv_frame(&mut stream);
    assert_eq!(id, 0x01, "pong_response is id 1");
    assert_eq!(
        i64::from_be_bytes(body.try_into().expect("eight bytes")),
        payload
    );

    let report = running.finish();
    assert!(
        report
            .transcript
            .iter()
            .any(|e| e.detail.contains("ping(s)")),
        "the teardown must account for the connections served: {:?}",
        report.transcript
    );
}

#[test]
fn an_offline_login_runs_the_whole_configuration_exchange_and_reaches_play() {
    let running = start("");
    let addr = running.addr;
    let mut stream = connect(addr);
    handshake(&mut stream, 767, addr, 2);

    // Login Start: a name and the client's guess at its own profile id.
    let mut body = Vec::new();
    write_string("Tester", &mut body);
    body.extend_from_slice(&[0u8; 16]);
    send_frame(&mut stream, 0x00, &body);

    // Set Compression comes first, at vanilla's threshold. From here on the
    // client's frames carry an uncompressed-length prefix, which is why this
    // test reads its last frames through the compressed reader below — the
    // switch is part of the protocol, not an optimisation to skip in a test.
    let (id, body) = recv_frame(&mut stream);
    assert_eq!(id, 0x03, "set_compression is id 3 clientbound in login");
    assert_eq!(read_var_int_from(&body).0, 256, "vanilla's threshold");

    // Login Success. Offline mode derives the profile id from the name, so it
    // is emphatically not the sixteen zero bytes the client sent.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 0x02, "login_finished is id 2 clientbound in login");
    assert_ne!(
        &body[..16],
        &[0u8; 16],
        "an offline id is derived, not echoed"
    );
    let (name_len, rest) = read_var_int_from(&body[16..]);
    let name = String::from_utf8(rest[..name_len as usize].to_vec()).expect("UTF-8");
    assert_eq!(name, "Tester");

    // Login Acknowledged moves both ends into configuration.
    send_compressed_frame(&mut stream, 0x03, &[]);

    // Configuration, in the order a real 1.21.1 server sends it. Captured from
    // one rather than read off a wiki, because the order is load-bearing and
    // the wire is the only place it is written down.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 0x01, "custom_payload carries the brand first");
    let (channel, rest) = read_string_at(&body);
    assert_eq!(channel, "minecraft:brand");
    assert_eq!(
        read_string_at(rest).0,
        "Dust",
        "not a lie about being vanilla"
    );

    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 0x0c, "update_enabled_features");
    let (count, rest) = read_var_int_from(&body);
    assert_eq!(count, 1);
    assert_eq!(read_string_at(rest).0, "minecraft:vanilla");

    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 0x0e, "select_known_packs");
    // Echoed back verbatim, which is what a client that has the pack does.
    send_compressed_frame(&mut stream, 0x07, &body);

    // Eleven registries, names only, and then the tags. The entry payloads are
    // absent because the pack was acknowledged, and absent is not the same as
    // empty: an empty definition would put the client in a world with no
    // dimension types.
    let mut seen = Vec::new();
    let mut tag_registries: Vec<(String, i32, i32)> = Vec::new();
    loop {
        let (id, body) = recv_compressed_frame(&mut stream);
        if id == 0x03 {
            break; // finish_configuration
        }
        if id == 0x0d {
            // update_tags, after the registries and before the finish, which
            // is where vanilla puts it. Read whole rather than skipped: a tag
            // is ids into a registry, and this is the only place the entries
            // are counted by something that did not build them.
            let (registries, mut rest) = read_var_int_from(&body);
            for _ in 0..registries {
                let (registry, after) = read_string_at(rest);
                let registry = registry.to_owned();
                let (tags, after) = read_var_int_from(after);
                rest = after;
                let mut entries = 0;
                for _ in 0..tags {
                    let (_name, after) = read_string_at(rest);
                    let (count, after) = read_var_int_from(after);
                    rest = after;
                    for _ in 0..count {
                        let (_id, after) = read_var_int_from(rest);
                        rest = after;
                    }
                    entries += count;
                }
                tag_registries.push((registry, tags, entries));
            }
            assert!(rest.is_empty(), "update_tags had trailing bytes");
            continue;
        }
        assert_eq!(id, 0x07, "only registry_data and update_tags come between");
        let (registry, rest) = read_string_at(&body);
        let registry = registry.to_owned();
        let (count, mut rest) = read_var_int_from(rest);
        for _ in 0..count {
            let (_entry, after) = read_string_at(rest);
            assert_eq!(after[0], 0, "no entry of {registry} may carry a payload");
            rest = &after[1..];
        }
        assert!(rest.is_empty(), "{registry} had trailing bytes");
        seen.push((registry, count));
    }

    // The thirteen tag registries, their tag counts and their flattened
    // membership counts, exactly as a real 1.21.1 server sent them: 514 tags
    // and 6,362 ids in 25,200 bytes. Read here by a client that shares no code
    // with the server, so this is the numbers meeting the wire and not the
    // generated table agreeing with itself.
    assert_eq!(
        tag_registries,
        vec![
            ("minecraft:block".to_owned(), 184, 3289),
            ("minecraft:entity_type".to_owned(), 34, 252),
            ("minecraft:worldgen/biome".to_owned(), 70, 554),
            ("minecraft:game_event".to_owned(), 5, 119),
            ("minecraft:item".to_owned(), 147, 1512),
            ("minecraft:point_of_interest_type".to_owned(), 3, 30),
            ("minecraft:enchantment".to_owned(), 22, 301),
            ("minecraft:fluid".to_owned(), 2, 4),
            ("minecraft:damage_type".to_owned(), 32, 176),
            ("minecraft:banner_pattern".to_owned(), 9, 42),
            ("minecraft:cat_variant".to_owned(), 2, 21),
            ("minecraft:instrument".to_owned(), 3, 16),
            ("minecraft:painting_variant".to_owned(), 1, 46),
        ]
    );

    // The eleven and their counts, as the real server sent them.
    assert_eq!(
        seen,
        vec![
            ("minecraft:worldgen/biome".to_owned(), 64),
            ("minecraft:chat_type".to_owned(), 7),
            ("minecraft:trim_pattern".to_owned(), 18),
            ("minecraft:trim_material".to_owned(), 10),
            ("minecraft:wolf_variant".to_owned(), 9),
            ("minecraft:painting_variant".to_owned(), 50),
            ("minecraft:dimension_type".to_owned(), 4),
            ("minecraft:damage_type".to_owned(), 47),
            ("minecraft:banner_pattern".to_owned(), 43),
            ("minecraft:enchantment".to_owned(), 42),
            ("minecraft:jukebox_song".to_owned(), 19),
        ]
    );

    // Acknowledge, which is what actually moves both ends into Play.
    send_compressed_frame(&mut stream, 0x03, &[]);

    // And Play is a world. The join packet, then the position, then the
    // columns, then the event that ends the loading screen.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 43, "login is id 43 clientbound in play");
    assert_eq!(
        i32::from_be_bytes(body[0..4].try_into().expect("four bytes")),
        1,
        "the player's entity id"
    );
    let (count, rest) = read_var_int_from(&body[5..]);
    assert_eq!(count, 3, "three dimensions are named");
    assert_eq!(read_string_at(rest).0, "minecraft:overworld");

    // Abilities, and this is the one whose absence is felt: a creative client
    // that is never sent it cannot fly, because the flags are where flight is
    // granted and the game mode in the join packet does not grant it. Found by
    // diffing this server's join sequence against a real one's.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 56, "player_abilities");
    assert_ne!(body[0] & 0x04, 0, "ALLOW_FLYING, or creative mode walks");
    assert_ne!(body[0] & 0x01, 0, "and invulnerable, as creative is");

    // The clock, and a **positive** time of day, which is what tells a client
    // the cycle runs. It used to be negative here — the protocol's way of
    // saying the sun is frozen — because nothing ticked a clock and every
    // player who ever joined Dust stood in a permanent midday. A world that
    // has never been played opens at dawn, tick 1,000, which is where
    // Minecraft opens a new one.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 100, "set_time");
    let world_age = i64::from_be_bytes(body[0..8].try_into().expect("eight bytes"));
    let time_of_day = i64::from_be_bytes(body[8..16].try_into().expect("eight bytes"));
    assert!(
        time_of_day > 0,
        "a negative time_of_day tells the client the cycle is frozen, which is \
         what this server used to say and no longer means: {time_of_day}"
    );
    // How far past dawn depends on how long this server has been up, and this
    // one is driven by a clock the harness winds, so the number is not
    // predictable. What *is* exact is the relationship: a fresh world starts
    // at game time 0 and day time 1,000, and with the cycle running both
    // advance together for ever. So the gap is the world's opening hour and
    // nothing else, whatever either number reached.
    assert_eq!(
        time_of_day - world_age,
        1_000,
        "a fresh world opens at dawn and the two clocks then move as one: \
         age {world_age}, sun {time_of_day}"
    );
    assert!(
        world_age > 0,
        "and the world has actually run some ticks by now, so the clock is \
         being moved rather than merely initialised: {world_age}"
    );

    // What may be typed after a slash. Without it a client's tab completion
    // offers nothing and its parser calls every command unknown, so a `/time`
    // that the server would happily run looks broken while it is being typed.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 17, "commands");
    let (nodes, _) = read_var_int_from(&body);
    assert_eq!(nodes, 14, "the root and the whole of the /time subtree");
    assert!(
        String::from_utf8_lossy(&body).contains("midnight"),
        "and the literals a client completes against are in it"
    );

    let (id, _) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 86, "set_default_spawn_position");

    // The position comes before the chunks, and that order matters: a client
    // uses where it is to decide which columns it wants, and one told about
    // columns first throws them away.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 64, "player_position");
    let x = f64::from_be_bytes(body[0..8].try_into().expect("eight bytes"));
    let y = f64::from_be_bytes(body[8..16].try_into().expect("eight bytes"));
    let z = f64::from_be_bytes(body[16..24].try_into().expect("eight bytes"));
    // The half-block offsets are not cosmetic: integer x and z spawn a player
    // on a block corner and the first physics tick pushes them off it.
    assert_eq!((x, y, z), (0.5, -59.0, 0.5));

    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 84, "set_chunk_cache_center");
    assert_eq!(read_var_int_from(&body).0, 0, "centred on chunk 0");

    // Twenty-five columns: a radius of two, which is (2*2+1)^2.
    let mut chunks = 0;
    let event = loop {
        let (id, body) = recv_compressed_frame(&mut stream);
        if id == 34 {
            break body;
        }
        assert_eq!(id, 39, "only chunks come between");
        chunks += 1;
        assert!(chunks <= 25, "more columns than a radius of two holds");
    };
    assert_eq!(chunks, 25, "every column within the radius");
    assert_eq!(
        event[0], 13,
        "game event 13 is what ends the loading screen; without it the terrain \
         arrives and the client keeps waiting"
    );

    // Health, which vanilla sends last in the join burst and Dust sends in
    // the same place. Not decoration: a client may treat this packet as the
    // moment it is in the world, and one sent before the position leaves such
    // a client believing it spawned at the origin.
    let (id, health) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 93, "set_health");
    assert_eq!(
        f32::from_be_bytes([health[0], health[1], health[2], health[3]]),
        20.0,
        "full health"
    );

    // What the player is carrying, all forty-six slots plus the cursor. This
    // is the only place the whole container goes out: a join has nothing to
    // compare against, and every change after it is a single slot.
    //
    // Sent *before* the arrival is announced, which is where vanilla sends it
    // too — a client that is in the world with no inventory renders an empty
    // hotbar for as long as the round trip takes.
    let (id, container) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 19, "container_set_content");
    let (window, rest) = (container[0], &container[1..]);
    assert_eq!(window, 0, "the player's own inventory is window 0");
    let (state_id, rest) = read_var_int_from(rest);
    assert_eq!(state_id, 1, "the first sync of the session");
    let (slots, rest) = read_var_int_from(rest);
    assert_eq!(slots, 46, "vanilla's own 0..=45");
    // A fresh player carries nothing, so every slot and the cursor are a
    // single zero byte: forty-six slots plus the carried item.
    assert_eq!(rest, vec![0u8; 47], "forty-seven empty stacks");

    let (id, carried) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 83, "set_carried_item");
    assert_eq!(carried, vec![0], "hotbar slot 0");

    // A player is told about their own arrival, as on every server since
    // 2010, and it comes before the keep-alive because the roster is joined
    // as soon as the world is on screen.
    let (id, announcement) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 108, "system_chat");
    let text = String::from_utf8_lossy(&announcement);
    assert!(text.contains("Tester"), "{text:?}");
    assert!(text.contains("joined the game"), "{text:?}");

    // And the connection stays up. A keep-alive arrives and is answered, which
    // is what turns "the packets were sent" into "the player is still there".
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 38, "keep_alive");
    assert_eq!(body.len(), 8, "eight opaque bytes");
    send_compressed_frame(&mut stream, 24, &body);

    // The player is still standing in the world when the server stops, which
    // is what the teardown must say: one login, and one still online. A
    // counter that only counted finished sessions would report neither, and
    // the server-list ping quotes that same number.
    let report = running.finish();
    assert!(
        report
            .transcript
            .iter()
            .any(|e| e.detail.contains("1 login(s)") && e.detail.contains("1 still online")),
        "the teardown must account for the player: {:?}",
        report.transcript
    );
}

/// Phase 3's exit criterion, in the part of it that exists: walk a long way
/// and require the world to keep arriving.
///
/// The numbers are checked rather than the behaviour being watched. A view of
/// radius two is a five-by-five square, so each chunk boundary crossed sends a
/// five-column edge and forgets another — and one thousand blocks east crosses
/// sixty-two of them. A server that stopped streaming, or that resent columns
/// the client already held, would produce a different count and no other
/// symptom.
#[test]
fn a_player_walking_a_thousand_blocks_is_streamed_the_world_as_they_go() {
    let running = start("");
    let addr = running.addr;
    let mut stream = connect(addr);
    let mut client = join(&mut stream, addr);

    // Twenty-five columns on arrival, and one centre.
    assert_eq!(client.chunks, 25, "the square at spawn");
    assert_eq!(client.forgets, 0, "nothing to forget yet");
    assert_eq!(client.centres, 1);

    // Due east, one block at a time, reading whatever comes back. The reads are
    // interleaved rather than saved to the end because the outbound queue is
    // bounded: a client that sends a thousand packets without reading is a
    // client the server is entitled to make wait.
    let mut x = 0.5f64;
    for step in 0..1000 {
        x += 1.0;
        let mut body = Vec::new();
        body.extend_from_slice(&x.to_be_bytes());
        body.extend_from_slice(&(-59.0f64).to_be_bytes());
        body.extend_from_slice(&0.5f64.to_be_bytes());
        body.push(1); // on_ground
        send_compressed_frame(&mut stream, 26, &body);
        if step % 16 == 0 {
            client.drain(&mut stream);
        }
    }
    // Waited for, not listened for. The drains inside the loop stop at the
    // first fifty-millisecond gap, which says the socket is empty and nothing
    // at all about whether the server has finished — so the three counts
    // asserted below are the three counts to wait for. They stay *equalities*
    // afterwards, which is what still catches a resend as an overshoot.
    assert!(
        client.drain_until(&mut stream, |c| c.centres >= 63
            && c.forgets >= 310
            && c.chunks >= 25 + 310),
        "the walk delivered {} recentre(s), {} forget(s) and {} column(s)",
        client.centres,
        client.forgets,
        client.chunks
    );

    // 1000 blocks east from x = 0.5 crosses into column 63, so sixty-two
    // boundaries after the first. Each one is five columns each way.
    assert_eq!(client.centres, 63, "one recentre per boundary crossed");
    assert_eq!(
        client.forgets, 310,
        "five columns forgotten per crossing, and never one the client did \
         not hold"
    );
    assert_eq!(
        client.chunks,
        25 + 310,
        "the square at spawn plus five columns per crossing — a resend would \
         push this above it and nothing else would show"
    );

    running.finish();
}

/// Two players, one world: what one breaks, the other is told about.
///
/// This is the first test in the project where two connections share
/// anything, and it is the property that makes the thing a *server* rather
/// than a generator with a socket on it. The second client is watching a
/// column it did not edit, so the only way the change reaches it is the
/// broadcast — a per-connection world would pass every other test here and
/// fail this one.
#[test]
fn a_block_one_player_breaks_is_announced_to_another() {
    let running = start("");
    let addr = running.addr;

    let mut watcher_stream = connect(addr);
    let mut watcher = join_as(&mut watcher_stream, addr, "Watcher");
    let mut breaker_stream = connect(addr);
    let _breaker = join_as(&mut breaker_stream, addr, "Breaker");

    // The surface block at the spawn column. Encoded the way the protocol
    // packs a position — 26 bits of x, 26 of z, 12 of y, in that order — by
    // hand, because a helper shared with the server would agree with it about
    // a layout neither of them checked.
    let (x, y, z) = within_reach_of_spawn(3, -60, 5);
    let packed = ((x & 0x3ff_ffff) << 38) | ((z & 0x3ff_ffff) << 12) | (y & 0xfff);

    let mut body = packed.to_be_bytes().to_vec();
    body.insert(0, 0); // status: start digging, which is what creative sends
    body.push(1); // face
    write_var_int(1, &mut body); // sequence
    send_compressed_frame(&mut breaker_stream, 36, &body);

    // The watcher must be told, and told the right block at the right place.
    let update = watcher
        .wait_for(&mut watcher_stream, 9)
        .expect("the watcher is told about the break");
    let position = i64::from_be_bytes(update[..8].try_into().expect("eight bytes"));
    assert_eq!(position, packed, "the same block, not a neighbour");
    let (state, _) = read_var_int_from(&update[8..]);
    assert_eq!(state, 0, "broken to air");

    running.finish();
}

/// The half of Phase 3's exit criterion that needs a restart: break a block,
/// walk away, stop the server, start it again, and find both the hole and
/// yourself where you left them.
///
/// Run across two whole server lifetimes rather than by calling the save code
/// directly, because what is being checked is that the write happens at the
/// right moment in the teardown and the read at the right moment in the boot.
/// A test that called `store` and `load` would pass with neither wired up.
#[test]
fn a_broken_block_and_a_walked_to_position_both_survive_a_restart() {
    let world_dir = std::env::temp_dir().join(format!(
        "dust-restart-{}-{}",
        std::process::id(),
        ICON_SEQ.fetch_add(1, Ordering::SeqCst)
    ));

    let (x, y, z) = within_reach_of_spawn(3, -60, 5);
    let packed = ((x & 0x3ff_ffff) << 38) | ((z & 0x3ff_ffff) << 12) | (y & 0xfff);
    // Far enough to be a different column, so the position is not
    // accidentally right by being the spawn one — and reached in two steps of
    // eight blocks rather than one of sixteen, because sixteen blocks in one
    // packet is not something a client can do and `dust_guard::Movement` now
    // says so. The two steps go out back to back with nothing between them,
    // which is also the bunched-up-after-a-stall case: if the movement budget
    // were charged by the clock rather than floored at a tick, the second of
    // these would be refused for arriving too soon after the first.
    let walked_to = 16.5f64;

    {
        let running = start_in(&world_dir, "");
        let addr = running.addr;
        let mut stream = connect(addr);
        let mut client = join_as(&mut stream, addr, "Digger");

        let mut body = packed.to_be_bytes().to_vec();
        body.insert(0, 0); // start digging, which is what creative sends
        body.push(1);
        write_var_int(1, &mut body);
        send_compressed_frame(&mut stream, 36, &body);
        client
            .wait_for(&mut stream, 5)
            .expect("the dig is acknowledged");

        for step in [8.5f64, walked_to] {
            let mut walk = Vec::new();
            walk.extend_from_slice(&step.to_be_bytes());
            walk.extend_from_slice(&(-59.0f64).to_be_bytes());
            walk.extend_from_slice(&0.5f64.to_be_bytes());
            walk.push(1);
            send_compressed_frame(&mut stream, 26, &walk);
        }
        // Wait for the packet the move *causes*, not for the socket to go
        // quiet. Silence proves only that nothing has arrived yet, and on a
        // slow runner "yet" includes "before the server got to it" — this
        // failed in CI and passed here for exactly that reason. Only the
        // *second* step crosses a column boundary, so a recentre is the server
        // saying it processed both.
        client
            .wait_for(&mut stream, 84)
            .expect("the move is processed");

        // Ending the connection before stopping the server, so the position is
        // recorded by the session rather than by a race with the shutdown.
        drop(stream);
        let report = running.finish();
        assert!(
            report
                .transcript
                .iter()
                .any(|e| e.detail.contains("saved 1 block change(s)")),
            "the teardown must say what it wrote: {:?}",
            report.transcript
        );
    }

    // A second server, same directory, nothing else in common.
    {
        let running = start_in(&world_dir, "");
        let addr = running.addr;
        let mut stream = connect(addr);
        let mut client = join_as(&mut stream, addr, "Digger");

        // The teleport on join is where the player left off, not spawn.
        let (back_x, _, _) = client.spawned_at.expect("a position on join");
        assert_eq!(back_x, walked_to, "the player is put back where they were");

        // And the hole is still there. Asked by breaking it again: an already
        // broken block re-broken is still air, so what this really pins is
        // that the *chunk* arrived with the edit in it — checked below by the
        // column count, since an edited column is built rather than templated
        // and both paths have to produce a chunk.
        assert!(
            client.drain_until(&mut stream, |c| c.chunks >= 25),
            "the world arrived: {} of the twenty-five columns",
            client.chunks
        );

        drop(stream);
        running.finish();
    }

    // The save itself, read as an operator would: it is a file, it is JSON,
    // and it names the block rather than a number that means nothing next
    // version.
    let saved = std::fs::read_to_string(world_dir.join("dust-edits.json")).expect("a save file");
    assert!(saved.contains("minecraft:air"), "{saved}");
    assert!(saved.contains("\"y\": -60"), "{saved}");

    let _ = std::fs::remove_dir_all(&world_dir);
}

/// Two players in one world can see each other.
///
/// The whole point of a server, and the thing every other test here would pass
/// without. A player has to arrive as *both* halves — a tab-list entry and an
/// entity — because a client shown one without the other renders either a name
/// with no body or a body with no name, and neither looks like a bug in the
/// half that is missing.
#[test]
fn two_players_see_each_other_arrive_move_and_leave() {
    let running = start("");
    let addr = running.addr;

    let mut first_stream = connect(addr);
    let mut first = join_as(&mut first_stream, addr, "First");
    // Nobody else is here yet, so the first player is told about nobody. This
    // one *is* a drain, because the claim is that nothing arrives — and there
    // is no packet to wait for when the expected answer is silence.
    first.drain(&mut first_stream);
    assert_eq!(first.spawned_entities, 0, "an empty server has no bodies");

    let mut second_stream = connect(addr);
    let mut second = join_as(&mut second_stream, addr, "Second");
    // The roster goes out *after* the loading-screen event, deliberately — an
    // entity announced before the client holds the column it stands in is one
    // the client files against nothing — so `join_as` has already returned by
    // the time it arrives. Waited for rather than drained: a drain stops at
    // the first quiet moment, which on a slow machine can be before the server
    // has said anything at all.
    second
        .wait_for(&mut second_stream, 1)
        .expect("the second player is told about the first");

    // The second player is told about the first, on arrival, from the roster
    // snapshot rather than from the broadcast.
    assert_eq!(second.player_infos, 1, "the first player's tab-list row");
    assert_eq!(second.spawned_entities, 1, "and the first player's body");

    // And the first is told about the second, through the broadcast.
    first
        .wait_for(&mut first_stream, 1)
        .expect("the first player is told the second arrived");
    assert_eq!(
        first.player_infos, 1,
        "the tab-list row came with the body, not instead of it"
    );

    // The second walks; the first is told where to. Seven blocks and a bit,
    // which is a distance a client can cover — sixty-four, which this used to
    // be, is one `dust_guard::Movement` refuses, and the packet the first
    // player would then be told about is the correction rather than the walk.
    let mut walk = Vec::new();
    walk.extend_from_slice(&6.5f64.to_be_bytes());
    walk.extend_from_slice(&(-59.0f64).to_be_bytes());
    walk.extend_from_slice(&4.5f64.to_be_bytes());
    walk.push(1);
    send_compressed_frame(&mut second_stream, 26, &walk);

    let teleport = first
        .wait_for(&mut first_stream, 112)
        .expect("the first player is told the second moved");
    let (_, rest) = read_var_int_from(&teleport);
    let x = f64::from_be_bytes(rest[0..8].try_into().expect("eight bytes"));
    assert_eq!(x, 6.5, "to where they actually went");

    // The second leaves; the first is told to forget them, both halves.
    drop(second_stream);
    first
        .wait_for(&mut first_stream, 66)
        .expect("the body is removed");
    first
        .wait_for(&mut first_stream, 61)
        .expect("and so is the tab-list row");

    drop(first_stream);
    running.finish();
}

/// What one player types, the other reads — and both are told who came and
/// went.
#[test]
fn chat_and_the_join_and_leave_lines_reach_everybody() {
    let running = start("");
    let addr = running.addr;

    let mut first_stream = connect(addr);
    let mut first = join_as(&mut first_stream, addr, "First");
    // Its own join announcement, which is the last thing a lone player is
    // sent — waited for rather than drained, so the assertions below start
    // from a known point.
    first
        .wait_for(&mut first_stream, 108)
        .expect("the first player's own join line");

    let mut second_stream = connect(addr);
    let mut second = join_as(&mut second_stream, addr, "Second");
    second
        .wait_for(&mut second_stream, 108)
        .expect("the second player's own join line");

    // The first player is told the second arrived.
    let line = first
        .wait_for(&mut first_stream, 108)
        .expect("a join announcement");
    let text = String::from_utf8_lossy(&line);
    assert!(text.contains("Second"), "{text:?}");
    assert!(text.contains("joined the game"), "{text:?}");

    // The second says something.
    let message = "hello from over here";
    let mut body = Vec::new();
    write_string(message, &mut body);
    body.extend_from_slice(&0i64.to_be_bytes()); // timestamp
    body.extend_from_slice(&0i64.to_be_bytes()); // salt
    body.push(0); // no signature
    write_var_int(0, &mut body); // acknowledgement offset
    body.extend_from_slice(&[0u8; 3]); // the fixed acknowledgement bitset
    send_compressed_frame(&mut second_stream, 6, &body);

    // And the first reads it, with the sender's name in it.
    let line = first
        .wait_for(&mut first_stream, 108)
        .expect("the message arrives");
    let text = String::from_utf8_lossy(&line);
    assert!(text.contains(message), "{text:?}");
    assert!(text.contains("Second"), "{text:?}");

    // A speaker sees their own words too, which is why the roster does not
    // filter them: a session adding them back locally would be a second code
    // path for one line.
    let own = second
        .wait_for(&mut second_stream, 108)
        .expect("the speaker sees their own message");
    assert!(String::from_utf8_lossy(&own).contains(message));

    // And leaving is announced.
    drop(second_stream);
    let line = first
        .wait_for(&mut first_stream, 108)
        .expect("a leave announcement");
    let text = String::from_utf8_lossy(&line);
    assert!(text.contains("left the game"), "{text:?}");

    drop(first_stream);
    running.finish();
}

/// Decision 0005's standing guard, as the architecture states it: **turning
/// the JVM off must leave a fully working server, minus plugins.**
///
/// A test already checked that `jvm.enabled = false` keeps the placeholder out
/// of the participant list. That was all a server with no players could check.
/// This is the guard the decision record actually asks for — a player joins,
/// receives a world, changes it, is told about the change, and talks — with no
/// JVM in the process at all.
///
/// If game logic ever leaks across the Java boundary, this is what goes red,
/// and it goes red on the feature that leaked rather than on a count.
#[test]
fn everything_still_works_with_the_jvm_switched_off() {
    let running = start("[jvm]\nenabled = false\n");
    let addr = running.addr;

    let mut stream = connect(addr);
    let mut client = join_as(&mut stream, addr, "NoJvm");
    assert!(client.chunks >= 25, "the world arrived");
    assert!(
        client.spawned_at.is_some(),
        "and the player was put somewhere in it"
    );

    // Breaking a block, which is world state changing.
    let (x, y, z) = within_reach_of_spawn(2, -60, 2);
    let packed = ((x & 0x3ff_ffff) << 38) | ((z & 0x3ff_ffff) << 12) | (y & 0xfff);
    let mut body = packed.to_be_bytes().to_vec();
    body.insert(0, 0);
    body.push(1);
    write_var_int(1, &mut body);
    send_compressed_frame(&mut stream, 36, &body);
    client
        .wait_for(&mut stream, 5)
        .expect("the dig is acknowledged with no JVM in the process");

    // And chat, which is the server speaking.
    let mut said = Vec::new();
    write_string("still here", &mut said);
    said.extend_from_slice(&0i64.to_be_bytes());
    said.extend_from_slice(&0i64.to_be_bytes());
    said.push(0);
    write_var_int(0, &mut said);
    said.extend_from_slice(&[0u8; 3]);
    send_compressed_frame(&mut stream, 6, &said);
    let line = client
        .wait_for(&mut stream, 108)
        .expect("chat still travels");
    assert!(String::from_utf8_lossy(&line).contains("still here"));

    let report = running.finish();
    assert!(
        !report.participants.contains(&"jvm-placeholder".to_owned()),
        "the JVM really was off: {:?}",
        report.participants
    );
}

/// Phase 3's exit criterion, in one test, as the build plan words it: connect,
/// walk a thousand blocks across streaming chunks, chat, disconnect, and
/// reconnect to the same position.
///
/// The pieces are each checked on their own elsewhere. This is the one that
/// says the milestone is met, so it does the whole thing in order and against
/// two server lifetimes — because "reconnect to the same position" is only
/// worth anything if the server was stopped in between.
///
/// What the plan asks for and this does not do: run for ten minutes, and run
/// as a headless bot client rather than a hand-written one. Both are the
/// difference between this and the standing suite the plan wants from Phase 3
/// onward, and neither is a thing to claim by leaving it unsaid.
#[test]
fn phase_three_walk_chat_disconnect_and_come_back() {
    let world_dir = std::env::temp_dir().join(format!(
        "dust-phase3-{}-{}",
        std::process::id(),
        ICON_SEQ.fetch_add(1, Ordering::SeqCst)
    ));

    let mut x = 0.5f64;
    {
        let running = start_in(&world_dir, "");
        let addr = running.addr;
        let mut stream = connect(addr);
        let mut client = join_as(&mut stream, addr, "Walker");

        // A thousand blocks east, one at a time, reading as we go.
        for step in 0..1000 {
            x += 1.0;
            let mut walk = Vec::new();
            walk.extend_from_slice(&x.to_be_bytes());
            walk.extend_from_slice(&(-59.0f64).to_be_bytes());
            walk.extend_from_slice(&0.5f64.to_be_bytes());
            walk.push(1);
            send_compressed_frame(&mut stream, 26, &walk);
            if step % 16 == 0 {
                client.drain(&mut stream);
            }
        }
        // Drained to a count rather than waiting for one more recentre: the
        // drains inside the loop above have already consumed most of them, so
        // there may be none left to wait for.
        assert!(
            client.drain_until(&mut stream, |c| c.centres >= 63),
            "the walk produced {} recentres, not the sixty-two boundaries plus \
             the join",
            client.centres
        );
        assert_eq!(client.centres, 63, "and no more than that");

        // Chat.
        let mut said = Vec::new();
        write_string("made it", &mut said);
        said.extend_from_slice(&0i64.to_be_bytes());
        said.extend_from_slice(&0i64.to_be_bytes());
        said.push(0);
        write_var_int(0, &mut said);
        said.extend_from_slice(&[0u8; 3]);
        send_compressed_frame(&mut stream, 6, &said);
        let line = client.wait_for(&mut stream, 108).expect("the message");
        assert!(String::from_utf8_lossy(&line).contains("made it"));

        // Disconnect.
        drop(stream);
        running.finish();
    }

    {
        let running = start_in(&world_dir, "");
        let addr = running.addr;
        let mut stream = connect(addr);
        let client = join_as(&mut stream, addr, "Walker");
        let (back_x, _, back_z) = client.spawned_at.expect("a position on rejoining");
        assert_eq!(back_x, x, "a thousand blocks east, where they stopped");
        assert_eq!(back_z, 0.5);
        assert!(client.chunks >= 25, "with the world around them");
        drop(stream);
        running.finish();
    }

    let _ = std::fs::remove_dir_all(&world_dir);
}

/// Phase 2's exit criterion, first half: a client is served a world Minecraft
/// generated, not the flat one.
///
/// `#[ignore]`, and it says so when it is skipped rather than passing
/// vacuously — a generated world is Mojang's content and nothing of theirs is
/// committed, so this needs `DUST_ANVIL_WORLD` pointing at a region directory.
/// See `crates/dust-world/tests/anvil.rs` for how to make one.
///
/// What it checks is that the terrain is *not flat*, which is the only claim
/// worth making from this end: a reader that returned the fallback for every
/// column would pass every structural check and fail this.
#[test]
#[ignore = "needs a real world; set DUST_ANVIL_WORLD to a region directory"]
fn a_world_minecraft_generated_is_served_to_a_client() {
    let Some(region) = std::env::var_os("DUST_ANVIL_WORLD") else {
        panic!("DUST_ANVIL_WORLD is not set; see this test's own documentation");
    };
    let region = region.to_str().expect("a UTF-8 path");

    let running = start(&format!("world_source = {region:?}\n"));
    let addr = running.addr;
    let mut stream = connect(addr);
    let client = join_as(&mut stream, addr, "Explorer");
    assert_eq!(client.chunks, 25, "the columns arrived");

    // And the player is standing on the world's surface rather than at a
    // superflat's. This was `SURFACE_Y + 1` — bedrock level — for every world
    // Dust served, which put a player underground in the dark on a server that
    // looked broken. Asserted as a range and not a number: the y depends on
    // the world somebody pointed this at, and what is being claimed is that it
    // came from the terrain and not from a constant.
    let (_, y, _) = client.spawned_at.expect("the join teleport carries one");
    assert!(
        y > -40.0,
        "spawned at y = {y}, which is a superflat's surface and not a real \
         world's"
    );

    // And it is standing where `level.dat` says, not at the origin. Read
    // through the server's own reader rather than a second parser here: what
    // is being checked is that the number reached the teleport, and a test
    // that re-derived it from the file would be checking its own arithmetic.
    let (x, _, z) = client.spawned_at.expect("the join teleport carries one");
    match dust_server::net::level::spawn_beside(std::path::Path::new(region)) {
        Ok(Some(point)) => {
            assert_eq!(
                (x, z),
                (f64::from(point.x) + 0.5, f64::from(point.z) + 0.5),
                "the world spawns at x {}, z {} and the player was put at \
                 x {x}, z {z}",
                point.x,
                point.z
            );
        }
        Ok(None) => assert_eq!(
            (x, z),
            (0.5, 0.5),
            "no level.dat beside this world, so the origin is the answer"
        ),
        Err(why) => panic!("{why}"),
    }

    // A flat column has one section with anything in it and twenty-three of
    // air. A generated one has terrain up through the surface, so several
    // sections carry a palette of more than one block.
    let mut interesting = 0usize;
    let mut stream2 = connect(addr);
    let mut second = join_as(&mut stream2, addr, "Reader");
    assert!(
        second.drain_until(&mut stream2, |c| c.chunks >= 25),
        "the second client was sent {} of the twenty-five columns",
        second.chunks
    );
    for body in &second.chunk_bodies {
        interesting += mixed_sections(body);
    }
    assert!(
        interesting > 25,
        "across twenty-five columns only {interesting} section(s) held more \
         than one kind of block; that is the flat fallback, not a generated \
         world"
    );

    drop(stream);
    drop(stream2);
    running.finish();
}

/// How many of a chunk packet's sections hold more than one kind of block.
///
/// Walks the section blob by hand, as the rest of this file does: a
/// bits-per-entry of zero is a section of one value, and anything else is a
/// section with terrain in it.
fn mixed_sections(body: &[u8]) -> usize {
    // Skip the coordinates and the heightmap NBT, then take the blob's length.
    let mut p = 8;
    p = skip_nbt(body, p);
    let (size, rest) = read_var_int_from(&body[p..]);
    let blob = &rest[..size as usize];

    let mut q = 0usize;
    let mut mixed = 0usize;
    while q + 3 <= blob.len() {
        q += 2; // the non-air count
        for limit in [8u8, 3u8] {
            let bpe = blob[q];
            q += 1;
            if bpe == 0 {
                let (_value, after) = read_var_int_from(&blob[q..]);
                q = blob.len() - after.len();
                let (longs, after) = read_var_int_from(&blob[q..]);
                q = blob.len() - after.len() + longs as usize * 8;
            } else {
                if bpe <= limit {
                    if limit == 8 {
                        mixed += 1;
                    }
                    let (count, after) = read_var_int_from(&blob[q..]);
                    q = blob.len() - after.len();
                    for _ in 0..count {
                        let (_entry, after) = read_var_int_from(&blob[q..]);
                        q = blob.len() - after.len();
                    }
                } else if limit == 8 {
                    mixed += 1;
                }
                let (longs, after) = read_var_int_from(&blob[q..]);
                q = blob.len() - after.len() + longs as usize * 8;
            }
        }
    }
    mixed
}

/// Step past one NBT document, returning where it ends.
///
/// Enough of a walker to skip the heightmap compound and no more. Written here
/// rather than borrowed from `dust-nbt` for the reason the rest of this file
/// hand-rolls its wire handling: a test that used the server's own reader
/// would agree with the server about a layout neither of them checked.
fn skip_nbt(bytes: &[u8], at: usize) -> usize {
    fn payload(bytes: &[u8], mut at: usize, tag: u8) -> usize {
        match tag {
            1 => at + 1,
            2 => at + 2,
            3 | 5 => at + 4,
            4 | 6 => at + 8,
            7 => {
                let n = i32::from_be_bytes(bytes[at..at + 4].try_into().expect("four")) as usize;
                at + 4 + n
            }
            8 => {
                let n = u16::from_be_bytes(bytes[at..at + 2].try_into().expect("two")) as usize;
                at + 2 + n
            }
            9 => {
                let inner = bytes[at];
                at += 1;
                let n = i32::from_be_bytes(bytes[at..at + 4].try_into().expect("four")) as usize;
                at += 4;
                for _ in 0..n {
                    at = payload(bytes, at, inner);
                }
                at
            }
            10 => loop {
                let inner = bytes[at];
                at += 1;
                if inner == 0 {
                    return at;
                }
                let n = u16::from_be_bytes(bytes[at..at + 2].try_into().expect("two")) as usize;
                at += 2 + n;
                at = payload(bytes, at, inner);
            },
            11 => {
                let n = i32::from_be_bytes(bytes[at..at + 4].try_into().expect("four")) as usize;
                at + 4 + 4 * n
            }
            12 => {
                let n = i32::from_be_bytes(bytes[at..at + 4].try_into().expect("four")) as usize;
                at + 4 + 8 * n
            }
            other => panic!("tag {other} is not one this walker knows"),
        }
    }
    let tag = bytes[at];
    payload(bytes, at + 1, tag)
}

#[test]
fn a_connection_that_says_nothing_costs_nothing() {
    let running = start("");
    let stream = connect(running.addr);
    drop(stream);
    running.finish();
}

#[test]
fn a_configured_favicon_reaches_the_wire_and_a_bad_one_stops_the_boot() {
    // The unit tests prove the picture is validated and that the document
    // carries it. Neither proves the boot phase actually reads the setting —
    // a start_network that never looked at `favicon` would pass both, and an
    // operator would get exactly what they get from setting nothing at all.
    let png = tiny_png(64, 64);
    let icon_path = std::env::temp_dir().join(format!(
        "dust-ping-icon-{}-{}.png",
        std::process::id(),
        ICON_SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&icon_path, &png).expect("write the icon");

    let running = start(&format!(
        "favicon = {:?}\n",
        icon_path.to_str().expect("a UTF-8 temp path")
    ));
    let addr = running.addr;
    let mut stream = connect(addr);
    handshake(&mut stream, 767, addr, 1);
    send_frame(&mut stream, 0x00, &[]);
    let json = read_status_json(&mut stream);
    assert!(
        json.contains(r#""favicon":"data:image/png;base64,"#),
        "the configured icon must reach the wire: {json}"
    );
    running.finish();

    // And the refusal half: a picture the client would silently ignore stops
    // the boot instead, because "shows nothing" and "was never set" look the
    // same to the only person who could fix it.
    let wrong = std::env::temp_dir().join(format!(
        "dust-ping-icon-{}-{}.png",
        std::process::id(),
        ICON_SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&wrong, tiny_png(128, 128)).expect("write the icon");
    let err = boot_expecting_failure(&format!(
        "favicon = {:?}\n",
        wrong.to_str().expect("a UTF-8 temp path")
    ));
    let message = err.to_string();
    assert!(message.contains("128x128"), "{message}");
    assert!(message.contains("64x64"), "{message}");

    let _ = std::fs::remove_file(&icon_path);
    let _ = std::fs::remove_file(&wrong);
}

static ICON_SEQ: AtomicU64 = AtomicU64::new(0);

/// A PNG header claiming a size, with no image data. Nothing in the server
/// decodes pixels, so nothing here needs any — and a test that shipped a real
/// picture would be testing a decoder that does not exist.
fn tiny_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes
}

/// Run a boot that is expected to fail in phase 3, and return the error.
fn boot_expecting_failure(extra_config: &str) -> dust_server::ServerError {
    let clock = Arc::new(ManualClock::new());
    let config = format!("[server]\nbind = \"127.0.0.1:0\"\nonline_mode = false\n{extra_config}");
    let options = ServerOptions {
        config_path: write_config(&config),
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        loop_parker: stepping(Arc::clone(&clock), TICK_NS),
        watchdog: WatchdogSetting::Disabled,
        ..ServerOptions::default()
    };
    Server::new(options)
        .run()
        .expect_err("a picture the client cannot use must stop the boot")
}

/// Block properties survive being read out of a world file.
///
/// The reader hands `(name, value)` pairs to the registry and the registry
/// walks them onto a state. Without that, every stair in a loaded world faces
/// north and every log lies on its side the same way — which renders as a world
/// that is subtly, uniformly wrong rather than as an error.
///
/// Checked against the registry directly rather than over the wire, because
/// what is being tested is the resolution and not the packet. `#[ignore]` only
/// because it is grouped with the world tests; it needs no world.
#[test]
fn a_block_state_is_resolved_from_its_properties_and_not_just_its_name() {
    use dust_world::anvil::Names;

    let names = dust_server::net::source::RegistryNames::new().expect("the biome registry");

    // A block with no properties resolves to itself.
    let stone = dust_registry::Block::from_name("minecraft:stone").expect("stone");
    assert_eq!(
        names.block("minecraft:stone", &[]),
        Some(stone.default_state().id())
    );

    // One with properties resolves to the state those properties name, and
    // *not* to the default — which is the whole point, so both are asserted.
    let stairs = dust_registry::Block::from_name("minecraft:oak_stairs").expect("oak stairs");
    let default = stairs.default_state();
    let facing_south = names
        .block("minecraft:oak_stairs", &[("facing", "south")])
        .expect("a state");
    assert_ne!(
        facing_south,
        default.id(),
        "applying a property must move off the default, or nothing was applied"
    );
    assert_eq!(
        dust_registry::BlockState::from_id(facing_south)
            .expect("a real state")
            .property("facing"),
        Some("south")
    );

    // Several at once, since the state id is mixed-radix over all of them and
    // applying two is where an implementation that only handled one shows.
    let both = names
        .block(
            "minecraft:oak_stairs",
            &[("facing", "west"), ("half", "top")],
        )
        .expect("a state");
    let both = dust_registry::BlockState::from_id(both).expect("a real state");
    assert_eq!(both.property("facing"), Some("west"));
    assert_eq!(both.property("half"), Some("top"));

    // A property this build does not model is skipped, not fatal: a world from
    // a newer Minecraft or a modded server carries fields this table has never
    // heard of, and refusing a chunk over one would make the world unopenable
    // for a detail nobody can see. The properties that *are* understood still
    // apply.
    let with_nonsense = names
        .block(
            "minecraft:oak_stairs",
            &[("facing", "east"), ("not_a_property", "yes")],
        )
        .expect("still a state");
    assert_eq!(
        dust_registry::BlockState::from_id(with_nonsense)
            .expect("a real state")
            .property("facing"),
        Some("east"),
        "the understood property survives the unknown one"
    );

    // And a block nobody has heard of is a `None`, which the parser turns into
    // a named error rather than a default.
    assert_eq!(names.block("minecraft:not_a_block", &[]), None);
}

/// A player who swings and crouches is one the others can see doing it.
///
/// Both were decoded and dropped, which meant another player mined, placed and
/// sneaked with their arms down and standing to full height. Neither changes a
/// block or a position, which is exactly why they are worth a test: nothing
/// else in the suite would notice them go missing.
#[test]
fn a_swing_and_a_crouch_reach_the_other_player() {
    let running = start("");
    let addr = running.addr;

    let mut first_stream = connect(addr);
    let mut first = join_as(&mut first_stream, addr, "First");
    first.drain(&mut first_stream);

    let mut second_stream = connect(addr);
    let _second = join_as(&mut second_stream, addr, "Second");
    first
        .wait_for(&mut first_stream, 1)
        .expect("the first player is told the second arrived");

    // Swing, main hand. Serverbound `swing` is id 54 in play on 1.21.1 and its
    // whole body is the hand as a VarInt.
    send_compressed_frame(&mut second_stream, 54, &[0]);
    // `animate` is clientbound id 3 in play on 1.21.1, from the generated
    // table. Written out rather than looked up: a test that asked the server's
    // own table which id to expect would agree with it whatever it said.
    let animate = first
        .wait_for(&mut first_stream, 3)
        .expect("the first player sees the swing");
    let (entity, rest) = read_var_int_from(&animate);
    assert!(entity > 0, "somebody's entity id");
    assert_eq!(rest, [0], "animation 0 is the main-hand swing");

    // Off hand, which is animation 3 and not 1 — 1 is taking damage, and a
    // server that relayed the hand number would show a hurt player.
    send_compressed_frame(&mut second_stream, 54, &[1]);
    let animate = first
        .wait_for(&mut first_stream, 3)
        .expect("the first player sees the off-hand swing");
    let (_, rest) = read_var_int_from(&animate);
    assert_eq!(rest, [3], "animation 3 is the off-hand swing");

    // Start sneaking. Serverbound `player_command` is id 37: entity id, then
    // the action, then the jump boost.
    let mut sneak = Vec::new();
    write_var_int_arg(&mut sneak, entity);
    write_var_int_arg(&mut sneak, 0); // StartSneaking
    write_var_int_arg(&mut sneak, 0); // no jump boost
    send_compressed_frame(&mut second_stream, 37, &sneak);

    let metadata = first
        .wait_for(&mut first_stream, 88)
        .expect("the first player is told the second is crouching");
    let (subject, rest) = read_var_int_from(&metadata);
    assert_eq!(subject, entity, "about the player who crouched");
    // Two slots and a terminator: the shared flag byte, then the pose. Both
    // are needed — the flag alone dims a name tag on a player standing at full
    // height, and the pose alone crouches somebody who is not sneaking.
    assert_eq!(rest[0], 0, "slot 0, the shared entity flags");
    assert_eq!(rest[1], 0, "serializer 0, a byte");
    assert_eq!(rest[2] & 0x02, 0x02, "bit 1 is crouching");
    assert_eq!(rest[3], 6, "slot 6, the pose");
    assert_eq!(rest[4], 21, "serializer 21, a pose");
    assert_eq!(rest[5], 5, "pose 5 is sneaking");
    assert_eq!(rest[6], 0xFF, "and the metadata terminator");

    // Stopping sneaking says so rather than saying nothing.
    let mut stand = Vec::new();
    write_var_int_arg(&mut stand, entity);
    write_var_int_arg(&mut stand, 1); // StopSneaking
    write_var_int_arg(&mut stand, 0);
    send_compressed_frame(&mut second_stream, 37, &stand);
    let metadata = first
        .wait_for(&mut first_stream, 88)
        .expect("and told when they stand up again");
    let (_, rest) = read_var_int_from(&metadata);
    assert_eq!(rest[2] & 0x02, 0, "the crouch bit is off");
    assert_eq!(rest[5], 0, "pose 0 is standing");

    drop(second_stream);
    drop(first_stream);
    running.finish();
}

/// A block position, checked to be one a player standing at the spawn may
/// actually reach.
///
/// The server refuses a dig or a place beyond `[server] interaction_range`, and
/// a test that aimed further used to pass and now fails with "the watcher is
/// told the block changed" — which names the packet it waited for and says
/// nothing about why it never came. This asserts the real constraint at the
/// place the coordinates are written, so the failure names the cause.
///
/// The same arithmetic `dust_guard::Reach` makes, deliberately written out
/// again rather than called: a test that used the checker to decide what the
/// checker should allow would agree with it under any rule, including a wrong
/// one.
fn within_reach_of_spawn(x: i64, y: i64, z: i64) -> (i64, i64, i64) {
    let (ex, ey, ez) = (
        dust_server::net::world::SPAWN.0,
        dust_server::net::world::SPAWN.1 + 1.62,
        dust_server::net::world::SPAWN.2,
    );
    let gap = |eye: f64, low: i64| {
        let low = low as f64;
        (low - eye).max(eye - (low + 1.0)).max(0.0)
    };
    let (dx, dy, dz) = (gap(ex, x), gap(ey, y), gap(ez, z));
    let distance = (dx * dx + dy * dy + dz * dz).sqrt();
    let limit = dust_config::DustConfig::default().server.interaction_range;
    assert!(
        distance < limit,
        "({x}, {y}, {z}) is {distance:.2} blocks from the spawn and the server \
         refuses past {limit}; pick a block a player could actually touch"
    );
    (x, y, z)
}

/// `write_var_int` with the arguments the other way round, because every call
/// beside it reads `(buffer, value)` and one that reads `(value, buffer)` is a
/// bug waiting for somebody in a hurry.
fn write_var_int_arg(out: &mut Vec<u8>, value: i32) {
    write_var_int(value, out);
}

/// What one player breaks, the others watch break.
///
/// The block change already reached them; what did not was the *effect* — the
/// particles and the dig sound, which a client makes out of the state that was
/// there rather than out of the air left behind. A server that sent only the
/// change leaves everybody else watching blocks vanish in silence.
///
/// Captured from a real 1.21.1 server before it was written: two bots, one
/// digging, and the other is sent `world_event` with effect 2001, `data` the
/// broken block's state id, and `global` false. The digger is sent nothing —
/// its own client played the effect before the server heard about the dig.
#[test]
fn a_block_one_player_breaks_is_seen_breaking_by_another() {
    let running = start("");
    let addr = running.addr;

    let mut watcher_stream = connect(addr);
    let mut watcher = join_as(&mut watcher_stream, addr, "Watcher");
    let mut breaker_stream = connect(addr);
    let mut breaker = join_as(&mut breaker_stream, addr, "Breaker");

    // The surface block under the spawn, packed the way the protocol packs a
    // position, by hand for the same reason the test above does it by hand.
    //
    // It used to be (7, -60, 9) — eleven blocks away, which no player can
    // reach and which the server now refuses. Every coordinate here goes
    // through `within_reach_of_spawn` so that the next one to drift out is a
    // named failure rather than a packet that never arrives.
    let (x, y, z) = within_reach_of_spawn(0, -60, 0);
    let packed = ((x & 0x3ff_ffff) << 38) | ((z & 0x3ff_ffff) << 12) | (y & 0xfff);
    let mut body = packed.to_be_bytes().to_vec();
    body.insert(0, 0); // status: start digging
    body.push(1); // face
    write_var_int(1, &mut body); // sequence
    send_compressed_frame(&mut breaker_stream, 36, &body);

    // The change first, then the effect. That order is not cosmetic: a client
    // told a block broke before it knows the block changed has nothing to
    // break.
    let update = watcher
        .wait_for(&mut watcher_stream, 9)
        .expect("the watcher is told the block changed");
    assert_eq!(
        i64::from_be_bytes(update[..8].try_into().expect("eight bytes")),
        packed
    );

    let effect = watcher
        .wait_for(&mut watcher_stream, 40)
        .expect("and is shown it breaking");
    assert_eq!(
        i32::from_be_bytes(effect[0..4].try_into().expect("four bytes")),
        2001,
        "PARTICLES_DESTROY_BLOCK"
    );
    assert_eq!(
        i64::from_be_bytes(effect[4..12].try_into().expect("eight bytes")),
        packed,
        "at the block that broke"
    );
    let data = i32::from_be_bytes(effect[12..16].try_into().expect("four bytes"));
    assert!(
        data > 0,
        "the data is the *broken* block's state id, not the air left behind"
    );
    assert_eq!(effect[16], 0, "a local effect, not a global one");

    // A second dig at the same block says nothing at all. A creative client
    // sends `start_digging` and a mining one sends `finish_digging` too, so a
    // single dig arrives twice — and the second one has air to break. Setting
    // air twice was idempotent and invisible until the effect went out with
    // it, as a silent puff made of nothing.
    send_compressed_frame(&mut breaker_stream, 36, &body);
    watcher.drain_until_quiet(&mut watcher_stream);
    assert_eq!(
        watcher.level_events, 1,
        "the second dig broke air, which is not breaking anything"
    );

    // And the breaker is told the change and not the effect. Checked by
    // draining what actually arrived rather than by waiting for silence: a
    // wait for a packet that never comes takes the read timeout and then says
    // nothing useful, and this way the assertion names what did arrive.
    breaker.drain_until_quiet(&mut breaker_stream);
    assert_eq!(
        breaker.level_events, 0,
        "the digger's own client already played it"
    );

    drop(breaker_stream);
    drop(watcher_stream);
    running.finish();
}

/// Somebody already crouching is crouching to whoever arrives next.
///
/// A spawned player stands upright until something says otherwise, so a player
/// who started sneaking before this one connected would be upright to them and
/// crouching to everybody else — two clients rendering the same player
/// differently. It is the reason the roster keeps the posture rather than the
/// session that owns it.
#[test]
fn a_player_already_crouching_is_crouching_to_whoever_arrives_next() {
    let running = start("");
    let addr = running.addr;

    // An observer connected *first*, whose metadata packet is what proves the
    // server has acted on the crouch. Without it there is nothing to wait for
    // — the posture is not echoed to the player who struck it — and the test
    // would be sleeping and hoping, which is the habit this file's own notes
    // are about.
    let mut observer_stream = connect(addr);
    let mut observer = join_as(&mut observer_stream, addr, "Observer");

    let mut sneaker_stream = connect(addr);
    let _sneaker = join_as(&mut sneaker_stream, addr, "Sneaker");
    observer
        .wait_for(&mut observer_stream, 1)
        .expect("the observer is told the sneaker arrived");

    // Start sneaking. The entity id in the body is the client's own claim and
    // the server uses the session's, so any value does here.
    let mut sneak = Vec::new();
    write_var_int(1, &mut sneak);
    write_var_int(0, &mut sneak); // StartSneaking
    write_var_int(0, &mut sneak); // the jump boost, which is always present
    send_compressed_frame(&mut sneaker_stream, 37, &sneak);
    observer
        .wait_for(&mut observer_stream, 88)
        .expect("the observer sees the crouch, which is what says the server acted");

    // Only now does the third player arrive.
    let mut watcher_stream = connect(addr);
    let mut watcher = join_as(&mut watcher_stream, addr, "Watcher");
    assert!(
        watcher.drain_until(&mut watcher_stream, |w| w.spawned_entities >= 2
            && w.postures >= 1),
        "the watcher was told about {} player(s) and {} posture(s)",
        watcher.spawned_entities,
        watcher.postures
    );

    assert_eq!(watcher.spawned_entities, 2, "the observer and the sneaker");
    assert_eq!(
        watcher.postures, 1,
        "one posture, for the one player who is not standing upright"
    );

    drop(watcher_stream);
    drop(sneaker_stream);
    drop(observer_stream);
    running.finish();
}

/// A client that asks for less than the server offers is served less.
///
/// The view distance is a ceiling on the server and a *request* from the
/// client, and the smaller of the two wins. A client asking for two on a
/// server set to eight is spared 264 columns it would throw away; a client
/// asking for thirty-two on the same server still gets eight.
///
/// Asserted by counting columns rather than by reading a field: what the join
/// packet advertises and what the streaming actually sends are two answers to
/// one question, and only the second is the one a player sees.
#[test]
fn a_client_asking_for_a_shorter_view_is_given_the_shorter_one() {
    // The server offers four — 81 columns — and the client will ask for one,
    // which is nine.
    let running = start("view_distance = 4\n");
    let addr = running.addr;
    let mut stream = connect(addr);
    let joined = join_asking_for_view_distance(&mut stream, addr, "Nearsighted", 1);
    assert_eq!(
        joined.chunks, 9,
        "three by three, which is what the client asked for"
    );
    drop(stream);
    running.finish();
}

/// A client that asks for more than the server offers is held to the server's.
#[test]
fn a_client_asking_for_a_longer_view_is_held_to_the_server_s() {
    let running = start("view_distance = 2\n");
    let addr = running.addr;
    let mut stream = connect(addr);
    let joined = join_asking_for_view_distance(&mut stream, addr, "Farsighted", 32);
    assert_eq!(joined.chunks, 25, "five by five, which is the server's");
    drop(stream);
    running.finish();
}

/// A player who turns their render distance down mid-game is served less.
///
/// The setting a client sends during configuration is honoured at the join; it
/// may send the packet again at any point afterwards, and until the streaming
/// was paced there was nothing cheap to do about it. Now the view simply
/// forgets what fell outside the new square, on its next move, out of the same
/// difference it already computes.
#[test]
fn a_render_distance_lowered_mid_game_forgets_what_fell_outside_it() {
    let running = start("view_distance = 4\n");
    let addr = running.addr;
    let mut stream = connect(addr);
    let mut joined = join_asking_for_view_distance(&mut stream, addr, "Shrinking", 4);
    assert!(
        joined.drain_until(&mut stream, |j| j.chunks >= 81),
        "only {} of the eighty-one columns arrived",
        joined.chunks
    );
    assert_eq!(joined.chunks, 81, "nine by nine to begin with");
    let forgotten_before = joined.forgets;

    // Ask for one — three by three — with `client_information`, id 0x0a in
    // play on 1.21.1. Written by hand like every other packet here.
    let mut settings = Vec::new();
    write_string("en_gb", &mut settings);
    settings.push(1); // view distance
    write_var_int(0, &mut settings); // chat mode
    settings.push(1); // chat colours
    settings.push(0x7f); // skin parts
    write_var_int(1, &mut settings); // main hand
    settings.push(0); // text filtering
    settings.push(1); // server listings
    send_compressed_frame(&mut stream, 0x0a, &settings);

    // A move, so the view recomputes. One block is enough: the difference is
    // taken against the new radius, not against how far the player went.
    let mut walk = Vec::new();
    walk.extend_from_slice(&1.5f64.to_be_bytes());
    walk.extend_from_slice(&(-59.0f64).to_be_bytes());
    walk.extend_from_slice(&1.5f64.to_be_bytes());
    walk.push(1);
    send_compressed_frame(&mut stream, 26, &walk);

    assert!(
        joined.drain_until(&mut stream, |j| j.forgets - forgotten_before >= 72),
        "eighty-one columns down to nine is seventy-two forgotten; saw {}",
        joined.forgets - forgotten_before
    );

    drop(stream);
    running.finish();
}

/// The other half of the reach check: a client that says it is somewhere it
/// could not have walked to is put back, over a real socket.
///
/// The correction is a `player_position` and not a log line, which is the whole
/// point — a client honours one by moving. What makes this a test of the
/// *validator* rather than of the packet is where it puts the player: back to
/// the honest step sent immediately before the impossible one. That single
/// coordinate says the first move was believed and the second was not, and it
/// says it without ever waiting for silence, which in this file proves nothing.
#[test]
fn a_player_who_claims_to_be_across_the_map_is_put_back() {
    let running = start("");
    let addr = running.addr;
    let mut stream = connect(addr);
    let mut client = join_as(&mut stream, addr, "Runner");
    let (spawn_x, spawn_y, spawn_z) = client.spawned_at.expect("a position on join");

    let step = |stream: &mut TcpStream, x: f64| {
        let mut walk = Vec::new();
        walk.extend_from_slice(&x.to_be_bytes());
        walk.extend_from_slice(&spawn_y.to_be_bytes());
        walk.extend_from_slice(&spawn_z.to_be_bytes());
        walk.push(1);
        send_compressed_frame(stream, 26, &walk);
    };

    // Eight blocks in one packet: more than a walking player covers in a tick
    // and less than a falling one, so it is inside the limit and has to be
    // believed. Then five hundred more, which nothing can do.
    let honest = spawn_x + 8.0;
    step(&mut stream, honest);
    step(&mut stream, honest + 500.0);

    let correction = client
        .wait_for(&mut stream, 64)
        .expect("the server puts a player back who says they crossed the map");
    let x = f64::from_be_bytes(correction[0..8].try_into().expect("eight bytes"));
    assert_eq!(
        x, honest,
        "put back to the last position it believed, not to spawn and not to the claim"
    );

    // Yaw and pitch are marked relative and sent as zero, so the correction
    // moves the player and does not spin them: a corrected player is already
    // having their day interrupted.
    let flags = correction[32];
    assert_eq!(
        flags & 0x18,
        0x18,
        "the rotation is left alone: {flags:#04x}"
    );
    let (teleport_id, _) = read_var_int_from(&correction[33..]);
    assert_ne!(teleport_id, 1, "a correction takes an id of its own");

    // And the session starts believing this player again once they answer.
    // Two steps of eight blocks rather than one of sixteen, for the reason the
    // restart test gives; the second crosses a column and the recentre is the
    // server saying so.
    let mut confirm = Vec::new();
    write_var_int(teleport_id, &mut confirm);
    send_compressed_frame(&mut stream, 0x00, &confirm);
    step(&mut stream, 8.5);
    step(&mut stream, 16.5);
    assert!(
        client.wait_for(&mut stream, 84).is_some(),
        "a corrected player can still walk"
    );

    drop(stream);
    running.finish();
}
