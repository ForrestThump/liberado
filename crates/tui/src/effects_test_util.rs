//! Test-only helper for effects tests: a URL that fails to connect.

/// A local URL whose connection fails on every platform.
///
/// Windows setups exist where nothing answers low or just-released ports —
/// the SYN is dropped and the stack only gives up after a retransmission
/// window (~2s), so "refused instantly" is not a portable assumption.
/// Reserving an ephemeral port and releasing it guarantees no listener;
/// callers that dial this wait well past the retransmit window.
pub(crate) fn refused_daemon_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}
