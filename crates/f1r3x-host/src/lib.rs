//! `f1r3x-host` — hosting a rho executive, without saying how.
//!
//! `f1r3x` decides *what* reduces. This decides *when* a decided commit is
//! released to whoever is watching, and packages the result as frames. It
//! knows nothing about sockets, threads, or the wall clock, and nothing at all
//! about any particular client.
//!
//! That is the whole point. There are three hosts, and they differ only in
//! what they wire:
//!
//! ```text
//!   f1r3x-serve   TcpListener + threads + WebSocket     a Mac on the network
//!   f1r3x-wasm    the page's own event loop             a browser tab
//!   f1r3x-ffi     whatever loop the caller has          linked in-process
//! ```
//!
//! Two decisions make that possible, and both were mistakes in the first
//! version of this code, which was written as a server and only afterwards
//! asked to be anything else.
//!
//! **The driver is a state machine, not a thread.** [`Session::poll`] takes
//! the current time and returns the frames that fell out. A native host calls
//! it from a thread; a browser calls it from `requestAnimationFrame`. The
//! admission logic does not change between them, because it was always a pure
//! predicate over the control state — it was only ever the *waiting* that
//! needed a thread.
//!
//! **The clock is injected.** `std::time::Instant::now` panics on
//! `wasm32-unknown-unknown`, which is one of the reasons `CampF1R3` exists at
//! all; a host layer that called it would have put the hazard straight back.
//! Time arrives as a `u64` of milliseconds from whatever the host has —
//! `Instant` natively, `performance.now()` in a page.
//!
//! ## Pacing
//!
//! How the scene and the evaluator are paced against each other is the
//! programmer's choice:
//!
//! - **Lockstep.** Admit a round of at most `cores` commits, then withhold
//!   admission of the next until the client acknowledges that it has finished
//!   showing this one.
//! - **Free-running.** Admit without waiting. Rounds feed a bounded buffer
//!   that the client drains; when it is full, admission pauses until an entry
//!   is drained, so the evaluator never runs further ahead than the bound and
//!   *every commit is seen*.
//!
//! The two are one rule with different bounds — lockstep is free-running with
//! a buffer of one — and [`Control::admission_open`] is where it lives. In
//! process this was a semaphore; across any boundary at all it has to be an
//! acknowledgment.

#![forbid(unsafe_code)]

pub mod proto;
pub mod scene;

use f1r3x::{Executive, Mode, Settings};
use k1ndl1ng_norm::json::{Buf, Budget};

pub use f1r3x::{Deploy, Diag, Step};
pub use k1ndl1ng_norm::json::Budget as TreeBudget;

/// Bumped when a client must notice a change to the host protocol. The
/// executive's own protocol version is reported separately, by `status`.
pub const WIRE_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// Time

/// Milliseconds from an arbitrary but monotonic origin.
///
/// A trait rather than a call, because the one thing every target has is a
/// different way of asking. Hosts that have no clock at all can pass a
/// counter: nothing here compares times across hosts, and the only uses are a
/// stats cadence and a throughput average.
pub trait Clock {
    fn now_ms(&self) -> u64;
}

/// A clock that never advances, for a host with nothing better and for tests.
/// The stats cadence degenerates to "every poll", which is harmless.
pub struct Frozen;

impl Clock for Frozen {
    fn now_ms(&self) -> u64 {
        0
    }
}

#[cfg(feature = "std")]
pub struct SystemClock {
    origin: std::time::Instant,
}

#[cfg(feature = "std")]
impl Default for SystemClock {
    fn default() -> Self {
        SystemClock {
            origin: std::time::Instant::now(),
        }
    }
}

#[cfg(feature = "std")]
impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
}

// ---------------------------------------------------------------------------
// Frames

/// One thing the host has to say.
///
/// The payload is already-serialised JSON. Framing is done once, here, so that
/// three transports cannot drift apart in how they spell a round.
#[derive(Clone, Debug)]
pub struct Frame {
    pub tag: &'static str,
    pub json: String,
}

