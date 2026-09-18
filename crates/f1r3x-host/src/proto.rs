//! The request vocabulary, in one place.
//!
//! This is the command surface `f1r3x-ffi` defined, moved down so that every
//! host shares it rather than each reimplementing it. A term that runs under
//! `f1r3x run` runs identically over a socket, in a browser tab, and behind
//! the C ABI, because all three arrive here.
//!
//! It is split in two on purpose:
//!
//! - [`executive_command`] is the vocabulary that needs only an `Executive` —
//!   `deploy`, `tick`, `run`, `scene`, `trace`, `log`, `cores`, `mode`,
//!   `status`, `show`. `f1r3x-ffi` calls this directly, which is what keeps
//!   its C ABI exactly as it was.
//! - [`handle`] adds the verbs that need the pacing state around the
//!   executive, and falls through to the first for everything else.
//!
//! A request is one message. It may begin with `#<n> ` to carry a correlation
//! number, which comes back on the reply; a client that only listens for
//! pushed frames can leave it off. The request side is a line protocol
//! precisely so that no host needs a JSON parser.

use f1r3x::{json, Executive, Mode};
use k1ndl1ng_norm::json::{esc, Buf};

use crate::{Frame, Pacing, Session};

/// What the host should do after a request.
pub enum After {
    Continue,
    /// The client said goodbye, or asked for the session to be destroyed.
    Close,
}

/// Run one request against a session.
pub fn handle(s: &mut Session, request: &str) -> (Frame, After) {
    let (seq, rest) = split_seq(request);
    let (body, after) = dispatch(s, rest);
    let frame = Frame::new("reply", |b| {
        b.comma();
        match seq {
            Some(n) => b.field_num("seq", n),
            None => b.field_raw("seq", "null"),
        }
        b.comma();
        b.field_raw("body", &body);
    });
    (frame, after)
}

/// `#12 cores 4` → `(Some(12), "cores 4")`
fn split_seq(req: &str) -> (Option<u64>, &str) {
    let Some(rest) = req.strip_prefix('#') else {
        return (None, req);
    };
    // The number runs to the first space; the request may then contain
    // newlines, as `deploy` does.
    match rest.find(' ') {
        Some(i) => match rest[..i].parse::<u64>() {
            Ok(n) => (Some(n), &rest[i + 1..]),
            Err(_) => (None, req),
        },
        None => (None, req),
    }
}

fn dispatch(s: &mut Session, req: &str) -> (String, After) {
    let head = match req.find('\n') {
        Some(i) => &req[..i],
        None => req,
    };
    let head = head.trim();
    let mut words = head.split_whitespace();
    let cmd = words.next().unwrap_or("");
    let args: Vec<&str> = words.collect();

    match cmd {
        "ping" => (ok("{\"pong\":true}"), After::Continue),

        "pacing" => {
            let p = match args.first().copied() {
                Some("free") | Some("free-running") => Pacing::FreeRunning,
                Some("lockstep") => Pacing::Lockstep,
                _ => return (err("bad-argument", "pacing takes `lockstep` or `free`"), After::Continue),
            };
            // Switching from free-running to lockstep pauses admission until
            // the buffer drains, then continues in lockstep. Nothing extra is
            // needed for that: `in_flight` is already the fill, and lockstep's
            // bound of one keeps admission closed until the acks bring it to
            // zero.
            s.control_mut().pacing = p;
            (ok(&pacing_state(s)), After::Continue)
        }

        "buffer" => {
            let n: u64 = match args.first().and_then(|a| a.parse().ok()) {
                Some(n) if (1..=1024).contains(&n) => n,
                _ => return (err("bad-argument", "buffer takes 1..1024 rounds"), After::Continue),
            };
            s.control_mut().buffer = n;
            (ok(&pacing_state(s)), After::Continue)
        }

        // The client has finished with everything through this round. In
        // lockstep this is the gate the scheduler waits on; in free-running it
        // is the drain that keeps the buffer moving.
        "ack" => {
            let upto: u64 = args.first().and_then(|a| a.parse().ok()).unwrap_or(0);
            s.ack(upto);
            (ok(&pacing_state(s)), After::Continue)
        }

        "play" => {
            s.control_mut().running = true;
            (ok(&pacing_state(s)), After::Continue)
        }

        "pause" => {
            let c = s.control_mut();
            c.running = false;
            c.steps_owed = 0;
            (ok(&pacing_state(s)), After::Continue)
        }

        // Single-step admits exactly one round, in either mode.
        "step" => {
            let c = s.control_mut();
            c.steps_owed = c.steps_owed.saturating_add(1);
            (ok(&pacing_state(s)), After::Continue)
        }

        // How deep the client can still draw a quotation before it wants an
        // elision glyph — derived on the client from whatever its own notion
        // of legibility is.
        "treedepth" => {
            let d: u32 = args.first().and_then(|a| a.parse().ok()).unwrap_or(4);
            s.control_mut().quote_depth = d.min(16);
            (ok(&pacing_state(s)), After::Continue)
        }

        "reset" => {
            s.reset();
            (ok("{\"reset\":true}"), After::Continue)
        }

        "bye" => (ok("{\"bye\":true}"), After::Close),

        // Everything else is the executive's own vocabulary.
        _ => match executive_command(s.executive_mut(), req) {
            Some(reply) => (reply, After::Continue),
            None => (
                err("unknown-command", &format!("unknown command `{cmd}`")),
                After::Continue,
            ),
        },
    }
}

