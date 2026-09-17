//! Tests for the glue and the kernel matcher, driven from source text.
//!
//! Two things about K1ndl1ng shape these tests. First, an identifier inside a
//! pattern that no inner scope binds *is* a binder (spec §7), so a pattern that
//! is meant to be ground must name its channels by quoted closed terms —
//! `@{Nil}`, not `a`. Second, free levels are assigned per term, so a receipt
//! and the send it meets must be normalised together if their free names are to
//! agree; each test therefore gives one source containing both.

use super::*;
use k1ndl1ng_norm::{normalise, Node, Options as NOpts, Subst};
use k1ndl1ng_parse::{parse, Options as POpts};

fn norm(src: &str) -> Norm {
    let p = parse(src, &POpts::default());
    assert!(p.ok(), "parse failed for {src}: {:?}", p.diags);
    normalise(&p.tree, &NOpts::default())
        .unwrap_or_else(|d| panic!("normalise failed for {src}: {d:?}"))
        .term
}

/// Split `RECV | SEND` into the receipt's binds and body and the send's args.
fn split(src: &str) -> (Vec<NBind>, Norm, Vec<Norm>) {
    let t = norm(src);
    let mut binds = None;
    let mut body = None;
    let mut args = None;
    for c in t.top_level() {
        match c.node() {
            Node::Receive { binds: b, body: k } => {
                binds = Some(b.clone());
                body = Some(k.clone());
            }
            Node::Send { args: a, .. } => args = Some(a.clone()),
            _ => {}
        }
    }
    (
        binds.expect("no receive"),
        body.expect("no receive"),
        args.expect("no send"),
    )
}

/// Match the receipt's sole bind against the send's arguments.
fn m(src: &str) -> Option<Vec<Val>> {
    let (binds, _, args) = split(src);
    assert_eq!(binds.len(), 1, "expected one bind");
    spatial::match_pats(&binds[0].pats, &args, binds[0].binders)
}

fn as_proc(v: &Val) -> Norm {
    match v {
        Val::Proc(p) => p.clone(),
        Val::Name(Name::Quote(p)) => p.clone(),
        _ => panic!("not a process"),
    }
}

fn enc(n: &Norm) -> Vec<u8> {
    n.encode().to_vec()
}

// -- the matcher ------------------------------------------------------------

#[test]
fn a_name_variable_binds_the_quoted_argument() {
    let src = "for(y <- c){ Nil } | c!(@{Nil}!(Nil))";
    let (_, _, args) = split(src);
    let b = m(src).expect("matches");
    assert_eq!(b.len(), 1);
    // The binding is a *name*: `y` is bound to `@Q`, not to `Q`.
    assert!(matches!(b[0], Val::Name(Name::Quote(_))));
    assert_eq!(enc(&as_proc(&b[0])), enc(&args[0]));
}

#[test]
fn a_wildcard_matches_and_binds_nothing() {
    let b = m("for(_ <- c){ Nil } | c!(@{Nil}!(Nil))").expect("matches");
    assert!(b.is_empty());
}

#[test]
fn arity_must_agree() {
    assert!(m("for(y, z <- c){ Nil } | c!(Nil)").is_none());
    assert!(m("for(y <- c){ Nil } | c!(Nil, Nil)").is_none());
}

#[test]
fn an_identifier_in_a_pattern_is_a_binder_not_a_name() {
    // This is the K1ndl1ng rule that surprises everyone: `d` here binds, so the
    // pattern matches a send on *any* channel.
    let b = m("for(@{d!(Nil)} <- c){ Nil } | c!(@{Nil}!(Nil))").expect("matches");
    assert_eq!(b.len(), 1);
    assert!(matches!(b[0], Val::Name(_)));
}

#[test]
fn a_quoted_pattern_matches_structurally() {
    let b = m("for(@{ @{Nil}!(*y) } <- c){ Nil } | c!(@{Nil}!(*@{Nil}))").expect("matches");
    assert_eq!(b.len(), 1);
    // `*@{Nil}` is `Nil`, so `y` is bound to the name `@Nil`.
    match &b[0] {
        Val::Name(n) => assert_eq!(n.encode(), Name::Quote(Norm::nil()).encode()),
        _ => panic!("expected a name"),
    }
}

