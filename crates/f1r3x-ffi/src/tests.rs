//! Tests for the boundary. Everything here goes through `dispatch` or the
//! handle API, which is the same code the C ABI calls — the ABI itself adds
//! only the byte copy.

use super::*;

fn fresh() -> Executive {
    Executive::new(Settings::default())
}

/// Crude field lookup, good enough to check a response without a JSON parser.
fn field(js: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":");
    let i = js.find(&pat)? + pat.len();
    let rest = &js[i..];
    if let Some(stripped) = rest.strip_prefix('"') {
        let j = stripped.find('"')?;
        return Some(stripped[..j].to_string());
    }
    let j = rest
        .find(|c: char| c == ',' || c == '}' || c == ']')
        .unwrap_or(rest.len());
    Some(rest[..j].to_string())
}

#[test]
fn a_deploy_is_accepted_and_numbered() {
    let mut x = fresh();
    let r = dispatch(&mut x, "deploy\nnew a in { a!(Nil) | for(y <- a){ Nil } }");
    assert_eq!(field(&r, "ok").as_deref(), Some("true"));
    assert_eq!(field(&r, "deploy").as_deref(), Some("0"));
}

#[test]
fn a_bad_deploy_comes_back_as_diagnostics() {
    let mut x = fresh();
    let r = dispatch(&mut x, "deploy\nfor(x <- ) {");
    assert_eq!(field(&r, "ok").as_deref(), Some("false"));
    assert!(r.contains("\"diags\""));
    assert!(r.contains("\"code\""));
}

#[test]
fn run_reports_the_steps_and_quiescence() {
    let mut x = fresh();
    dispatch(&mut x, "deploy\nnew a in { a!(Nil) | for(y <- a){ Nil } }");
    let r = dispatch(&mut x, "run");
    assert_eq!(field(&r, "ok").as_deref(), Some("true"));
    assert!(r.contains("\"kind\":\"comm\""));
    assert_eq!(field(&r, "quiescent").as_deref(), Some("true"));
}

#[test]
fn tick_advances_only_the_cores_setting() {
    let mut x = fresh();
    dispatch(&mut x, "cores 1 64");
    dispatch(&mut x, "deploy\nnew a, b, c in { a!(Nil) | b!(Nil) | c!(Nil) }");
    let r = dispatch(&mut x, "tick");
    // The first tick only opens the `new`.
    assert!(r.contains("\"kind\":\"mint\""));
    assert_eq!(field(&r, "quiescent").as_deref(), Some("false"));
    let s = dispatch(&mut x, "status");
    assert_eq!(field(&s, "queued").as_deref(), Some("3"));
}

#[test]
fn the_knobs_are_settable_and_reported() {
    let mut x = fresh();
    let r = dispatch(&mut x, "cores 8 64");
    assert_eq!(field(&r, "cores").as_deref(), Some("8"));
    let r = dispatch(&mut x, "cores 999 16");
    assert_eq!(field(&r, "cores").as_deref(), Some("16"));
    let r = dispatch(&mut x, "mode free");
    assert_eq!(field(&r, "mode").as_deref(), Some("free-running"));
    let r = dispatch(&mut x, "mode lockstep");
    assert_eq!(field(&r, "mode").as_deref(), Some("lockstep"));
}

#[test]
fn the_scene_crosses_the_boundary() {
    let mut x = fresh();
    dispatch(&mut x, "deploy\nnew a, b in { a!(Nil) | for(y <- b){ Nil } }");
    dispatch(&mut x, "run");
    let r = dispatch(&mut x, "scene");
    assert!(r.contains("\"tuples\":[{"));
    assert!(r.contains("\"listeners\":[{"));
    assert!(r.contains("\"kind\":\"tuple\""));
    assert!(r.contains("\"kind\":\"listener\""));
}

#[test]
fn the_log_crosses_the_boundary_and_drains() {
    let mut x = fresh();
    dispatch(&mut x, "deploy\nnew a in { a!(Nil) | for(y <- a){ Nil } }");
    dispatch(&mut x, "run");
    let r = dispatch(&mut x, "log");
    assert!(r.contains("\"kind\":\"comm\""));
    let r2 = dispatch(&mut x, "log");
    assert!(r2.contains("\"events\":[]"));
}

