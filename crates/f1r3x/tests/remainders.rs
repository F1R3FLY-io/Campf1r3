//! Remainder patterns for lists and maps: `[a, b ...rest]`, `{"k": v ...rest}`.
//!
//! A remainder names what a pattern does not: a list pattern matches a prefix
//! and binds the suffix; a map pattern matches the keys it names and binds the
//! map of the others. Without `...`, patterns keep their meaning: same length,
//! same key set.

use b3ll0ws::{ApertureId, Machine};
use campf1r3_core::Config;
use k1ndl1ng_norm::{normalise, CollKind, Name, Node, Norm, Options as NOpts};
use k1ndl1ng_parse::{parse, Level, Options as POpts};

fn norm_at(src: &str) -> Result<(Norm, Vec<String>), Vec<String>> {
    let p = parse(src, &POpts { level: Level::K1G, ..POpts::default() });
    if !p.ok() {
        return Err(p.diags.iter().map(|d| d.code.to_string()).collect());
    }
    let n = normalise(&p.tree, &NOpts::default()).map_err(|d| d.iter().map(|d| d.code.to_string()).collect::<Vec<_>>())?;
    Ok((n.term, n.free.iter().map(|f| f.ident.clone()).collect()))
}

fn norm(src: &str) -> Norm {
    norm_at(src).unwrap_or_else(|e| panic!("{src}: {e:?}")).0
}

