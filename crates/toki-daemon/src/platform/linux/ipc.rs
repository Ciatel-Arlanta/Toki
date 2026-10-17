//! Unix socket server at `$XDG_RUNTIME_DIR/toki.sock`, per DESIGN.md §8.
//!
//! The socket stands in for Windows' named pipe: a connect succeeding is the liveness
//! answer, and what travels over it is `crate::ipc`'s business. The runtime directory is
//! private to the user, so nobody else can reach it.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use crate::platform::Raw;
use crate::resolver::ResolverMsg;
use crate::state::Shared;

static STOP: AtomicBool = AtomicBool::new(false);

/// How long one read or write may stall, the same bound the Windows pipe has. The GUI writes
/// its request the moment it connects, so a client quiet this long is broken, and the one
/// server thread moves on to whoever is waiting behind it rather than holding them for long
/// enough to read the daemon as hung.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Ask the server to shut down. Safe to call before it has started.
///
/// `accept` only returns when a client connects, so shutdown dials the socket itself to
/// unblock it — the same trick the Windows side plays on its pipe.
pub fn stop() {
    STOP.store(true, Ordering::Relaxed);
    let _ = UnixStream::connect(toki_store::ipc_socket_path());
}

/// Run the server until [`stop`] is called. Intended as the body of a dedicated thread.
pub fn run(tx: Sender<Raw>, resolve_tx: Sender<ResolverMsg>, shared: Arc<Shared>) {
    let Some(store) = crate::ipc::open_store(|| STOP.load(Ordering::Relaxed)) else {
        return;
    };
    let path = toki_store::ipc_socket_path();
    // A socket file outlives a crash. The single-instance lock is already held, so any file
    // here is stale rather than another daemon's.
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            warn!(%e, path = %path.display(), "cannot open the IPC socket");
            return;
        }
    };
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    for conn in listener.incoming() {
        if STOP.load(Ordering::Relaxed) {
            break;
        }
        let Ok(mut conn) = conn else {
            continue;
        };
        let _ = conn.set_read_timeout(Some(CLIENT_TIMEOUT));
        let _ = conn.set_write_timeout(Some(CLIENT_TIMEOUT));
        crate::ipc::serve(&mut conn, &tx, &resolve_tx, &shared, &store);
    }
    let _ = std::fs::remove_file(&path);
}
