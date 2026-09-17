//! Tests for the machine: whole programs, run to quiescence, checked by what
//! is left in the store and by the step stream.
//!
//! Every program here is closed by `new`, or has its free names grounded first,
//! because free levels are per-term: two deploys that both write `stdout` only
//! meet on one channel once their free names have been grounded to the same
//! unforgeable (see `deploy`).

use super::*;
use campf1r3_core::Keyed;
use k1ndl1ng_norm::{normalise, Options as NOpts};
use k1ndl1ng_parse::{parse, Options as POpts};

fn norm(src: &str) -> Norm {
    let p = parse(src, &POpts::default());
    assert!(p.ok(), "parse failed for {src}: {:?}", p.diags);
    normalise(&p.tree, &NOpts::default())
        .unwrap_or_else(|d| panic!("normalise failed for {src}: {d:?}"))
        .term
}

/// Parse, normalise, and ground the free names by identifier — the deploy path
/// in miniature.
fn deploy(src: &str) -> Norm {
    let p = parse(src, &POpts::default());
    assert!(p.ok(), "parse failed for {src}: {:?}", p.diags);
    let n = normalise(&p.tree, &NOpts::default())
        .unwrap_or_else(|d| panic!("normalise failed for {src}: {d:?}"));
    let idents: Vec<String> = n.free.iter().map(|f| f.ident.clone()).collect();
    n.term.ground_free_by(&idents)
}

fn machine() -> Machine {
    Machine::new(Config::default())
}

fn run(src: &str) -> (Machine, Vec<Step>) {
    let mut m = machine();
    m.spawn(deploy(src));
    let steps = m.run(10_000).expect("no store error");
    (m, steps)
}

fn comms(steps: &[Step]) -> usize {
    steps
        .iter()
        .filter(|s| matches!(s, Step::Comm { .. }))
        .count()
}

#[test]
fn a_send_with_no_receiver_comes_to_rest() {
    let (m, steps) = run("c!(Nil)");
    assert!(m.is_quiescent());
    assert_eq!(comms(&steps), 0);
    assert_eq!(m.space().stats(), (1, 0));
}

#[test]
fn a_receive_with_no_sender_comes_to_rest() {
    let (m, steps) = run("for(y <- c){ Nil }");
    assert!(m.is_quiescent());
    assert_eq!(comms(&steps), 0);
    assert_eq!(m.space().stats(), (0, 1));
}

#[test]
fn a_send_and_a_receive_in_parallel_react() {
    let (m, steps) = run("c!(Nil) | for(y <- c){ Nil }");
    assert!(m.is_quiescent());
    assert_eq!(comms(&steps), 1);
    assert_eq!(m.space().stats(), (0, 0));
    assert_eq!(m.counters().1, 1);
}

#[test]
fn the_continuation_runs_after_the_comm() {
    let (m, steps) = run("c!(Nil) | for(y <- c){ d!(Nil) }");
    assert_eq!(comms(&steps), 1);
    assert_eq!(m.space().stats(), (1, 0));
    assert!(steps
        .iter()
        .any(|s| matches!(s, Step::Comm { spawned, .. } if *spawned == 1)));
}

#[test]
fn the_bound_name_reaches_the_body() {
    // `for(y <- c){ *y }` receiving `@{d!(Nil)}` leaves `d!(Nil)` at rest.
    let (m, _) = run("c!(d!(Nil)) | for(y <- c){ *y }");
    assert_eq!(m.counters().1, 1);
    assert_eq!(m.space().stats(), (1, 0));
    let d = Chan::unforgeable(k1ndl1ng_norm::free_key("d"));
    assert_eq!(m.space().get_data(&d).len(), 1);
}

