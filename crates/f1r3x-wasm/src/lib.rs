//! `f1r3x-wasm` — a rho executive inside a browser page.
//!
//! There is no server here and no network in the loop. The executive is
//! compiled into the page and the "transport" is a function call: the page
//! hands in a request and gets a reply, and calls [`f1r3x_poll`] from its own
//! animation frame to collect whatever the session has to say. A visualiser
//! in JavaScript then receives exactly the frames a headset receives over a
//! socket, with identical scene trees and identical fingerprints, so the two
//! can be run against the same term and compared.
//!
//! ## Why this is possible at all
//!
//! Only because `f1r3x-host` has no thread and no clock. The pacing state
//! machine that a native host drives from a parked thread is driven here from
//! `requestAnimationFrame`, and the time it is given is `performance.now()`.
//! Nothing else differs — not the vocabulary, not the frames, not the
//! admission rule. The host is built `default-features = false`, so
//! `Instant::now`, which panics on `wasm32-unknown-unknown`, is not merely
//! unused but unreachable.
//!
//! ## Shape
//!
//! The same shape `f1r3x-ffi` uses, and for the same reason: a wasm export
//! cannot return a string. Every call returns a small integer, and the caller
//! pulls the bytes out afterwards. Sessions live behind `u32` handles, so no
//! Rust pointer crosses the wall.
//!
//! ```text
//!   let h   = f1r3x_new(1);
//!   let buf = f1r3x_alloc(len);            // write the request into memory
//!   let r   = f1r3x_request(h, buf, len);  // -> response id
//!   let n   = f1r3x_take_len(r);
//!   f1r3x_take_read(r, out, n);            // copies and releases
//! ```
//!
//! `f1r3x_poll` is the same, except that its buffer holds a JSON *array* of
//! frames — possibly empty, which is the common case and costs two bytes.
//!
//! ## Unsafe
//!
//! Everything below the registry is safe Rust and is what the tests exercise.
//! The `unsafe` is confined to the four exported functions that copy bytes to
//! or from a caller's buffer, each of which only touches a length the caller
//! supplied.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use f1r3x_host::{proto, Config, Session, WIRE_VERSION};

// ---------------------------------------------------------------------------
// The safe core

#[derive(Default)]
struct Registry {
    sessions: BTreeMap<u32, Session>,
    bufs: BTreeMap<u32, Vec<u8>>,
    next_session: u32,
    next_buf: u32,
}

fn reg() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry::default()))
}

fn with<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut g = reg().lock().unwrap_or_else(|p| p.into_inner());
    f(&mut g)
}

fn park(body: Vec<u8>) -> u32 {
    with(|r| {
        r.next_buf += 1;
        let id = r.next_buf;
        r.bufs.insert(id, body);
        id
    })
}

/// Create a session. `cores_max` is the page's own answer — a browser has no
/// business claiming the machine's core count, and `navigator.hardwareConcurrency`
/// is the caller's to consult if it wants to.
pub fn create(cores_max: u32) -> u32 {
    with(|r| {
        r.next_session += 1;
        let h = r.next_session;
        r.sessions.insert(
            h,
            Session::new(Config {
                cores_max: cores_max.max(1),
                ..Config::default()
            }),
        );
        h
    })
}

pub fn destroy(handle: u32) -> bool {
    with(|r| r.sessions.remove(&handle).is_some())
}

/// Run one request; the reply is parked for the caller to read.
pub fn request(handle: u32, req: &str) -> u32 {
    let body = with(|r| match r.sessions.get_mut(&handle) {
        None => no_such_handle(),
        Some(s) => {
            let (frame, after) = s.handle(req);
            if matches!(after, proto::After::Close) {
                r.sessions.remove(&handle);
            }
            frame.json
        }
    });
    park(body.into_bytes())
}

/// One turn of the page's loop. The frames come back as a JSON array, in
/// order; an empty array means nothing happened, which is most turns.
pub fn poll(handle: u32, now_ms: u64) -> u32 {
    let body = with(|r| match r.sessions.get_mut(&handle) {
        None => no_such_handle(),
        Some(s) => {
            let frames = s.poll(now_ms);
            let mut out = String::from("[");
            for (i, f) in frames.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&f.json);
            }
            out.push(']');
            out
        }
    });
    park(body.into_bytes())
}

/// Greet a session, exactly as a networked host does, so a page and a socket
/// client take the same first frame.
pub fn ready(handle: u32, name: &str) -> u32 {
    let body = with(|r| match r.sessions.get_mut(&handle) {
        None => no_such_handle(),
        // A page is never resuming: there was no earlier connection to resume
        // from, and saying otherwise would be a lie a client acts on.
        Some(s) => s.ready(name, false).json,
    });
    park(body.into_bytes())
}

/// The scene as it stands, without advancing anything.
pub fn scene(handle: u32) -> u32 {
    let body = with(|r| match r.sessions.get(&handle) {
        None => no_such_handle(),
        Some(s) => s.scene_frame().json,
    });
    park(body.into_bytes())
}

pub fn take_len(id: u32) -> usize {
    with(|r| r.bufs.get(&id).map(|b| b.len()).unwrap_or(0))
}

pub fn take(id: u32) -> Option<Vec<u8>> {
    with(|r| r.bufs.remove(&id))
}

pub fn release(id: u32) -> bool {
    with(|r| r.bufs.remove(&id).is_some())
}

fn no_such_handle() -> String {
    proto::err("no-such-handle", "no session with that handle")
}

// ---------------------------------------------------------------------------
// The exports

/// Protocol version, for a page to check before anything else.
#[no_mangle]
pub extern "C" fn f1r3x_version() -> u32 {
    WIRE_VERSION
}

