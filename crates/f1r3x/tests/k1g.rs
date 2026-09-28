//! K1G ground data (U1), apertures and injection (U4), the observer-generic
//! machine (U3) and the keyed minter (U6), as the F1R3Gaze implementation
//! specification asks of this repository.

use b3ll0ws::{ApertureId, Direct, Machine};
use campf1r3_core::{Abort, Config, Observer};
use k1ndl1ng_campf1r3::{BindPat, Chan, Cont, Data};
use k1ndl1ng_norm::{normalise, CollKind, Keyed, Lit, Name, Node, Norm, Options as NOpts};
use k1ndl1ng_parse::{parse, Level, Options as POpts};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

fn norm_at(src: &str, level: Level) -> Result<(Norm, Vec<String>), String> {
    let p = parse(src, &POpts { level, ..POpts::default() });
    if !p.ok() {
        return Err(format!("{:?}", p.diags.iter().map(|d| d.code.to_string()).collect::<Vec<_>>()));
    }
    let n = normalise(&p.tree, &NOpts::default())
        .map_err(|d| format!("{:?}", d.iter().map(|d| d.code.to_string()).collect::<Vec<_>>()))?;
    let free = n.free.iter().map(|f| f.ident.clone()).collect();
    Ok((n.term, free))
}

fn norm(src: &str) -> Norm {
    norm_at(src, Level::K1G).expect("parses at K1G").0
}