#[test]
fn new_mints_a_private_channel_that_nobody_else_can_name() {
    let (m, steps) = run("new x in { x!(Nil) } | new x in { x!(Nil) }");
    assert_eq!(comms(&steps), 0);
    assert_eq!(m.space().stats(), (2, 0));
    let minted: Vec<&Step> = steps
        .iter()
        .filter(|s| matches!(s, Step::Minted { .. }))
        .collect();
    assert_eq!(minted.len(), 2);
    match (minted[0], minted[1]) {
        (Step::Minted { first: a, .. }, Step::Minted { first: b, .. }) => assert_ne!(a, b),
        _ => unreachable!(),
    }
}

#[test]
fn a_new_scope_lets_its_own_halves_meet() {
    let (m, steps) = run("new x in { x!(Nil) | for(y <- x){ Nil } }");
    assert_eq!(comms(&steps), 1);
    assert_eq!(m.space().stats(), (0, 0));
}

#[test]
fn a_persistent_receipt_serves_every_sender() {
    let (m, steps) = run("new x in { x!(Nil) | x!(Nil) | x!(Nil) | for(y <= x){ Nil } }");
    assert_eq!(comms(&steps), 3);
    assert_eq!(m.space().stats(), (0, 1));
}

#[test]
fn a_persistent_send_is_reissued_and_comes_to_rest() {
    // The receipt is parked first, so the persistent send arrives to a waiting
    // continuation: it reacts, was never stored, and is re-issued.
    let mut m = machine();
    m.spawn(deploy("for(y <- c){ Nil }"));
    m.run(100).expect("no store error");
    assert_eq!(m.space().stats(), (0, 1));
    m.spawn(deploy("c!!(Nil)"));
    let steps = m.run(100).expect("no store error");
    assert_eq!(comms(&steps), 1);
    assert!(steps.iter().any(|s| matches!(s, Step::Reissued { .. })));
    assert_eq!(m.space().stats(), (1, 0));
}

#[test]
fn a_persistent_receipt_is_reissued_too() {
    // Symmetrically: the data are parked first, so the persistent receipt
    // arrives to three waiting data and serves all of them.
    let mut m = machine();
    m.spawn(deploy("c!(Nil) | c!(@{Nil}!(Nil)) | c!(@{Nil}!(Nil, Nil))"));
    m.run(100).expect("no store error");
    assert_eq!(m.space().stats(), (3, 0));
    m.spawn(deploy("for(y <= c){ Nil }"));
    let steps = m.run(100).expect("no store error");
    assert_eq!(comms(&steps), 3);
    assert_eq!(m.space().stats(), (0, 1));
}

#[test]
fn a_peek_leaves_the_datum_for_the_next_reader() {
    let (m, steps) = run("new x in { x!(Nil) | for(y <<- x){ Nil } }");
    assert_eq!(comms(&steps), 1);
    assert_eq!(m.space().stats(), (1, 0));
}

#[test]
fn a_join_needs_both_channels() {
    let (m, steps) = run("new a, b in { a!(Nil) | for(y <- a & z <- b){ Nil } }");
    assert_eq!(comms(&steps), 0);
    assert_eq!(m.space().stats(), (1, 1));
    let (m2, steps2) = run("new a, b in { a!(Nil) | b!(Nil) | for(y <- a & z <- b){ Nil } }");
    assert_eq!(comms(&steps2), 1);
    assert_eq!(m2.space().stats(), (0, 0));
}

#[test]
fn a_chain_of_three_runs_to_the_end() {
    let src = "new a, b, c in {
        a!(Nil)
      | for(_ <- a){ b!(Nil) }
      | for(_ <- b){ c!(Nil) }
      | for(_ <- c){ Nil }
    }";
    let (m, steps) = run(src);
    assert_eq!(comms(&steps), 3);
    assert_eq!(m.space().stats(), (0, 0));
}