impl Frame {
    fn new(tag: &'static str, body: impl FnOnce(&mut Buf)) -> Frame {
        let mut b = Buf::new();
        b.raw("{\"t\":\"");
        b.raw(tag);
        b.raw("\"");
        body(&mut b);
        b.raw("}");
        Frame {
            tag,
            json: b.into_string(),
        }
    }

    pub fn error(code: &str, message: &str) -> Frame {
        Frame::new("error", |b| {
            b.comma();
            b.field_str("code", code);
            b.comma();
            b.field_str("message", message);
        })
    }
}

// ---------------------------------------------------------------------------
// Pacing

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Pacing {
    Lockstep,
    FreeRunning,
}

impl Pacing {
    pub fn name(self) -> &'static str {
        match self {
            Pacing::Lockstep => "lockstep",
            Pacing::FreeRunning => "free-running",
        }
    }
}

/// What the client's controls set. Held apart from the executive so a host can
/// read it without touching the machine.
#[derive(Clone, Debug)]
pub struct Control {
    pub pacing: Pacing,
    /// Rounds admitted but not yet acknowledged: the buffer's fill.
    pub in_flight: u64,
    /// The buffer bound, in rounds. Lockstep ignores it: its bound is one.
    pub buffer: u64,
    /// The client's run/pause.
    pub running: bool,
    /// Rounds still owed after a `step`, for single-stepping in either mode.
    pub steps_owed: u64,
    /// Quotation depth the client can still draw; past it the tree elides.
    pub quote_depth: u32,
    /// Rounds admitted since the session began.
    pub round: u64,
}

impl Default for Control {
    fn default() -> Self {
        Control {
            pacing: Pacing::Lockstep,
            in_flight: 0,
            buffer: 128,
            running: false,
            steps_owed: 0,
            quote_depth: 4,
            round: 0,
        }
    }
}

impl Control {
    /// Whether another round may be admitted.
    ///
    /// Lockstep is free-running with a bound of one; writing it that way
    /// rather than as two code paths is what keeps the modes honestly
    /// comparable, and is why there is no second branch anywhere below.
    pub fn admission_open(&self) -> bool {
        self.in_flight < self.bound()
    }

    pub fn bound(&self) -> u64 {
        match self.pacing {
            Pacing::Lockstep => 1,
            Pacing::FreeRunning => self.buffer.max(1),
        }
    }

    pub fn wants_to_advance(&self) -> bool {
        (self.running || self.steps_owed > 0) && self.admission_open()
    }
}

// ---------------------------------------------------------------------------
// The session

pub struct Config {
    /// Upper bound of the client's round-width control: the parallelism
    /// visible to this process, which a host supplies because only a host
    /// knows.
    pub cores_max: u32,
    /// How often stats are emitted while nothing else is happening.
    pub stats_interval_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            cores_max: 1,
            stats_interval_ms: 250,
        }
    }
}

/// One executive, plus the pacing state around it.
///
/// Single-threaded and `&mut`-driven. A host that serves several of these
/// keeps them in whatever collection it likes and takes whatever lock it
/// needs; a host that serves one — a browser tab — needs neither.
pub struct Session {
    exec: Executive,
    ctl: Control,
    cfg: Config,
    /// For the throughput readout.
    comms_mark: u64,
    comms_at_ms: u64,
    rate: f64,
    last_stats_ms: u64,
    /// Set once the client has been told what it attached to.
    greeted: bool,
}

impl Session {
    pub fn new(cfg: Config) -> Session {
        Session {
            // The host owns pacing, so the executive is always asked for one
            // round at a time; `cores` is the round's width.
            exec: Executive::new(Settings {
                mode: Mode::Lockstep,
                ..Settings::default()
            }),
            ctl: Control::default(),
            cfg,
            comms_mark: 0,
            comms_at_ms: 0,
            rate: 0.0,
            last_stats_ms: 0,
            greeted: false,
        }
    }