/// The vocabulary that needs only an executive.
///
/// `None` means the command is not one of these — the caller decides whether
/// that is an error or something it handles itself.
pub fn executive_command(x: &mut Executive, req: &str) -> Option<String> {
    let (head, body) = match req.find('\n') {
        Some(i) => (&req[..i], &req[i + 1..]),
        None => (req, ""),
    };
    let head = head.trim();
    let mut words = head.split_whitespace();
    let cmd = words.next().unwrap_or("");
    let args: Vec<&str> = words.collect();

    Some(match cmd {
        "deploy" => match x.deploy(body) {
            Err(e) => json::error(&e.diags),
            Ok(id) => json::ok(&format!("{{\"deploy\":{id}}}")),
        },
        "tick" => match x.tick() {
            Err(e) => store_error(e),
            Ok(steps) => json::ok(&format!(
                "{{\"steps\":{},\"quiescent\":{}}}",
                json::steps(&steps),
                x.is_quiescent()
            )),
        },
        "run" => match x.run_to_rest() {
            Err(e) => store_error(e),
            Ok(steps) => json::ok(&format!(
                "{{\"steps\":{},\"quiescent\":{}}}",
                json::steps(&steps),
                x.is_quiescent()
            )),
        },
        "scene" => json::ok(&json::scene(&x.scene())),
        "trace" => json::ok(&json::steps(x.trace())),
        "log" => {
            let log = x.take_log();
            json::ok(&format!("{{\"events\":{}}}", events(&log)))
        }
        "cores" => {
            let n: u32 = args.first().and_then(|a| a.parse().ok()).unwrap_or(1);
            let max: u32 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(64);
            x.set_cores(n, max);
            json::ok(&exec_status(x))
        }
        "mode" => {
            // Retained for the linked-in-process host, which has no separate
            // pacing state. A networked host should use `pacing`, which gates
            // admission rather than changing the executive's own budget.
            let m = match args.first().copied() {
                Some("free") | Some("free-running") => Mode::FreeRunning,
                _ => Mode::Lockstep,
            };
            x.set_mode(m);
            json::ok(&exec_status(x))
        }
        "status" => json::ok(&exec_status(x)),
        "show" => match x.deploys().last() {
            None => err("no-deploy", "nothing has been deployed yet"),
            Some(d) => json::ok(&format!(
                "{{\"source\":\"{}\",\"normal\":\"{}\",\"hash\":\"{}\"}}",
                json::esc(&d.source),
                json::esc(&f1r3x::show(&d.term)),
                d.term.hash().hex()
            )),
        },
        "" => err("empty", "no command"),
        _ => return None,
    })
}

fn exec_status(x: &Executive) -> String {
    let s = x.settings();
    let (steps, comms) = x.machine().counters();
    let (data, waiting) = x.machine().space().stats();
    format!(
        "{{\"cores\":{},\"mode\":\"{}\",\"tick\":{},\"steps\":{},\"comms\":{},\"queued\":{},\"data\":{},\"waiting\":{},\"deploys\":{},\"quiescent\":{},\"protocol\":{}}}",
        s.cores,
        match s.mode {
            Mode::Lockstep => "lockstep",
            Mode::FreeRunning => "free-running",
        },
        x.tick_count(),
        steps,
        comms,
        x.machine().queued(),
        data,
        waiting,
        x.deploys().len(),
        x.is_quiescent(),
        crate::WIRE_VERSION
    )
}

fn pacing_state(s: &Session) -> String {
    let c = s.control();
    let mut b = Buf::new();
    b.raw("{");
    b.field_str("pacing", c.pacing.name());
    b.comma();
    b.field_bool("running", c.running);
    b.comma();
    b.field_num("round", c.round);
    b.comma();
    b.field_num("inFlight", c.in_flight);
    b.comma();
    b.field_num("buffer", c.bound());
    b.comma();
    b.field_bool("admissionOpen", c.admission_open());
    b.comma();
    b.field_num("quoteDepth", c.quote_depth as u64);
    b.raw("}");
    b.into_string()
}

fn events(v: &[campf1r3_core::Event]) -> String {
    let one = |e: &campf1r3_core::Event| match e {
        campf1r3_core::Event::Produce { chan, datum, persist } => format!(
            "{{\"kind\":\"produce\",\"chan\":\"{}\",\"datum\":\"{}\",\"persist\":{persist}}}",
            hex(chan),
            hex(datum)
        ),
        campf1r3_core::Event::Consume { group, cont, persist, installing } => format!(
            "{{\"kind\":\"consume\",\"cont\":\"{}\",\"group\":[{}],\"persist\":{persist},\"installing\":{installing}}}",
            hex(cont),
            group.0.iter().map(|h| format!("\"{}\"", hex(h))).collect::<Vec<_>>().join(",")
        ),
        campf1r3_core::Event::Comm { group, cont, data } => format!(
            "{{\"kind\":\"comm\",\"cont\":\"{}\",\"group\":[{}],\"data\":[{}]}}",
            hex(cont),
            group.0.iter().map(|h| format!("\"{}\"", hex(h))).collect::<Vec<_>>().join(","),
            data.iter().map(|h| format!("\"{}\"", hex(h))).collect::<Vec<_>>().join(",")
        ),
    };
    format!("[{}]", v.iter().map(one).collect::<Vec<_>>().join(","))
}

fn hex(h: &campf1r3_core::Hash32) -> String {
    h.0.iter().map(|b| format!("{b:02x}")).collect()
}

/// The executive's success shape, so a client has one reply path.
pub fn ok(body: &str) -> String {
    format!("{{\"ok\":true,\"result\":{body}}}")
}

/// The executive's failure shape, likewise.
pub fn err(code: &str, message: &str) -> String {
    format!(
        "{{\"ok\":false,\"diags\":[{{\"lo\":0,\"hi\":0,\"code\":\"{}\",\"message\":\"{}\"}}]}}",
        esc(code),
        esc(message)
    )
}

fn store_error(e: campf1r3_core::SpaceError) -> String {
    err("store", &format!("{e:?}"))
}