#[test]
fn a_pattern_routes_the_message() {
    let src = "new x in {
        x!(@{Nil}!(Nil))
      | for(@{ @{Nil}!(Nil) } <- x){ Nil }
    }";
    let (m, steps) = run(src);
    assert_eq!(comms(&steps), 1);
    assert_eq!(m.space().stats(), (0, 0));

    let src = "new x in {
        x!(@{Nil}!(Nil, Nil))
      | for(@{ @{Nil}!(Nil) } <- x){ Nil }
    }";
    let (m, steps) = run(src);
    assert_eq!(comms(&steps), 0);
    assert_eq!(m.space().stats(), (1, 1));
}

#[test]
fn free_names_must_be_grounded_for_two_deploys_to_meet() {
    // Two separately normalised terms whose free `c` sits at a different level
    // still meet, because the deploy path grounds `c` by identifier.
    let mut m = machine();
    m.spawn(deploy("c!(Nil)"));
    m.spawn(deploy("q!(Nil) | for(y <- c){ Nil }"));
    let steps = m.run(10_000).expect("no store error");
    assert_eq!(comms(&steps), 1);
    assert_eq!(m.space().stats(), (1, 0));
}

#[test]
fn the_plan_cache_is_reused_across_comms() {
    let (m, _) = run("new x in { x!(Nil) | x!(Nil) | x!(Nil) | for(y <= x){ Nil } }");
    let (hits, misses) = m.backend().stats();
    assert_eq!(misses, 1, "one distinct continuation");
    assert_eq!(hits, 2, "prepared again for each further comm");
    assert_eq!(m.backend().plans(), 1);
}

#[test]
fn the_plan_cache_is_bounded() {
    let mut b = Direct::with_capacity(2);
    for i in 0..8 {
        let src = format!("@{{Nil}}!(Nil){}", " | @{Nil}!(@{Nil}!(Nil))".repeat(i));
        let t = norm(&src);
        b.prepare(&t, 0);
    }
    assert!(b.plans() <= 2, "cache grew to {}", b.plans());
}

#[test]
fn a_run_is_reproducible() {
    let src = "new a, b in {
        a!(Nil) | a!(@{Nil}!(Nil)) | b!(Nil)
      | for(y <= a){ b!(*y) }
      | for(z <- b){ Nil }
    }";
    let sketch = |steps: &[Step]| -> Vec<String> {
        steps
            .iter()
            .map(|s| match s {
                Step::Comm { chan, cont, .. } => format!("C{}{}", chan.short(), cont.short()),
                Step::Produced { chan, datum } => format!("P{}{}", chan.short(), datum.short()),
                Step::Consumed { cont, .. } => format!("K{}", cont.short()),
                Step::Minted { count, .. } => format!("N{count}"),
                Step::Reissued { chan } => format!("R{}", chan.short()),
                Step::Inert { term } => format!("I{}", term.short()),
            })
            .collect()
    };
    let (_, a) = run(src);
    let (_, b) = run(src);
    assert_eq!(sketch(&a), sketch(&b));
    assert!(!a.is_empty());
}

#[test]
fn stepping_one_at_a_time_gives_the_same_result() {
    let src = "new a, b in { a!(Nil) | for(_ <- a){ b!(Nil) } | for(_ <- b){ Nil } }";
    let mut m = machine();
    m.spawn(deploy(src));
    let mut n = 0;
    while let Some(r) = m.step() {
        r.expect("no store error");
        n += 1;
        assert!(n < 1000, "should terminate");
    }
    assert!(m.is_quiescent());
    assert_eq!(m.space().stats(), (0, 0));
    assert_eq!(m.counters().1, 2);
}

#[test]
fn the_store_and_the_machine_agree_on_the_channel() {
    let mut m = machine();
    m.spawn(deploy("c!(Nil)"));
    m.run(100).expect("no store error");
    let chan = Chan::unforgeable(k1ndl1ng_norm::free_key("c"));
    assert_eq!(m.space().get_data(&chan).len(), 1);
    assert_eq!(chan.content_hash().0.len(), 32);
}
