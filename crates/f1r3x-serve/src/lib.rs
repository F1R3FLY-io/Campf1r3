//! `f1r3x-serve` — the native host.
//!
//! A rho executive on a machine with sockets and threads, served to anything
//! that speaks WebSocket. It contains no parser, no normaliser, no store, no
//! interpreter, no scheduling policy and no command vocabulary — all of that
//! is `f1r3x-host` and the crates below it. What is here is a socket, a
//! registry of sessions, and a loop that turns [`Session::poll`] into frames
//! on a wire.
//!
//! ```text
//!   any client                    this machine
//!   ┌──────────────┐  WebSocket   ┌──────────────────────────────┐
//!   │ headset,     │◀────────────▶│ f1r3x-serve   registry · ws  │
//!   │ browser, CLI │  rounds/acks │        ↓                     │
//!   └──────────────┘              │ f1r3x-host    session·pacing │
//!                                 │ f1r3x · b3ll0ws · campf1r3   │
//!                                 └──────────────────────────────┘
//! ```
//!
//! ## Why a session outlives its socket
//!
//! Clients go away. A headset is taken off, sleeps, is backgrounded, and roams
//! between access points; a browser tab is reloaded. Every one of those drops
//! the socket, and if that destroyed the executive a person would lose a
//! running world by putting a device down for a minute. Sessions are named by
//! the client — the name is in the request path, and lives in the client's own
//! storage rather than in a cookie this server issued — so a resume works
//! across a process the system has restarted. An unknown name is not an error;
//! it creates the session, so a first run and a resume are the same call.
//!
//! One rule keeps this honest: **a session has at most one connection.** A
//! second attach evicts the first and says so, because two clients
//! acknowledging into one executive would each see pacing neither asked for.

#![forbid(unsafe_code)]

pub mod ws;

use std::collections::BTreeMap;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use f1r3x_host::{proto, Clock, Config as HostConfig, Frame, Session, SystemClock, WIRE_VERSION};

pub struct Config {
    pub addr: String,
    /// Upper bound of the client's round-width control.
    pub cores_max: u32,
    /// How long a detached session keeps its world.
    pub session_ttl: Duration,
    pub verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            addr: "0.0.0.0:9099".to_string(),
            cores_max: f1r3x_host::available_parallelism(),
            session_ttl: Duration::from_secs(900),
            verbose: false,
        }
    }
}

// ---------------------------------------------------------------------------
// A hosted session: the host's session, plus the socket it speaks on.

struct Hosted {
    session: Mutex<Session>,
    /// `None` while detached. The poll loop will not advance without it: a
    /// round nobody can see would never be acknowledged, and this is also
    /// what makes a reconnect resume mid-run rather than skip ahead.
    out: Mutex<Option<Sink>>,
    closing: AtomicBool,
    detached_since: Mutex<Option<Instant>>,
    epoch: AtomicU64,
    name: String,
}

struct Sink {
    sock: TcpStream,
    epoch: u64,
}

impl Hosted {
    fn new(name: &str, cores_max: u32) -> Hosted {
        Hosted {
            session: Mutex::new(Session::new(HostConfig {
                cores_max,
                ..HostConfig::default()
            })),
            out: Mutex::new(None),
            closing: AtomicBool::new(false),
            detached_since: Mutex::new(Some(Instant::now())),
            epoch: AtomicU64::new(0),
            name: name.to_string(),
        }
    }

    /// Write one frame, if anyone is listening. A write failure is not an
    /// error here: it means the socket has gone, and the reader will notice.
    fn emit(&self, frame: &Frame) {
        let mut guard = self.out.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(sink) = guard.as_mut() {
            if ws::write_text(&mut sink.sock, &frame.json).is_err() {
                *guard = None;
                *self.detached_since.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(Instant::now());
            }
        }
    }

    fn attached(&self) -> bool {
        self.out.lock().unwrap_or_else(|p| p.into_inner()).is_some()
    }

    fn attach(&self, sock: TcpStream) -> u64 {
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let mut guard = self.out.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(mut old) = guard.take() {
            let _ = ws::write_text(
                &mut old.sock,
                &Frame::error(
                    "evicted",
                    "another connection attached to this session; \
                     a session serves one client at a time",
                )
                .json,
            );
            let _ = ws::write_close(&mut old.sock);
            let _ = old.sock.shutdown(std::net::Shutdown::Both);
        }
        *guard = Some(Sink { sock, epoch });
        *self.detached_since.lock().unwrap_or_else(|p| p.into_inner()) = None;
        epoch
    }

    fn detach(&self, epoch: u64) {
        let mut guard = self.out.lock().unwrap_or_else(|p| p.into_inner());
        if guard.as_ref().map(|s| s.epoch) == Some(epoch) {
            *guard = None;
            *self.detached_since.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
        }
    }

    fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let mut guard = self.out.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(sink) = guard.as_mut() {
            let _ = ws::write_close(&mut sink.sock);
            let _ = sink.sock.shutdown(std::net::Shutdown::Both);
        }
        *guard = None;
    }
}

// ---------------------------------------------------------------------------
// The registry
//
// Native hosting only. A browser tab is one page and one executive, and needs
// none of this — which is why it lives here and not in `f1r3x-host`.

pub struct Registry {
    sessions: Mutex<BTreeMap<String, Arc<Hosted>>>,
    cores_max: u32,
    ttl: Duration,
}

impl Registry {
    pub fn new(cores_max: u32, ttl: Duration) -> Registry {
        Registry {
            sessions: Mutex::new(BTreeMap::new()),
            cores_max,
            ttl,
        }
    }

    fn open(&self, name: &str) -> (Arc<Hosted>, bool) {
        let mut map = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(s) = map.get(name) {
            return (s.clone(), true);
        }
        let s = Arc::new(Hosted::new(name, self.cores_max));
        map.insert(name.to_string(), s.clone());
        (s, false)
    }

    pub fn names(&self) -> Vec<String> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn remove(&self, name: &str) -> bool {
        let s = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(name);
        if let Some(s) = &s {
            s.close();
        }
        s.is_some()
    }

    /// Drop sessions detached longer than the TTL. A device put down for five
    /// minutes keeps its world; one abandoned for an hour does not hold memory
    /// for the rest of the day.
    pub fn reap(&self) -> Vec<String> {
        let now = Instant::now();
        let mut dead = Vec::new();
        {
            let map = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            for (name, s) in map.iter() {
                if let Some(t) = *s.detached_since.lock().unwrap_or_else(|p| p.into_inner()) {
                    if now.duration_since(t) > self.ttl {
                        dead.push(name.clone());
                    }
                }
            }
        }
        for name in &dead {
            self.remove(name);
        }
        dead
    }

    pub fn close_all(&self) {
        for s in self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
        {
            s.close();
        }
    }
}

// ---------------------------------------------------------------------------
// The server

pub struct Server {
    pub reg: Arc<Registry>,
    pub stop: Arc<AtomicBool>,
    listener: TcpListener,
    verbose: bool,
}

impl Server {
    pub fn bind(cfg: &Config) -> io::Result<Server> {
        Ok(Server {
            reg: Arc::new(Registry::new(cfg.cores_max, cfg.session_ttl)),
            stop: Arc::new(AtomicBool::new(false)),
            listener: TcpListener::bind(&cfg.addr)?,
            verbose: cfg.verbose,
        })
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Deploy a term into a named session before anyone connects, so a demo
    /// can start with something already standing.
    pub fn preload(&self, session: &str, source: &str) -> Result<u32, Vec<f1r3x::Diag>> {
        let (h, _) = self.reg.open(session);
        let mut s = h.session.lock().unwrap_or_else(|p| p.into_inner());
        s.executive_mut().deploy(source).map_err(|e| e.diags)
    }

    pub fn serve(&self) -> io::Result<()> {
        let reg = self.reg.clone();
        let stop = self.stop.clone();
        let verbose = self.verbose;
        thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_secs(15));
                for name in reg.reap() {
                    if verbose {
                        eprintln!("[reap] session {name} expired");
                    }
                }
            }
        });

        for incoming in self.listener.incoming() {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let sock = match incoming {
                Ok(s) => s,
                Err(e) => {
                    if self.verbose {
                        eprintln!("[accept] {e}");
                    }
                    continue;
                }
            };
            let reg = self.reg.clone();
            let verbose = self.verbose;
            thread::spawn(move || {
                if let Err(e) = connection(reg, sock, verbose) {
                    if verbose {
                        eprintln!("[conn] {e}");
                    }
                }
            });
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.reg.close_all();
        // Unblock `incoming` by connecting to ourselves.
        if let Ok(addr) = self.listener.local_addr() {
            let _ = TcpStream::connect(addr);
        }
    }
}