    pub fn control(&self) -> &Control {
        &self.ctl
    }
    pub fn control_mut(&mut self) -> &mut Control {
        &mut self.ctl
    }
    pub fn executive(&self) -> &Executive {
        &self.exec
    }
    pub fn executive_mut(&mut self) -> &mut Executive {
        &mut self.exec
    }
    pub fn cores_max(&self) -> u32 {
        self.cfg.cores_max
    }

    /// Throw the world away and keep the session.
    pub fn reset(&mut self) {
        let cores = self.exec.settings().cores;
        self.exec = Executive::new(Settings {
            mode: Mode::Lockstep,
            cores,
            ..Settings::default()
        });
        self.ctl = Control {
            quote_depth: self.ctl.quote_depth,
            pacing: self.ctl.pacing,
            buffer: self.ctl.buffer,
            ..Control::default()
        };
        self.comms_mark = 0;
        self.rate = 0.0;
    }

    /// The client has finished with everything through this round, releasing
    /// admission. An ack names the highest round dealt with, so a client that
    /// batches acks, or whose ack was lost in a reconnect, still frees the
    /// right number of slots and can never push the count below zero.
    pub fn ack(&mut self, upto: u64) {
        let outstanding = self.ctl.round.saturating_sub(upto);
        self.ctl.in_flight = self.ctl.in_flight.min(outstanding);
    }

    /// Greet a client. `resumed` is the host's to know, not this crate's.
    pub fn ready(&mut self, name: &str, resumed: bool) -> Frame {
        self.greeted = true;
        let cores_max = self.cfg.cores_max as u64;
        Frame::new("ready", |b| {
            b.comma();
            b.field_num("wire", WIRE_VERSION as u64);
            b.comma();
            b.field_str("session", name);
            b.comma();
            b.field_bool("resumed", resumed);
            b.comma();
            b.field_num("coresMax", cores_max);
        })
    }

    /// Handle one request. See [`proto`] for the vocabulary.
    pub fn handle(&mut self, request: &str) -> (Frame, proto::After) {
        proto::handle(self, request)
    }

    /// One turn of the host's loop: admit a round if admission allows, and
    /// emit stats on their cadence. Returns what fell out, in order.
    ///
    /// A host that has nobody listening should not call this. A round nobody
    /// can see is a round that will never be acknowledged, and the session
    /// would wedge at the first one — which is also exactly what makes a
    /// reconnect resume mid-run rather than skip ahead.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Frame> {
        let mut out = Vec::new();

        if self.ctl.wants_to_advance() {
            match self.exec.tick() {
                Err(e) => {
                    self.ctl.running = false;
                    self.ctl.steps_owed = 0;
                    out.push(Frame::error("store", &format!("{e:?}")));
                }
                Ok(steps) => {
                    let quiescent = self.exec.is_quiescent();
                    let steps_json = f1r3x::json::steps(&steps);
                    let scene_json = self.scene_json();

                    self.ctl.round += 1;
                    self.ctl.in_flight += 1;
                    if self.ctl.steps_owed > 0 {
                        self.ctl.steps_owed -= 1;
                    }
                    if quiescent {
                        self.ctl.running = false;
                        self.ctl.steps_owed = 0;
                    }
                    self.update_rate(now_ms);

                    let round = self.ctl.round;
                    let cores = self.exec.settings().cores as u64;
                    let pacing = self.ctl.pacing;
                    let in_flight = self.ctl.in_flight;
                    let bound = self.ctl.bound();

                    out.push(Frame::new("round", |b| {
                        b.comma();
                        b.field_num("round", round);
                        b.comma();
                        b.field_num("cores", cores);
                        b.comma();
                        b.field_str("pacing", pacing.name());
                        b.comma();
                        b.field_raw("steps", &steps_json);
                        b.comma();
                        b.field_bool("quiescent", quiescent);
                        b.comma();
                        // What must come back to free admission. In lockstep
                        // this is the gate; in free-running it is the drain.
                        b.field_bool("ackRequired", true);
                        b.comma();
                        b.field_num("inFlight", in_flight);
                        b.comma();
                        b.field_num("buffer", bound);
                        b.comma();
                        b.field_raw("scene", &scene_json);
                    }));

                    if quiescent {
                        out.push(Frame::new("quiescent", |b| {
                            b.comma();
                            b.field_num("round", round);
                        }));
                    }
                    out.push(self.stats());
                    self.last_stats_ms = now_ms;
                    return out;
                }
            }
        }