#[test]
fn literals_parse_and_encode() {
    let t = norm(r#"c!("a\n\"b\"", -42, true, [1, 2], (1, "x"), {"k": [], "j": {}})"#);
    let Node::Send { args, .. } = t.node() else { panic!("send") };
    assert_eq!(args[0].as_str(), Some("a\n\"b\""));
    assert_eq!(args[1].as_int(), Some(-42));
    assert_eq!(args[2].as_bool(), Some(true));
    assert_eq!(args[3].as_coll(CollKind::List).map(|v| v.len()), Some(2));
    assert_eq!(args[4].as_coll(CollKind::Tuple).map(|v| v.len()), Some(2));
    assert!(args[5].map_get("k").is_some());
    // Round trip through the encoding.
    let back = Norm::decode(t.encode()).expect("decodes");
    assert_eq!(back.hash(), t.hash());
}

#[test]
fn map_order_is_the_key_encoding() {
    let a = norm(r#"c!({"b": 2, "a": 1})"#);
    let b = norm(r#"c!({"a": 1, "b": 2})"#);
    assert_eq!(a.hash(), b.hash());
}

#[test]
fn duplicate_map_keys_are_rejected() {
    let e = norm_at(r#"c!({"a": 1, "a": 2})"#, Level::K1G).unwrap_err();
    assert!(e.contains("duplicate-key"), "{e}");
}

#[test]
fn ground_data_needs_k1g() {
    let e = norm_at(r#"c!("x")"#, Level::K1).unwrap_err();
    assert!(e.contains("K1G") || e.to_lowercase().contains("level") || e.contains("need"), "{e}");
    // Grouping is unchanged at K1.
    assert!(norm_at("c!((Nil))", Level::K1).is_ok());
}

#[test]
fn integers_round_trip_at_the_extremes() {
    for v in [i64::MIN + 1, -1, 0, 1, i64::MAX] {
        let n = Norm::int(v);
        assert_eq!(Norm::decode(n.encode()).unwrap().as_int(), Some(v));
    }
    let n = Norm::int(i64::MIN);
    assert_eq!(Norm::decode(n.encode()).unwrap().as_int(), Some(i64::MIN));
}

#[test]
fn show_round_trips() {
    let t = norm(r#"c!(("ok", [1, "two"], {"x": false}))"#);
    let shown = k1ndl1ng_norm::show(&t);
    let again = norm(&shown);
    assert_eq!(again.hash(), t.hash(), "{shown}");
}

fn ground(t: &Norm, free: &[String], keys: &[(&str, [u8; 32])]) -> Norm {
    let ks: Vec<[u8; 32]> = free
        .iter()
        .map(|f| keys.iter().find(|(n, _)| n == f).map(|(_, k)| *k).unwrap_or([0xEE; 32]))
        .collect();
    t.ground_free(&ks)
}

#[test]
fn tuple_patterns_bind_names_and_data() {
    // The reply convention of the DOM protocol: ("ok", *name).
    let (t, free) = norm_at(
        r#"new r, el in { r!(("ok", *el)) | for (@("ok", *x) <- r) { x!(7) } | for (@v <- el) { out!(v) } }"#,
        Level::K1G,
    )
    .unwrap();
    let out = [9u8; 32];
    let t = ground(&t, &free, &[("out", out)]);
    let mut m: Machine = Machine::new(Config::default());
    m.bind_aperture(Name::Unforgeable(out), ApertureId(1));
    m.spawn(t);
    m.run(10_000).unwrap();
    let ob = m.take_outbound();
    assert_eq!(ob.len(), 1);
    assert_eq!(ob[0].aperture, ApertureId(1));
    assert_eq!(ob[0].args[0].as_int(), Some(7));
}

#[test]
fn apertures_collect_sends_and_inject_answers() {
    let (t, free) = norm_at(
        r##"new ret in { doc!("query1", "#lamp", *ret) | for (@("ok", *lamp) <- ret) { lamp!("classAdd", "lit") } }"##,
        Level::K1G,
    )
    .unwrap();
    let doc = [1u8; 32];
    let t = ground(&t, &free, &[("doc", doc)]);
    let mut m: Machine = Machine::new(Config::default());
    m.bind_aperture(Name::Unforgeable(doc), ApertureId(0));
    m.spawn(t);
    m.run(10_000).unwrap();
    let ob = m.take_outbound();
    assert_eq!(ob.len(), 1);
    assert_eq!(ob[0].args[0].as_str(), Some("query1"));
    // The reply name travels as `*ret`: an Eval of an unforgeable.
    let ret = match ob[0].args[2].node() {
        Node::Eval(n) => n.clone(),
        other => panic!("reply is not a name: {:?}", std::mem::discriminant(other)),
    };
    // The host answers with a fresh element name, which is also an aperture.
    let lamp = [2u8; 32];
    m.bind_aperture(Name::Unforgeable(lamp), ApertureId(7));
    m.inject(
        ret,
        vec![Norm::tuple(vec![Norm::str("ok"), Norm::eval(Name::Unforgeable(lamp))])],
    );
    m.run(10_000).unwrap();
    let ob = m.take_outbound();
    assert_eq!(ob.len(), 1);
    assert_eq!(ob[0].aperture, ApertureId(7));
    assert_eq!(ob[0].args[0].as_str(), Some("classAdd"));
    assert!(matches!(ob[0].args[1].as_lit(), Some(Lit::Str(s)) if &**s == "lit"));
}

#[derive(Clone, Default)]
struct Count(Arc<AtomicU64>);
impl Observer<Chan, BindPat, Data, Cont> for Count {
    fn observe_comm(&self, _k: &Cont, _p: bool, _d: &[(&Data, bool)]) -> Result<(), Abort> {
        if self.0.fetch_add(1, Ordering::SeqCst) >= 3 {
            Err(Abort)
        } else {
            Ok(())
        }
    }
}

#[test]
fn observer_can_meter_and_abort() {
    let (t, _) = norm_at("new c in { c!(Nil) | c!(Nil) | c!(Nil) | c!(Nil) | c!(Nil) | for (_ <= c) { Nil } }", Level::K1).unwrap();
    let meter = Count::default();
    let mut m = Machine::with_observer(Config::default(), Direct::default(), Keyed::new([3; 32]), meter.clone());
    m.spawn(t);
    let r = m.run(10_000);
    assert!(r.is_err(), "the fourth comm must be refused");
    assert_eq!(meter.0.load(Ordering::SeqCst), 4);
}

#[test]
fn keyed_minting_is_reproducible_and_seed_dependent() {
    let (t, _) = norm_at("new a in { a!(Nil) }", Level::K1).unwrap();
    let chan_of = |seed: [u8; 32]| {
        let mut m = Machine::with(Config::default(), Direct::default(), Keyed::new(seed));
        m.spawn(t.clone());
        m.run(100).unwrap();
        m.channels()[0].0.clone()
    };
    assert!(chan_of([1; 32]) == chan_of([1; 32]));
    assert!(chan_of([1; 32]) != chan_of([2; 32]));
}

#[test]
fn a_name_sent_as_data_is_received_as_that_name() {
    // `c!(*x)` sends the name x; `for (y <- c)` must bind y to x itself (`@*x = x`),
    // and a quoted pattern `@{*y}` must bind it too.
    let (t, free) = norm_at(
        r#"new x, c, d in { c!(*x) | d!(*x) | for (y <- c) { y!(1) } | for (@{*z} <- d) { z!(2) } | for (@v <= x) { out!(v) } }"#,
        Level::K1G,
    )
    .unwrap();
    let out = [8u8; 32];
    let t = ground(&t, &free, &[("out", out)]);
    let mut m: Machine = Machine::new(Config::default());
    m.bind_aperture(Name::Unforgeable(out), ApertureId(0));
    m.spawn(t);
    m.run(10_000).unwrap();
    let mut got: Vec<i64> = m.take_outbound().iter().filter_map(|o| o.args[0].as_int()).collect();
    got.sort();
    assert_eq!(got, vec![1, 2]);
}