/// Run `src` with its free `out` bound to an aperture; return what was sent.
fn run(src: &str) -> Vec<Vec<Norm>> {
    let (t, free) = norm_at(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    let out = [9u8; 32];
    let keys: Vec<[u8; 32]> = free.iter().map(|f| if f == "out" { out } else { [0xEE; 32] }).collect();
    let mut m: Machine = Machine::new(Config::default());
    m.bind_aperture(Name::Unforgeable(out), ApertureId(1));
    m.spawn(t.ground_free(&keys));
    m.run(10_000).unwrap();
    m.take_outbound().into_iter().map(|o| o.args).collect()
}

fn val(src: &str) -> Norm {
    let Node::Send { args, .. } = norm(&format!("c!({src})")).node().clone() else { panic!() };
    args[0].clone()
}

const CLICK: &str = r#"{"type": "click", "x": 120, "y": 48, "button": 0, "mods": [], "detail": 1}"#;

// --- syntax -----------------------------------------------------------------

#[test]
fn both_spellings_mean_the_same_pattern() {
    let a = norm(r#"for (@{"x": x ...r} <- c) { Nil }"#);
    let b = norm(r#"for (@{"x": x, ...r} <- c) { Nil }"#);
    assert_eq!(a.hash(), b.hash());
    let a = norm(r#"for (@[h ...t] <- c) { Nil }"#);
    let b = norm(r#"for (@[h, ...t] <- c) { Nil }"#);
    assert_eq!(a.hash(), b.hash());
}

#[test]
fn remainder_patterns_print_encode_and_decode() {
    for src in [
        r#"for (@{"x": x, "y": y ..._} <- c) { out!(x, y) }"#,
        r#"for (@{...r} <- c) { out!(r) }"#,
        r#"for (@[first, second ...tail] <- c) { out!(tail) }"#,
        r#"for (@[...all] <- c) { out!(all) }"#,
        r#"for (@{"a": x ...{"b": y}} <- c) { out!(x, y) }"#,
    ] {
        let t = norm(src);
        let shown = k1ndl1ng_norm::show(&t);
        assert_eq!(norm(&shown).hash(), t.hash(), "{src} printed as {shown}");
        let back = Norm::decode(t.encode()).expect("decodes");
        assert_eq!(back.hash(), t.hash(), "{src}");
    }
    // New tags; the encodings of plain collections are unchanged.
    let Node::Receive { binds, .. } = norm(r#"for (@{"x": x ...r} <- c) { Nil }"#).node().clone() else { panic!() };
    let Name::Quote(p) = &binds[0].pats[0] else { panic!() };
    assert_eq!(p.encode()[0], 0x15);
    let Node::Receive { binds, .. } = norm(r#"for (@[h ...t] <- c) { Nil }"#).node().clone() else { panic!() };
    let Name::Quote(p) = &binds[0].pats[0] else { panic!() };
    assert_eq!(p.encode()[0], 0x14);
    assert_eq!(val(r#"{"a": 1}"#).encode()[0], 0x0F);
    assert_eq!(val("[1]").encode()[0], 0x0D);
}

#[test]
fn misuse_is_diagnosed() {
    let code = |src: &str| norm_at(src).unwrap_err().join(",");
    // A remainder is a pattern: it has no meaning as data.
    assert!(code(r#"c!({"a": 1 ...{}})"#).contains("remainder-outside-pattern"));
    // Tuples have a fixed arity.
    assert!(code(r#"for (@(a ...r) <- c) { Nil }"#).contains("remainder-place"));
    assert!(code(r#"for (@(a, ...r) <- c) { Nil }"#).contains("remainder-place"));
    // A key with a hole: matching would depend on stored order. This also
    // closes the old misalignment for plain map patterns.
    assert!(code(r#"for (@{k: 1 ..._} <- c) { Nil }"#).contains("map-key-pattern"));
    assert!(code(r#"for (@{k: 1} <- c) { Nil }"#).contains("map-key-pattern"));
    assert!(code(r#"for (@{_: v} <- c) { Nil }"#).contains("map-key-pattern"));
    // Remainders are K1G syntax.
    let p = parse(r#"for (@{...r} <- c) { Nil }"#, &POpts { level: Level::K1, ..POpts::default() });
    assert!(!p.ok());
}

// --- matching -----------------------------------------------------------------

#[test]
fn a_map_pattern_names_only_what_it_needs() {
    let got = run(&format!(r#"new c in {{ c!({CLICK}) | for (@{{"x": x, "y": y ..._}} <- c) {{ out!(x, y) }} }}"#));
    assert_eq!(got.len(), 1);
    assert_eq!((got[0][0].as_int(), got[0][1].as_int()), (Some(120), Some(48)));
}

#[test]
fn the_remainder_is_the_map_of_the_other_keys_in_normal_form() {
    let got = run(&format!(r#"new c in {{ c!({CLICK}) | for (@{{"type": "click", "x": _ ...rest}} <- c) {{ out!(rest) }} }}"#));
    assert_eq!(got.len(), 1);
    let expect = val(r#"{"y": 48, "button": 0, "mods": [], "detail": 1}"#);
    assert_eq!(got[0][0].hash(), expect.hash(), "the remainder is canonical, so it hashes like a literal");
}

#[test]
fn without_a_remainder_the_key_sets_must_be_equal() {
    // Exact patterns keep their meaning.
    assert!(run(&format!(r#"new c in {{ c!({CLICK}) | for (@{{"x": x, "y": y}} <- c) {{ out!(x) }} }}"#)).is_empty());
    assert_eq!(run(r#"new c in { c!({"y": 2, "x": 1}) | for (@{"x": x, "y": y} <- c) { out!(x, y) } }"#).len(), 1);
}

#[test]
fn a_missing_key_or_a_wrong_value_does_not_match() {
    assert!(run(&format!(r#"new c in {{ c!({CLICK}) | for (@{{"key": k ..._}} <- c) {{ out!(k) }} }}"#)).is_empty());
    assert!(run(&format!(r#"new c in {{ c!({CLICK}) | for (@{{"type": "keydown" ..._}} <- c) {{ out!(1) }} }}"#)).is_empty());
    // Not a map at all.
    assert!(run(r#"new c in { c!([1, 2]) | for (@{...r} <- c) { out!(r) } }"#).is_empty());
}

#[test]
fn edge_cases() {
    // `{...r}` takes the whole map, including the empty one.
    let got = run(r#"new c in { c!({}) | for (@{...r} <- c) { out!(r) } }"#);
    assert_eq!(got[0][0].hash(), val("{}").hash());
    // A remainder may itself be a pattern: here, "exactly one more key, b".
    let src = |m: &str| format!(r#"new c in {{ c!({m}) | for (@{{"a": x ...{{"b": y}}}} <- c) {{ out!(x, y) }} }}"#);
    assert_eq!(run(&src(r#"{"a": 1, "b": 2}"#)).len(), 1);
    assert!(run(&src(r#"{"a": 1, "b": 2, "c": 3}"#)).is_empty());
    // A pattern with no holes at all still matches structurally.
    assert_eq!(run(r#"new c in { c!({"a": 1}) | for (@{"a": 1 ...{}} <- c) { out!(true) } }"#).len(), 1);
    assert!(run(r#"new c in { c!({"a": 1, "z": 0}) | for (@{"a": 1 ...{}} <- c) { out!(true) } }"#).is_empty());
}

#[test]
fn list_patterns_take_a_prefix_and_bind_the_suffix() {
    let got = run(r#"new c in { c!([1, 2, 3]) | for (@[h ...t] <- c) { out!(h, t) } }"#);
    assert_eq!(got[0][0].as_int(), Some(1));
    assert_eq!(got[0][1].hash(), val("[2, 3]").hash());
    assert!(run(r#"new c in { c!([1]) | for (@[a, b ...t] <- c) { out!(t) } }"#).is_empty());
    let got = run(r#"new c in { c!([]) | for (@[...t] <- c) { out!(t) } }"#);
    assert_eq!(got[0][0].hash(), val("[]").hash());
    // A tuple is not a list.
    assert!(run(r#"new c in { c!((1, 2)) | for (@[...t] <- c) { out!(t) } }"#).is_empty());
}

#[test]
fn a_name_in_an_event_is_bound_and_usable() {
    // The browser's case: the target element arrives as a name inside the
    // event map; the page binds it and sends on it.
    let got = run(
        r#"new c, el in {
             c!({"type": "click", "target": *el, "x": 1, "y": 2}) |
             for (@{"target": *t ..._} <- c) { t!("classAdd", "lit") } |
             for (@verb, @cls <- el) { out!(verb, cls) }
           }"#,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(got[0][0].as_str(), Some("classAdd"));
}

#[test]
fn persistent_listeners_see_every_event() {
    let got = run(
        r#"new c in {
             c!({"type": "click", "x": 1}) | c!({"type": "click", "x": 2, "extra": true}) |
             for (@{"x": x ..._} <= c) { out!(x) }
           }"#,
    );
    let mut xs: Vec<i64> = got.iter().filter_map(|a| a[0].as_int()).collect();
    xs.sort();
    assert_eq!(xs, vec![1, 2]);
    let _ = CollKind::Map;
}