/// One connection: handshake, attach, poll loop, read loop.
fn connection(reg: Arc<Registry>, mut sock: TcpStream, verbose: bool) -> io::Result<()> {
    sock.set_nodelay(true)?;
    let target = ws::accept(&mut sock)?;

    // The session name is the request path: `/s/<name>`. Putting it in the URL
    // rather than in a first message means the socket is bound to a session
    // before any request is read, so a resumed session can be sent its scene
    // immediately and a client that only listens never has to speak first.
    let name = target
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && *s != "s")
        .unwrap_or("default")
        .to_string();
    if name.len() > 128
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        let _ = ws::write_text(
            &mut sock,
            &Frame::error("bad-session", "the session name in the path is not acceptable").json,
        );
        let _ = ws::write_close(&mut sock);
        return Ok(());
    }

    let (h, resumed) = reg.open(&name);
    let epoch = h.attach(sock.try_clone()?);
    if verbose {
        eprintln!("[conn] session {name} attached (resumed: {resumed})");
    }

    // Greet, then draw the world before anything is asked for. A resumed
    // client sees what it left; a fresh one sees an empty plane, which is also
    // correct.
    {
        let ready = {
            let mut s = h.session.lock().unwrap_or_else(|p| p.into_inner());
            s.ready(&name, resumed)
        };
        h.emit(&ready);
        let s = h.session.lock().unwrap_or_else(|p| p.into_inner());
        let scene = s.scene_frame();
        let stats = s.stats();
        drop(s);
        h.emit(&scene);
        h.emit(&stats);
    }

    // One poll loop per session. Where the previous design parked a thread on
    // a condition variable, this one asks the session what has happened —
    // which is the change that lets the same session run in a browser's event
    // loop with no thread at all.
    if !resumed {
        let hosted = h.clone();
        thread::spawn(move || poll_loop(hosted));
    }

    let result = read_loop(&reg, &h, &mut sock, epoch, verbose);
    h.detach(epoch);
    if verbose {
        eprintln!("[conn] session {name} detached");
    }
    result
}

fn poll_loop(h: Arc<Hosted>) {
    let clock = SystemClock::default();
    while !h.closing.load(Ordering::SeqCst) {
        if !h.attached() {
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        let frames = {
            let mut s = h.session.lock().unwrap_or_else(|p| p.into_inner());
            s.poll(clock.now_ms())
        };
        for f in &frames {
            h.emit(f);
        }
        // Nothing fell out, so there is nothing to do until something changes.
        // A few milliseconds is well under the eye's threshold and costs
        // nothing; the alternative is a condition variable, which is the thing
        // this refactor exists to remove.
        if frames.is_empty() {
            thread::sleep(Duration::from_millis(8));
        }
    }
}

fn read_loop(
    reg: &Arc<Registry>,
    h: &Arc<Hosted>,
    sock: &mut TcpStream,
    epoch: u64,
    verbose: bool,
) -> io::Result<()> {
    loop {
        // If this connection has been evicted by a later one, stop reading.
        if h.epoch.load(Ordering::SeqCst) != epoch || h.closing.load(Ordering::SeqCst) {
            return Ok(());
        }
        let msg = match ws::read(sock) {
            Ok(Some(m)) => m,
            Ok(None) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        match msg {
            ws::Msg::Close => return Ok(()),
            ws::Msg::Ping(p) => {
                let mut g = h.out.lock().unwrap_or_else(|x| x.into_inner());
                if let Some(sink) = g.as_mut() {
                    let _ = ws::write(&mut sink.sock, 0xA, &p);
                }
            }
            ws::Msg::Pong(_) => {}
            ws::Msg::Binary(_) => h.emit(&Frame::error(
                "binary",
                "requests are text frames; this protocol has no binary form",
            )),
            ws::Msg::Text(req) => {
                if verbose {
                    eprintln!("[req] {}", req.lines().next().unwrap_or(""));
                }
                let head = command_of(&req);
                let (reply, after) = {
                    let mut s = h.session.lock().unwrap_or_else(|p| p.into_inner());
                    s.handle(&req)
                };
                h.emit(&reply);

                // Some commands change what is drawn, so the scene follows
                // them without the client having to ask.
                if matches!(
                    head.as_str(),
                    "deploy" | "tick" | "run" | "reset" | "resend" | "treedepth"
                ) {
                    let s = h.session.lock().unwrap_or_else(|p| p.into_inner());
                    let scene = s.scene_frame();
                    let stats = s.stats();
                    drop(s);
                    h.emit(&scene);
                    h.emit(&stats);
                } else if head == "cores" || head == "pacing" || head == "buffer" {
                    let s = h.session.lock().unwrap_or_else(|p| p.into_inner());
                    let stats = s.stats();
                    drop(s);
                    h.emit(&stats);
                }

                if matches!(after, proto::After::Close) {
                    reg.remove(&h.name);
                    return Ok(());
                }
            }
        }
    }
}

/// The verb of a request, with any `#n ` correlation prefix removed.
fn command_of(req: &str) -> String {
    let line = req.lines().next().unwrap_or("").trim();
    let rest = match line.strip_prefix('#') {
        Some(r) => r.split_once(' ').map(|(_, t)| t).unwrap_or(""),
        None => line,
    };
    rest.split_whitespace().next().unwrap_or("").to_string()
}

/// The host protocol version this server speaks.
pub fn wire_version() -> u32 {
    WIRE_VERSION
}

#[cfg(test)]
mod tests;
