//! `f1r3x-ffi` — the C ABI.
//!
//! One boundary serves both clients the Rhobots work needs: Swift on visionOS,
//! and `wasm32-unknown-unknown` with no `wasm-bindgen`. That second target is
//! why the shape is what it is. A wasm export cannot return a string, so every
//! call returns a small integer and the caller pulls the bytes out afterwards;
//! and because the K1ndl1ng spec forbids stub wasm builds, this crate compiles
//! to the same code on every target rather than growing a `#[cfg]` fork.
//!
//! ## Protocol
//!
//! Executives live behind `u32` handles in a registry, so no Rust pointer ever
//! crosses the boundary. Requests are a one-line command followed, where the
//! command takes one, by a payload:
//!
//! ```text
//! deploy\n<rho source>     submit a program
//! tick                     one tick of the control panel
//! run                      run to quiescence
//! scene                    the current scene
//! trace                    the steps since the last drain
//! log                      the store's event log (draining)
//! cores <n> [max]          set the cores knob
//! mode lockstep|free       set the scheduler mode
//! status                   counters and settings
//! show                     the normal form of the last deploy
//! ```
//!
//! Every response is JSON: `{"ok":true,"result":…}` or
//! `{"ok":false,"diags":[…]}`. Nothing here parses JSON — the request side is
//! a line protocol precisely so that no parser is needed on this side of the
//! wall.
//!
//! ## Unsafe
//!
//! The rest of the workspace forbids `unsafe`. This crate cannot: copying
//! bytes to a caller's buffer is the whole job. It is confined to the four
//! functions at the bottom, each of which only reads or writes a caller
//! buffer of a length the caller supplied. Everything above them, including
//! the whole protocol, is safe Rust and is what the tests exercise.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use f1r3x::{json, Executive, Mode, Settings};

/// Bumped when the protocol changes in a way a client must notice.
pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// The safe core: everything the tests drive.

/// Run one request against one executive.
pub fn dispatch(x: &mut Executive, req: &str) -> String {
    let (head, body) = match req.find('\n') {
        Some(i) => (&req[..i], &req[i + 1..]),
        None => (req, ""),
    };
    let head = head.trim();
    let mut words = head.split_whitespace();
    let cmd = words.next().unwrap_or("");
    let args: Vec<&str> = words.collect();

    match cmd {
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
            json::ok(&status(x))
        }
        "mode" => {
            let m = match args.first().copied() {
                Some("free") | Some("free-running") => Mode::FreeRunning,
                _ => Mode::Lockstep,
            };
            x.set_mode(m);
            json::ok(&status(x))
        }
        "status" => json::ok(&status(x)),
        "show" => match x.deploys().last() {
            None => bad("no-deploy", "nothing has been deployed yet"),
            Some(d) => json::ok(&format!(
                "{{\"source\":\"{}\",\"normal\":\"{}\",\"hash\":\"{}\"}}",
                json::esc(&d.source),
                json::esc(&f1r3x::show(&d.term)),
                d.term.hash().hex()
            )),
        },
        "" => bad("empty", "no command"),
        other => bad("unknown-command", &format!("unknown command `{other}`")),
    }
}

fn status(x: &Executive) -> String {
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
        PROTOCOL_VERSION
    )
}

fn events(v: &[campf1r3_core::Event]) -> String {
    let one = |e: &campf1r3_core::Event| match e {
        campf1r3_core::Event::Produce {
            chan,
            datum,
            persist,
        } => format!(
            "{{\"kind\":\"produce\",\"chan\":\"{}\",\"datum\":\"{}\",\"persist\":{persist}}}",
            hex(chan),
            hex(datum)
        ),
        campf1r3_core::Event::Consume {
            group,
            cont,
            persist,
            installing,
        } => format!(
            "{{\"kind\":\"consume\",\"cont\":\"{}\",\"group\":[{}],\"persist\":{persist},\"installing\":{installing}}}",
            hex(cont),
            group
                .0
                .iter()
                .map(|h| format!("\"{}\"", hex(h)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        campf1r3_core::Event::Comm { group, cont, data } => format!(
            "{{\"kind\":\"comm\",\"cont\":\"{}\",\"group\":[{}],\"data\":[{}]}}",
            hex(cont),
            group
                .0
                .iter()
                .map(|h| format!("\"{}\"", hex(h)))
                .collect::<Vec<_>>()
                .join(","),
            data.iter()
                .map(|h| format!("\"{}\"", hex(h)))
                .collect::<Vec<_>>()
                .join(",")
        ),
    };
    format!("[{}]", v.iter().map(one).collect::<Vec<_>>().join(","))
}

fn hex(h: &campf1r3_core::Hash32) -> String {
    h.0.iter().map(|b| format!("{b:02x}")).collect()
}

fn bad(code: &str, msg: &str) -> String {
    format!(
        "{{\"ok\":false,\"diags\":[{{\"lo\":0,\"hi\":0,\"code\":\"{}\",\"message\":\"{}\"}}]}}",
        json::esc(code),
        json::esc(msg)
    )
}

fn store_error(e: campf1r3_core::SpaceError) -> String {
    bad("store", &format!("{e:?}"))
}

// ---------------------------------------------------------------------------
// The registry. Handles, not pointers.

#[derive(Default)]
struct Registry {
    execs: BTreeMap<u32, Executive>,
    bufs: BTreeMap<u32, Vec<u8>>,
    next_exec: u32,
    next_buf: u32,
}

fn reg() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry::default()))
}

