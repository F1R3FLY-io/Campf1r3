//! Tests for the transport, and only the transport.
//!
//! The pacing contract, the vocabulary and the frames are tested in
//! `f1r3x-host`, without a socket. What is left here is what only a socket can
//! show: the handshake, the framing, and the lifecycle a client forces on a
//! session by going away and coming back.
//!
//! That split is the refactor's dividend. Four of these used to be timing
//! tests asserting that *no* round arrived within a window — slow, and flaky
//! under load. Their subject moved to `f1r3x-host`, where the driver is a
//! state machine and the same property is a loop over `poll`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;

/// A client just complete enough to drive the server.
struct Client {
    sock: TcpStream,
    /// Frames read while looking for a different one. The server pushes
    /// rounds, scenes and stats on its own schedule, so a pushed frame
    /// routinely arrives before the reply to the request that caused it. A
    /// client that discarded those would lose them; a real one dispatches on
    /// the tag for the same reason.
    pending: Vec<String>,
}

impl Client {
    fn connect(addr: std::net::SocketAddr, session: &str) -> Client {
        let mut sock = TcpStream::connect(addr).expect("connect");
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            sock,
            "GET /s/{session} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(sock.read(&mut b).unwrap(), 1, "handshake truncated");
            head.push(b[0]);
        }
        let head = String::from_utf8_lossy(&head).to_string();
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        // The value RFC 6455 §1.3 publishes for this key.
        assert!(head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{head}");
        Client {
            sock,
            pending: Vec::new(),
        }
    }

    fn send(&mut self, s: &str) {
        let payload = s.as_bytes();
        let mut f = vec![0x81];
        let mask = [0x37u8, 0xfa, 0x21, 0x3d];
        let n = payload.len();
        if n < 126 {
            f.push(0x80 | n as u8);
        } else {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        f.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            f.push(b ^ mask[i & 3]);
        }
        self.sock.write_all(&f).unwrap();
    }

    fn try_recv(&mut self) -> Option<String> {
        let mut h = [0u8; 2];
        if self.sock.read_exact(&mut h).is_err() {
            return None;
        }
        assert_eq!(h[0] & 0x0F, 0x1, "expected a text frame");
        assert_eq!(h[1] & 0x80, 0, "a server frame must not be masked");
        let mut len = (h[1] & 0x7F) as usize;
        if len == 126 {
            let mut e = [0u8; 2];
            self.sock.read_exact(&mut e).ok()?;
            len = u16::from_be_bytes(e) as usize;
        } else if len == 127 {
            let mut e = [0u8; 8];
            self.sock.read_exact(&mut e).ok()?;
            len = u64::from_be_bytes(e) as usize;
        }
        let mut p = vec![0u8; len];
        self.sock.read_exact(&mut p).ok()?;
        Some(String::from_utf8(p).unwrap())
    }

    fn take_pending(&mut self, needle: &str) -> Option<String> {
        let i = self.pending.iter().position(|f| f.contains(needle))?;
        Some(self.pending.remove(i))
    }

    /// Read frames until one carries `tag`. Frames that do not match are
    /// kept, not dropped.
    fn until(&mut self, tag: &str) -> String {
        let needle = format!("\"t\":\"{tag}\"");
        if let Some(f) = self.take_pending(&needle) {
            return f;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match self.try_recv() {
                Some(f) if f.contains(&needle) => return f,
                Some(f) => self.pending.push(f),
                None => break,
            }
        }
        panic!("no `{tag}` frame arrived");
    }

    fn sees(&mut self, tag: &str, window: Duration) -> bool {
        let needle = format!("\"t\":\"{tag}\"");
        if self.take_pending(&needle).is_some() {
            return true;
        }
        self.sock.set_read_timeout(Some(window)).unwrap();
        let deadline = Instant::now() + window;
        let mut seen = false;
        while Instant::now() < deadline {
            match self.try_recv() {
                Some(f) if f.contains(&needle) => {
                    seen = true;
                    break;
                }
                Some(f) => self.pending.push(f),
                None => break,
            }
        }
        self.sock
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        seen
    }
}