#[test]
fn a_quoted_pattern_rejects_a_different_shape() {
    // Wrong channel.
    assert!(m("for(@{ @{Nil}!(Nil) } <- c){ Nil } | c!(@{@{Nil}!(Nil)}!(Nil))").is_none());
    // Wrong arity.
    assert!(m("for(@{ @{Nil}!(Nil) } <- c){ Nil } | c!(@{Nil}!(Nil, Nil))").is_none());
    // Wrong argument.
    assert!(m("for(@{ @{Nil}!(Nil) } <- c){ Nil } | c!(@{Nil}!(@{Nil}!(Nil)))").is_none());
}

#[test]
fn persistence_is_part_of_the_shape() {
    assert!(m("for(@{ @{Nil}!(Nil) } <- c){ Nil } | c!(@{Nil}!!(Nil))").is_none());
    assert!(m("for(@{ @{Nil}!!(Nil) } <- c){ Nil } | c!(@{Nil}!!(Nil))").is_some());
}

#[test]
fn a_ground_par_pattern_matches_up_to_reordering() {
    let a = "@{Nil}";
    let b = "@{@{Nil}!(Nil)}";
    let src = format!("for(@{{ {a}!(Nil) | {b}!(Nil) }} <- c){{ Nil }} | c!({b}!(Nil) | {a}!(Nil))");
    assert!(m(&src).is_some());
}

#[test]
fn a_par_pattern_needs_every_component() {
    let a = "@{Nil}";
    let b = "@{@{Nil}!(Nil)}";
    let src = format!("for(@{{ {a}!(Nil) | {b}!(Nil) }} <- c){{ Nil }} | c!({a}!(Nil))");
    assert!(m(&src).is_none());
}

#[test]
fn a_par_pattern_with_a_hole_searches() {
    let a = "@{Nil}";
    let b = "@{@{Nil}!(Nil)}";
    let src = format!("for(@{{ {a}!(Nil) | x }} <- c){{ Nil }} | c!({b}!(Nil) | {a}!(Nil))");
    let vals = m(&src).expect("matches");
    assert_eq!(vals.len(), 1);
    assert_eq!(enc(&as_proc(&vals[0])), enc(&norm(&format!("{b}!(Nil)"))));
}

#[test]
fn a_par_remainder_takes_several_components() {
    let a = "@{Nil}";
    let b = "@{@{Nil}!(Nil)}";
    let src = format!(
        "for(@{{ {a}!(Nil) | x }} <- c){{ Nil }} | c!({a}!(Nil) | {b}!(Nil) | {b}!({b}!(Nil)))"
    );
    let vals = m(&src).expect("matches");
    let want = norm(&format!("{b}!(Nil) | {b}!({b}!(Nil))"));
    assert_eq!(enc(&as_proc(&vals[0])), enc(&want));
}

#[test]
fn a_par_pattern_backtracks_when_the_first_choice_fails() {
    // The searched component must land on the send that can match it, whichever
    // order the store happens to present them in.
    let a = "@{Nil}";
    let b = "@{@{Nil}!(Nil)}";
    let src = format!("for(@{{ {a}!(*y) | z }} <- c){{ Nil }} | c!({b}!(Nil) | {a}!(*{b}))");
    let vals = m(&src).expect("matches");
    assert_eq!(vals.len(), 2);
    // One slot took the name under the `{a}` send, the other the leftover
    // component; which slot is which is fixed by the encoding order of the
    // pattern, not by how it was written.
    let want = enc(&norm(&format!("{b}!(Nil)")));
    assert!(vals.iter().any(|v| enc(&as_proc(v)) == want));
}

#[test]
fn a_nested_receive_pattern_matches() {
    let a = "@{Nil}";
    let src = format!("for(@{{ for(w <- {a}){{ Nil }} }} <- c){{ Nil }} | c!(for(w <- {a}){{ Nil }})");
    assert!(m(&src).is_some());
    // A different bind kind is a different shape.
    let src = format!("for(@{{ for(w <- {a}){{ Nil }} }} <- c){{ Nil }} | c!(for(w <= {a}){{ Nil }})");
    assert!(m(&src).is_none());
}

#[test]
fn deep_patterns_do_not_blow_the_stack() {
    // Thousands of nested quotes on both sides: the matcher is iterative.
    let mut p = String::from("Nil");
    for _ in 0..8_000 {
        p = format!("*@{{{p}}}");
    }
    let src = format!("for(@{{ @{{Nil}}!({p}) }} <- c){{ Nil }} | c!(@{{Nil}}!({p}))");
    assert!(m(&src).is_some());
}