fn with_reg<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut g = reg().lock().unwrap_or_else(|p| p.into_inner());
    f(&mut g)
}

/// Create an executive; returns its handle. Handle 0 is never issued, so a
/// client can use it as "none".
pub fn create() -> u32 {
    with_reg(|r| {
        r.next_exec += 1;
        let h = r.next_exec;
        r.execs.insert(h, Executive::new(Settings::default()));
        h
    })
}

pub fn destroy(handle: u32) -> bool {
    with_reg(|r| r.execs.remove(&handle).is_some())
}

/// Run a request and park the response in the registry; returns a buffer id.
pub fn call(handle: u32, req: &str) -> u32 {
    let body = with_reg(|r| match r.execs.get_mut(&handle) {
        None => bad("no-such-handle", "no executive with that handle"),
        Some(x) => dispatch(x, req),
    });
    with_reg(|r| {
        r.next_buf += 1;
        let id = r.next_buf;
        r.bufs.insert(id, body.into_bytes());
        id
    })
}

pub fn response_len(id: u32) -> usize {
    with_reg(|r| r.bufs.get(&id).map(|b| b.len()).unwrap_or(0))
}

pub fn response_take(id: u32) -> Option<Vec<u8>> {
    with_reg(|r| r.bufs.remove(&id))
}

pub fn release(id: u32) -> bool {
    with_reg(|r| r.bufs.remove(&id).is_some())
}

// ---------------------------------------------------------------------------
// The C ABI. All the `unsafe` in the workspace lives here.

/// Protocol version, for a client to check before anything else.
#[no_mangle]
pub extern "C" fn f1r3x_version() -> u32 {
    PROTOCOL_VERSION
}

/// Create an executive. Returns a handle, never 0.
#[no_mangle]
pub extern "C" fn f1r3x_new() -> u32 {
    create()
}

/// Destroy an executive. Returns 1 if it existed.
#[no_mangle]
pub extern "C" fn f1r3x_free(handle: u32) -> u32 {
    destroy(handle) as u32
}

/// Bytes the response occupies, so the caller can size a buffer.
#[no_mangle]
pub extern "C" fn f1r3x_response_len(id: u32) -> usize {
    response_len(id)
}

/// Discard a response the caller decided not to read.
#[no_mangle]
pub extern "C" fn f1r3x_response_release(id: u32) -> u32 {
    release(id) as u32
}

/// Allocate `len` bytes for the caller to write a request into. wasm has no
/// other way to hand us a string.
///
/// # Safety
/// The returned pointer must be given back to `f1r3x_dealloc` with the same
/// length, or passed to `f1r3x_call` and then freed.
#[no_mangle]
pub extern "C" fn f1r3x_alloc(len: usize) -> *mut u8 {
    let mut v: Vec<u8> = Vec::with_capacity(len);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Release a buffer from `f1r3x_alloc`.
///
/// # Safety
/// `ptr` must come from `f1r3x_alloc` with this same `len`, and must not have
/// been freed already.
#[no_mangle]
pub unsafe extern "C" fn f1r3x_dealloc(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    drop(Vec::from_raw_parts(ptr, 0, len));
}

/// Run a request. `ptr`/`len` are UTF-8 bytes; the return value is a response
/// id to be read with `f1r3x_response_read` and then released.
///
/// # Safety
/// `ptr` must point to `len` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn f1r3x_call(handle: u32, ptr: *const u8, len: usize) -> u32 {
    if ptr.is_null() {
        return call(handle, "");
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    match std::str::from_utf8(bytes) {
        Ok(s) => call(handle, s),
        Err(_) => {
            let body = bad("not-utf8", "the request was not valid UTF-8");
            with_reg(|r| {
                r.next_buf += 1;
                let id = r.next_buf;
                r.bufs.insert(id, body.into_bytes());
                id
            })
        }
    }
}

/// Copy a response into the caller's buffer and release it. Returns the number
/// of bytes written, or 0 if the buffer was too small — in which case the
/// response is left in place so the caller can retry with a bigger one.
///
/// # Safety
/// `out` must point to `cap` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn f1r3x_response_read(id: u32, out: *mut u8, cap: usize) -> usize {
    if out.is_null() {
        return 0;
    }
    let n = response_len(id);
    if n == 0 || n > cap {
        return 0;
    }
    match response_take(id) {
        None => 0,
        Some(v) => {
            std::ptr::copy_nonoverlapping(v.as_ptr(), out, v.len());
            v.len()
        }
    }
}

#[cfg(test)]
mod tests;
