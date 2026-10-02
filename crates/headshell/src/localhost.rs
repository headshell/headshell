//! Where the interface is served from (D-086): a loopback HTTP address
//! instead of the `tauri://` scheme.
//!
//! YouTube's official player (the IFrame embed) refuses to play in a page
//! that sends no `Referer` — "Error 153", measured on 2026-10-02 — and a page
//! loaded from a custom scheme never sends one: the Fetch standard keeps a
//! referrer only for `http(s)` pages. Served from `http://127.0.0.1:<port>/`,
//! the page is an ordinary web page to the player.
//!
//! The port is picked when the app starts, so two copies of the app or another
//! program on a fixed port never collide. Nothing the interface keeps lives
//! in the page's storage (its settings are in `ui.json`, the rest in the
//! core), so a new origin every launch loses nothing.

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// The only address the interface listens on: never the network.
pub const HOST: &str = "127.0.0.1";

/// The name the window opens it by. The same address — but YouTube's player
/// answered a page at `http://127.0.0.1:<port>/` with "Error 150" (this video
/// is unavailable) for every video tried, and played them all at
/// `http://localhost:<port>/` (measured 2026-10-02): it takes a bare address
/// for a referrer that hides who embeds it.
pub const PAGE_HOST: &str = "localhost";

/// How long the window waits for the server's first answer before it says so.
const LISTEN_TIMEOUT: Duration = Duration::from_secs(5);

/// A free port on the loopback address.
///
/// The system hands one out and it is let go at once; the server binds it a
/// moment later. Another program could take it in between — then
/// [`wait_until_listening`] still sees a listener, and the page shows the
/// other program's refusal: rare enough to say plainly rather than guard.
///
/// # Errors
/// If no port can be had at all, with its stage.
pub fn free_port() -> Result<u16, String> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .map_err(|err| format!("STEP: WINDOW_OPEN\n  no loopback port for the interface: {err}"))
}

/// The address the window opens.
///
/// # Errors
/// Never in practice; the address is built from a number.
pub fn page_url(port: u16) -> Result<tauri::Url, String> {
    format!("http://{PAGE_HOST}:{port}/")
        .parse()
        .map_err(|err| format!("STEP: WINDOW_OPEN\n  the interface address is not valid: {err}"))
}

/// Waits until the interface's server answers, so the window's first request
/// does not race the server's thread and land on a refused port.
///
/// # Errors
/// If nothing listens within [`LISTEN_TIMEOUT`].
pub fn wait_until_listening(port: u16) -> Result<(), String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let started = Instant::now();
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
        if started.elapsed() >= LISTEN_TIMEOUT {
            return Err(format!(
                "STEP: WINDOW_OPEN\n  the interface's server did not start on {addr} within {} s",
                LISTEN_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