// -- the store instantiation -------------------------------------------------

fn chan(src: &str) -> Chan {
    Chan::quote(norm(src))
}

#[test]
fn a_send_and_a_receive_meet_in_the_store() {
    let s = rspace(Config::default());
    let (binds, body, _) = split("for(y <- @{Nil}){ *y } | @{Nil}!(Nil)");
    let pats: Vec<BindPat> = binds.iter().map(BindPat::of).collect();
    let chans: Vec<Chan> = binds.iter().map(|b| Chan(b.chan.clone())).collect();
    let k = Cont::new(body, binds.iter().map(|b| b.binders).sum());
    assert!(s
        .consume(&chans, &pats, k, false, PeekSet::empty())
        .unwrap()
        .is_none());

    let arg = norm("@{Nil}!(Nil)");
    let r = s
        .produce(chan("Nil"), Data::new(vec![arg.clone()]), false)
        .unwrap()
        .expect("reacts");
    let vals = slots_of(&r.bound);
    assert_eq!(vals.len(), 1);
    // The body was `*y`; substituting `@Q` for `y` gives `*@Q`, which the smart
    // constructors have already collapsed to `Q`.
    let out = r.cont.body.substitute(&Subst::from_slots(vals));
    assert_eq!(enc(&out), enc(&arg));
}

#[test]
fn a_join_waits_for_both_channels() {
    let s = rspace(Config::default());
    let (binds, body, _) =
        split("for(y <- @{Nil} & z <- @{@{Nil}!(Nil)}){ Nil } | @{Nil}!(Nil)");
    let pats: Vec<BindPat> = binds.iter().map(BindPat::of).collect();
    let chans: Vec<Chan> = binds.iter().map(|b| Chan(b.chan.clone())).collect();
    let k = Cont::new(body, binds.iter().map(|b| b.binders).sum());
    s.consume(&chans, &pats, k, false, PeekSet::empty()).unwrap();
    assert!(s
        .produce(chans[0].clone(), Data::new(vec![norm("Nil")]), false)
        .unwrap()
        .is_none());
    let r = s
        .produce(chans[1].clone(), Data::new(vec![norm("Nil")]), false)
        .unwrap()
        .expect("reacts");
    assert_eq!(r.bound.len(), 2);
}

#[test]
fn the_pattern_decides_which_datum_is_taken() {
    let s = rspace(Config::default());
    let c = chan("Nil");
    s.produce(c.clone(), Data::new(vec![norm("@{Nil}!(Nil)")]), false)
        .unwrap();
    s.produce(
        c.clone(),
        Data::new(vec![norm("@{@{Nil}!(Nil)}!(Nil)")]),
        false,
    )
    .unwrap();
    let (binds, body, _) =
        split("for(@{ @{Nil}!(Nil) } <- @{Nil}){ Nil } | @{Nil}!(@{Nil}!(Nil))");
    let pats: Vec<BindPat> = binds.iter().map(BindPat::of).collect();
    let k = Cont::new(body, 0);
    let r = s
        .consume(&[c.clone()], &pats, k, false, PeekSet::empty())
        .unwrap()
        .expect("reacts");
    assert_eq!(enc(&r.bound[0].datum.args[0]), enc(&norm("@{Nil}!(Nil)")));
    assert_eq!(s.get_data(&c).len(), 1);
}

#[test]
fn channels_are_names_so_equal_terms_share_a_channel() {
    let s = rspace(Config::default());
    // `@{Nil | Nil}` and `@{Nil}` are the same name: par absorbs the unit.
    s.produce(chan("Nil | Nil"), Data::new(vec![norm("Nil")]), false)
        .unwrap();
    assert_eq!(s.get_data(&chan("Nil")).len(), 1);
}

#[test]
fn data_hashing_distinguishes_arity() {
    let a = Data::new(vec![norm("Nil"), norm("Nil")]);
    let b = Data::new(vec![norm("Nil")]);
    assert_ne!(a.content_hash(), b.content_hash());
    // And bindings do not enter the hash.
    let mut c = Data::new(vec![norm("Nil")]);
    c.bound = vec![Val::Proc(norm("@{Nil}!(Nil)"))];
    assert_eq!(b.content_hash(), c.content_hash());
}