#[no_mangle]
pub extern "C" fn f1r3x_new(cores_max: u32) -> u32 {
    create(cores_max)
}

#[no_mangle]
pub extern "C" fn f1r3x_free(handle: u32) -> u32 {
    destroy(handle) as u32
}

/// # Safety
/// `name_ptr` must point to `name_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn f1r3x_ready(handle: u32, name_ptr: *const u8, name_len: usize) -> u32 {
    let name = read_str(name_ptr, name_len).unwrap_or_else(|| "page".to_string());
    ready(handle, &name)
}

#[no_mangle]
pub extern "C" fn f1r3x_scene(handle: u32) -> u32 {
    scene(handle)
}

/// Advance the session and collect its frames. `now_ms` is the page's clock —
/// `performance.now()` rounded — because this crate has none.
#[no_mangle]
pub extern "C" fn f1r3x_poll(handle: u32, now_ms: u64) -> u32 {
    poll(handle, now_ms)
}

#[no_mangle]
pub extern "C" fn f1r3x_take_len(id: u32) -> usize {
    take_len(id)
}

#[no_mangle]
pub extern "C" fn f1r3x_release(id: u32) -> u32 {
    release(id) as u32
}

/// Allocate `len` bytes for the page to write a request into. A page has no
/// other way to hand over a string.
///
/// # Safety
/// The pointer must be returned to `f1r3x_dealloc` with the same length, or
/// passed to `f1r3x_request` and then freed.
#[no_mangle]
pub extern "C" fn f1r3x_alloc(len: usize) -> *mut u8 {
    let mut v: Vec<u8> = Vec::with_capacity(len);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `ptr` must come from `f1r3x_alloc` with this same `len`, and must not have
/// been freed already.
#[no_mangle]
pub unsafe extern "C" fn f1r3x_dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(Vec::from_raw_parts(ptr, 0, len));
    }
}

/// Run a request. `ptr`/`len` are UTF-8 bytes.
///
/// # Safety
/// `ptr` must point to `len` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn f1r3x_request(handle: u32, ptr: *const u8, len: usize) -> u32 {
    match read_str(ptr, len) {
        Some(s) => request(handle, &s),
        None => park(
            proto::err("not-utf8", "the request was not valid UTF-8").into_bytes(),
        ),
    }
}

/// Copy a parked buffer into the caller's memory and release it. Returns the
/// bytes written, or 0 if `cap` was too small — in which case the buffer is
/// left in place, so the caller can ask for its length again and retry.
///
/// # Safety
/// `out` must point to `cap` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn f1r3x_take_read(id: u32, out: *mut u8, cap: usize) -> usize {
    if out.is_null() {
        return 0;
    }
    let n = take_len(id);
    if n == 0 || n > cap {
        return 0;
    }
    match take(id) {
        None => 0,
        Some(v) => {
            std::ptr::copy_nonoverlapping(v.as_ptr(), out, v.len());
            v.len()
        }
    }
}

/// # Safety
/// `ptr` must point to `len` readable bytes.
unsafe fn read_str(ptr: *const u8, len: usize) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    std::str::from_utf8(std::slice::from_raw_parts(ptr, len))
        .ok()
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page's whole lifecycle, driven through the safe core. This runs on
    /// the host machine, which is the point: the wasm target needs a browser
    /// to exercise, and everything except the byte-copying is target
    /// independent.
    #[test]
    fn a_page_can_deploy_run_and_read_frames() {
        let h = create(1);
        assert!(h != 0);

        let r = ready(h, "page-1");
        let greeting = String::from_utf8(take(r).unwrap()).unwrap();
        assert!(greeting.contains("\"t\":\"ready\""), "{greeting}");
        assert!(greeting.contains("\"session\":\"page-1\""), "{greeting}");

        let r = request(h, "deploy\nfor(y <- @{Nil}){ *y } | @{Nil}!(@{Nil}!(Nil))");
        let reply = String::from_utf8(take(r).unwrap()).unwrap();
        assert!(reply.contains("\"ok\":true"), "{reply}");

        let _ = take(request(h, "play"));

        // The page's loop: poll, draw, acknowledge.
        let mut rounds = 0;
        for t in 0..200u64 {
            let id = poll(h, t * 16);
            let frames = String::from_utf8(take(id).unwrap()).unwrap();
            if frames.contains("\"t\":\"round\"") {
                rounds += 1;
                let _ = take(request(h, &format!("ack {rounds}")));
            }
        }
        assert!(rounds >= 3, "the page only saw {rounds} rounds");

        let s = String::from_utf8(take(scene(h)).unwrap()).unwrap();
        assert!(s.contains("\"tree\":"), "a scene must carry the shapes");

        assert!(destroy(h));
        assert!(!destroy(h));
    }

    #[test]
    fn an_empty_poll_is_an_empty_array_and_not_an_error() {
        let h = create(1);
        let id = poll(h, 0);
        let frames = String::from_utf8(take(id).unwrap()).unwrap();
        // Paused and with nothing deployed, the only thing that can fall out
        // is the stats cadence; whatever happens it must be a JSON array.
        assert!(frames.starts_with('['), "{frames}");
        assert!(frames.ends_with(']'), "{frames}");
        destroy(h);
    }

    #[test]
    fn a_call_on_a_dead_handle_is_an_error_not_a_panic() {
        let body = String::from_utf8(take(request(9999, "status")).unwrap()).unwrap();
        assert!(body.contains("no-such-handle"), "{body}");
    }

    #[test]
    fn bye_destroys_the_session() {
        let h = create(1);
        let _ = take(request(h, "bye"));
        let body = String::from_utf8(take(request(h, "status")).unwrap()).unwrap();
        assert!(body.contains("no-such-handle"), "{body}");
    }
}