#[test]
fn show_returns_the_normal_form_and_its_hash() {
    let mut x = fresh();
    dispatch(&mut x, "deploy\nNil | c!(Nil)");
    let r = dispatch(&mut x, "show");
    assert_eq!(field(&r, "ok").as_deref(), Some("true"));
    // `Nil |` was absorbed by the unit law before it ever reached the store.
    let normal = field(&r, "normal").unwrap();
    assert!(!normal.contains("Nil |"), "got {normal}");
    assert_eq!(field(&r, "hash").unwrap().len(), 64);
}

#[test]
fn show_with_nothing_deployed_is_an_error_not_a_panic() {
    let mut x = fresh();
    let r = dispatch(&mut x, "show");
    assert_eq!(field(&r, "ok").as_deref(), Some("false"));
    assert!(r.contains("no-deploy"));
}

#[test]
fn an_unknown_command_is_an_error_not_a_panic() {
    let mut x = fresh();
    for req in ["", "   ", "wobble", "deploy", "cores", "mode"] {
        let r = dispatch(&mut x, req);
        assert!(r.starts_with("{\"ok\":"), "req {req:?} gave {r}");
    }
}

#[test]
fn a_source_with_quotes_and_newlines_survives_the_round_trip() {
    let mut x = fresh();
    // The payload is everything after the first newline, so a multi-line
    // program arrives intact.
    let src = "deploy\nnew a in {\n  a!(Nil)\n| for(y <- a){ Nil }\n}";
    let r = dispatch(&mut x, src);
    assert_eq!(field(&r, "ok").as_deref(), Some("true"));
    let r = dispatch(&mut x, "run");
    assert!(r.contains("\"kind\":\"comm\""));
    // And the source comes back escaped.
    let r = dispatch(&mut x, "show");
    assert!(r.contains("\\n"));
}

#[test]
fn handles_are_independent() {
    let a = create();
    let b = create();
    assert_ne!(a, b);
    assert_ne!(a, 0);
    let id = call(a, "deploy\nnew x in { x!(Nil) }");
    assert!(response_take(id).is_some());
    call(a, "run");
    let ra = String::from_utf8(response_take(call(a, "status")).unwrap()).unwrap();
    let rb = String::from_utf8(response_take(call(b, "status")).unwrap()).unwrap();
    assert_eq!(field(&ra, "deploys").as_deref(), Some("1"));
    assert_eq!(field(&rb, "deploys").as_deref(), Some("0"));
    assert!(destroy(a));
    assert!(destroy(b));
    assert!(!destroy(a));
}

#[test]
fn a_call_on_a_dead_handle_is_an_error() {
    let h = create();
    destroy(h);
    let id = call(h, "status");
    let body = String::from_utf8(response_take(id).unwrap()).unwrap();
    assert!(body.contains("no-such-handle"));
}

#[test]
fn responses_are_read_once_and_released() {
    let h = create();
    let id = call(h, "status");
    let n = response_len(id);
    assert!(n > 0);
    assert!(response_take(id).is_some());
    assert_eq!(response_len(id), 0);
    assert!(!release(id));
    destroy(h);
}

#[test]
fn the_version_is_reported() {
    assert_eq!(f1r3x_version(), PROTOCOL_VERSION);
}

#[test]
fn a_whole_session_runs_through_the_handle_api() {
    let h = create();
    let say = |req: &str| -> String {
        let id = call(h, req);
        String::from_utf8(response_take(id).unwrap()).unwrap()
    };
    assert!(say("cores 2 64").contains("\"cores\":2"));
    assert!(say("deploy\nnew a, b in { a!(Nil) | for(_ <- a){ b!(Nil) } }").contains("\"ok\":true"));
    let mut ticks = 0;
    loop {
        let r = say("tick");
        ticks += 1;
        if field(&r, "quiescent").as_deref() == Some("true") {
            break;
        }
        assert!(ticks < 50);
    }
    let st = say("status");
    assert_eq!(field(&st, "comms").as_deref(), Some("1"));
    assert_eq!(field(&st, "data").as_deref(), Some("1"));
    assert!(say("scene").contains("\"quiescent\":true"));
    destroy(h);
}
