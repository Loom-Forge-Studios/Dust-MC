//! The socket: where a stranger's bytes enter this process.
//!
//! # The boundary this module draws
//!
//! Three crates meet here and none of them can meet anywhere else.
//! `dust-net` owns bytes, frames, ciphers and the legality of a state
//! transition; `dust-protocol` owns what a frame's contents mean in a given
//! version; `dust-config` owns what this operator asked for. Assembly is
//! `dust-server`'s job by the architecture's dependency rule, and this is the
//! assembly.
//!
//! # Binding happens in the boot phase; serving happens on the runtime
//!
//! The listener's socket is bound *synchronously*, inside the ordered boot, and
//! only then handed to the asynchronous runtime that accepts on it. That split
//! is the whole design of [`listen`], and it exists because the two failures
//! are completely different in kind. A port already in use is a *boot* failure:
//! the operator gets an error naming the setting, the phases that already
//! started are torn down in reverse, and the process exits non-zero. If the
//! bind happened inside a spawned task instead, the same mistake would produce
//! a server that started, logged something, ticked happily and accepted no
//! connections — the worst outcome available, and the same one the ore
//! resolver refuses for unknown ore names.
//!
//! # What the tick loop has to do with this, today: nothing
//!
//! A server-list ping touches no world state, no entity and no player. It is
//! answered entirely off the network runtime while the tick loop runs beside
//! it, and the two share nothing but an atomic player count. That is not a
//! shortcut — it is why status is the right first thing to serve, and it is
//! also why the seam that will carry Play traffic into the tick is not
//! invented here on speculation. When there is a player to move, there will be
//! a [`TickParticipant`](crate::participant::TickParticipant) to move them.

use std::time::Duration;

pub(crate) use dust_net::frame::Frame;
use dust_net::io::{Conn, ConnError};
pub(crate) use dust_protocol::wire::Writer;
use tokio::io::{AsyncRead, AsyncWrite};

pub mod chat;
pub mod collide;
pub mod commands;
pub mod configure;
pub mod daylight;
pub mod edits;
pub mod falling;
pub mod favicon;
pub mod furnaces;
pub mod generated;
pub mod inventory;
pub mod items;
pub mod level;
pub mod listen;
pub mod play;
pub mod players;
pub mod residency;
pub mod save;
pub mod session;
pub mod source;
pub mod status;
pub mod updates;
pub mod view;
pub mod world;

/// Turn a packet into a frame without writing its id into its own body.
///
/// The two crates keep the id in different places on purpose — `dust-net`'s
/// `Frame` holds it as a number because the framer has to read it to find the
/// body, and `dust-protocol` holds it as a lookup because the number depends on
/// the version. Writing it twice would give the framer two ids and no rule
/// about which one wins.
#[macro_export]
macro_rules! to_frame {
    ($packet:expr, $version:expr) => {{
        let packet = $packet;
        let mut body = $crate::net::Writer::default();
        packet.encode_body(&mut body, $version)?;
        $crate::net::Frame::new(packet.protocol_id($version)? as i32, body.into_bytes())
    }};
}

/// How long a graceful close may spend flushing before it is abandoned.
///
/// `Conn::close` is *willing to wait*, which is the right default and the wrong
/// one here: a peer that stops reading can hold a flush open for as long as it
/// likes, and every one of these connections belongs to a stranger. Five
/// seconds is far beyond any real client's need for a few hundred bytes on a
/// socket it is actively reading, and short enough that a peer cannot pin a
/// task by going quiet.
pub(crate) const CLOSE_LINGER: Duration = Duration::from_secs(5);

/// End a connection, flushing what is queued if the peer will take it.
///
/// Dropping a `Conn` **aborts** it — that is `dust-net`'s documented contract,
/// and it is the right default, because a caller that wanted the flush
/// guarantee had `close` for it. It also means that returning from this module
/// with a reply still in the outbound queue sends the peer nothing at all,
/// which is exactly the bug this function exists to make unrepresentable: every
/// path that sent something goes through here.
///
/// On timeout the close future is dropped, which drops the `Conn`, which sets
/// the abort flag. The fallback needs no code of its own.
pub(crate) async fn finish<W>(
    conn: Conn<W>,
    outcome: session::Served,
) -> Result<session::Served, session::SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match tokio::time::timeout(CLOSE_LINGER, conn.close()).await {
        // A peer that hung up mid-flush is the ordinary end of a status ping,
        // not a failure worth reporting: the client got what it asked for and
        // stopped listening. The outcome stands.
        Ok(Ok(())) | Err(_) => Ok(outcome),
        Ok(Err(ConnError::Closed)) => Ok(outcome),
        Ok(Err(e)) => Err(session::SessionError::Conn(e)),
    }
}

pub use edits::{Edit, EditedWorld, SharedWorld};
pub use favicon::{Favicon, FaviconError};
pub use listen::{Counters, Listener, ListenerHandle, NetStats};
pub use session::{Authority, PlaceableBlocks, Served, SessionContext, SessionError};
pub use status::StatusPolicy;