        if now_ms.saturating_sub(self.last_stats_ms) >= self.cfg.stats_interval_ms {
            self.last_stats_ms = now_ms;
            out.push(self.stats());
        }
        out
    }

    /// The scene as it stands, without advancing anything.
    pub fn scene_frame(&self) -> Frame {
        let json = self.scene_json();
        let round = self.ctl.round;
        Frame::new("scene", |b| {
            b.comma();
            b.field_num("round", round);
            b.comma();
            b.field_raw("scene", &json);
        })
    }

    fn scene_json(&self) -> String {
        let mut b = Buf::with_capacity(8192);
        scene::write(
            &mut b,
            &self.exec.scene(),
            self.exec.machine(),
            Budget::with_quote_depth(self.ctl.quote_depth),
        );
        b.into_string()
    }

    fn update_rate(&mut self, now_ms: u64) {
        let (_, comms) = self.exec.machine().counters();
        let dt = now_ms.saturating_sub(self.comms_at_ms);
        if dt >= 500 {
            self.rate = (comms.saturating_sub(self.comms_mark)) as f64 * 1000.0 / dt as f64;
            self.comms_mark = comms;
            self.comms_at_ms = now_ms;
        }
    }

    /// Buffer fill, whether admission is open or paused by back-pressure, and
    /// commits per second.
    pub fn stats(&self) -> Frame {
        let (steps, comms) = self.exec.machine().counters();
        let (data, waiting) = self.exec.machine().space().stats();
        let c = self.ctl.clone();
        let cores = self.exec.settings().cores as u64;
        let cores_max = self.cfg.cores_max as u64;
        let tick = self.exec.tick_count();
        let queued = self.exec.machine().queued() as u64;
        let deploys = self.exec.deploys().len() as u64;
        let quiescent = self.exec.is_quiescent();
        let rate = self.rate;
        Frame::new("stats", |b| {
            b.comma();
            b.field_num("round", c.round);
            b.comma();
            b.field_num("tick", tick);
            b.comma();
            b.field_str("pacing", c.pacing.name());
            b.comma();
            b.field_bool("running", c.running);
            b.comma();
            b.field_num("cores", cores);
            b.comma();
            b.field_num("coresMax", cores_max);
            b.comma();
            b.field_num("inFlight", c.in_flight);
            b.comma();
            b.field_num("buffer", c.bound());
            b.comma();
            // False means the evaluator is waiting for the client: this is
            // back-pressure, not a stall, and a client that shows it as one
            // will send people looking for a bug that is not there.
            b.field_bool("admissionOpen", c.admission_open());
            b.comma();
            b.field_num("commitsPerSec", rate.round() as u64);
            b.comma();
            b.field_num("comms", comms);
            b.comma();
            b.field_num("steps", steps);
            b.comma();
            b.field_num("queued", queued);
            b.comma();
            b.field_num("data", data as u64);
            b.comma();
            b.field_num("waiting", waiting as u64);
            b.comma();
            b.field_num("deploys", deploys);
            b.comma();
            b.field_bool("quiescent", quiescent);
        })
    }
}

/// Parallelism visible to this process. Native only: a host on a target
/// without threads supplies its own answer, which is one.
#[cfg(feature = "std")]
pub fn available_parallelism() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
        .max(1)
}

#[cfg(test)]
mod tests;