fn server() -> (Arc<Server>, std::net::SocketAddr) {
    let s = Arc::new(
        Server::bind(&Config {
            addr: "127.0.0.1:0".to_string(),
            cores_max: 4,
            session_ttl: Duration::from_secs(60),
            verbose: false,
        })
        .expect("bind"),
    );
    let addr = s.local_addr().unwrap();
    let t = s.clone();
    std::thread::spawn(move || {
        let _ = t.serve();
    });
    (s, addr)
}

const RELAY: &str = "deploy\nfor(y <- @{Nil}){ *y } | @{Nil}!(@{Nil}!(Nil))";

/// Replication from reflection: this one never comes to rest, which is what
/// makes "did it keep running?" a question with a loud answer.
const REPLICATOR: &str = "deploy\nnew x in { for(y <- x){ x!(*y) | *y } | x!( for(y <- x){ x!(*y) | *y } | stdout!(Nil) ) }";

// ---------------------------------------------------------------------------
// The handshake

#[test]
fn the_handshake_accept_value_is_the_one_rfc_6455_gives() {
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let d = ws::sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
    assert_eq!(ws::b64(&d), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}

#[test]
fn sha1_and_base64_agree_with_their_published_vectors() {
    let hex = |d: [u8; 20]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        hex(ws::sha1(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(hex(ws::sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(ws::b64(b"f"), "Zg==");
    assert_eq!(ws::b64(b"fo"), "Zm8=");
    assert_eq!(ws::b64(b"foo"), "Zm9v");
    assert_eq!(ws::b64(b"foobar"), "Zm9vYmFy");
}

// ---------------------------------------------------------------------------
// The lifecycle

#[test]
fn a_client_attaches_and_is_told_what_it_attached_to() {
    let (srv, addr) = server();
    let mut c = Client::connect(addr, "e2e-hello");
    let ready = c.until("ready");
    assert!(ready.contains("\"session\":\"e2e-hello\""), "{ready}");
    assert!(ready.contains("\"resumed\":false"), "{ready}");
    assert!(ready.contains("\"coresMax\":4"), "{ready}");
    assert!(
        ready.contains(&format!("\"wire\":{}", wire_version())),
        "{ready}"
    );
    // The world is drawn before anything is asked for.
    assert!(c.until("scene").contains("\"rhobots\":["));
    srv.shutdown();
}

#[test]
fn a_round_reaches_the_client_and_its_acknowledgment_reaches_the_session() {
    // The pacing itself is tested in `f1r3x-host`; what is tested here is that
    // both ends of it survive a socket.
    let (srv, addr) = server();
    let mut c = Client::connect(addr, "e2e-round");
    c.until("ready");
    c.send(RELAY);
    assert!(c.until("reply").contains("\"ok\":true"));
    c.send("play");
    c.until("reply");

    let first = c.until("round");
    assert!(first.contains("\"round\":1"), "{first}");
    assert!(first.contains("\"scene\":{"), "a round carries the scene");
    assert!(first.contains("\"inFlight\":1"), "{first}");

    c.send("ack 1");
    assert!(c.until("round").contains("\"round\":2"));
    srv.shutdown();
}

#[test]
fn a_reconnect_resumes_the_same_world() {
    let (srv, addr) = server();
    let mut c = Client::connect(addr, "e2e-resume");
    c.until("ready");
    c.send("deploy\n@{Nil}!(@{Nil}!(Nil))");
    c.until("reply");
    c.send("step");
    c.until("reply");
    c.until("round");
    drop(c);

    std::thread::sleep(Duration::from_millis(150));
    let mut c2 = Client::connect(addr, "e2e-resume");
    let ready = c2.until("ready");
    assert!(ready.contains("\"resumed\":true"), "{ready}");
    c2.send("status");
    assert!(c2.until("reply").contains("\"deploys\":1"));
    srv.shutdown();
}

#[test]
fn a_detached_session_stops_advancing() {
    // A round nobody can see would never be acknowledged, so a session with no
    // socket must not run on -- which is also what makes the resume above pick
    // up where it stopped rather than skip ahead.
    //
    // The signal is buffer exhaustion rather than an exact tick count. The
    // term below is a replicator, so a session that ignored detachment would
    // have filled its whole buffer within a millisecond or two of the socket
    // going, and admission would still be closed on reattach: no round would
    // arrive at all, and the read below would time out. A session that stops
    // when detached is still holding round 1, so round 2 arrives at once.
    //
    // An earlier version asserted `tick == 1`, which was wrong. A few rounds
    // always slip through between a client closing its end and the server
    // noticing, and how many depends on how fast the machine is -- so it
    // passed on a slow box and failed on a fast one, which is the worst way
    // for a test to be wrong.
    let (srv, addr) = server();
    let mut c = Client::connect(addr, "e2e-detach");
    c.until("ready");
    c.send(REPLICATOR);
    assert!(c.until("reply").contains("\"ok\":true"));
    c.send("pacing free");
    c.until("reply");
    c.send("buffer 256");
    c.until("reply");
    c.send("play");
    c.until("reply");
    c.until("round");
    drop(c);

    std::thread::sleep(Duration::from_millis(600));
    let mut c2 = Client::connect(addr, "e2e-detach");
    c2.until("ready");
    let round = c2.until("round");
    let n: u64 = round
        .split("\"round\":")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(u64::MAX);
    assert!(
        n < 128,
        "the session ran on while detached: it reached round {n} in 600ms"
    );
    srv.shutdown();
}

#[test]
fn a_second_connection_evicts_the_first_and_says_so() {
    let (srv, addr) = server();
    let mut a = Client::connect(addr, "e2e-evict");
    a.until("ready");
    let mut b = Client::connect(addr, "e2e-evict");
    b.until("ready");
    assert!(
        a.sees("error", Duration::from_secs(2)),
        "the evicted client was not told"
    );
    srv.shutdown();
}

#[test]
fn a_bad_term_comes_back_as_diagnostics_not_a_dropped_socket() {
    let (srv, addr) = server();
    let mut c = Client::connect(addr, "e2e-bad");
    c.until("ready");
    c.send("deploy\nfor(y <- ){ *y");
    let reply = c.until("reply");
    assert!(reply.contains("\"ok\":false"), "{reply}");
    assert!(reply.contains("\"diags\":["), "{reply}");
    // And the connection is still usable afterwards.
    c.send("ping");
    assert!(c.until("reply").contains("pong"));
    srv.shutdown();
}

#[test]
fn bye_destroys_the_session() {
    let (srv, addr) = server();
    let mut c = Client::connect(addr, "e2e-bye");
    c.until("ready");
    c.send("bye");
    c.until("reply");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!srv.reg.names().contains(&"e2e-bye".to_string()));
    srv.shutdown();
}

#[test]
fn a_term_can_be_preloaded_before_anyone_connects() {
    let (srv, addr) = server();
    srv.preload("default", "@{Nil}!(Nil)").expect("preload");
    let mut c = Client::connect(addr, "default");
    let ready = c.until("ready");
    assert!(ready.contains("\"resumed\":true"), "{ready}");
    c.send("status");
    assert!(c.until("reply").contains("\"deploys\":1"));
    srv.shutdown();
}

// ---------------------------------------------------------------------------
// The registry, which is native hosting and therefore lives here

#[test]
fn a_detached_session_expires() {
    let reg = Registry::new(4, Duration::from_millis(1));
    assert!(reg.is_empty());
    let (_h, existed) = reg.open("gone");
    assert!(!existed);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(reg.reap(), vec!["gone".to_string()]);
    assert!(reg.is_empty());
}

#[test]
fn a_session_is_resumed_by_name_not_reissued() {
    let reg = Registry::new(4, Duration::from_secs(60));
    let (a, existed) = reg.open("client-1");
    assert!(!existed);
    let (b, existed) = reg.open("client-1");
    assert!(existed);
    assert!(Arc::ptr_eq(&a, &b));
}
