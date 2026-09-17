//! Tests for the store, over a toy instantiation: channels and data are
//! strings, a pattern is either a wildcard or a literal, and a continuation is
//! a string. The core is indifferent to what these are, which is the point.

use super::*;

fn h(s: &str) -> Hash32 {
    // A small, stable, dependency-free hash. The core never computes these in
    // production; the caller does.
    let mut st = [0u8; 32];
    let mut a: u64 = 0xcbf2_9ce4_8422_2325;
    for (i, b) in s.bytes().enumerate() {
        a ^= b as u64;
        a = a.wrapping_mul(0x1000_0000_01b3);
        st[i % 32] ^= (a >> 24) as u8;
    }
    for i in 0..4 {
        let w = a.rotate_left(13 * (i as u32 + 1)).to_le_bytes();
        for j in 0..8 {
            st[i * 8 + j] ^= w[j];
        }
    }
    Hash32(st)
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Ch(String);
#[derive(Clone, PartialEq, Eq, Debug)]
struct Da(String);
#[derive(Clone, PartialEq, Eq, Debug)]
struct Ko(String);
#[derive(Clone, Debug)]
enum Pa {
    Any,
    Lit(String),
}

impl Keyed for Ch {
    fn content_hash(&self) -> Hash32 {
        h(&self.0)
    }
}
impl Keyed for Da {
    fn content_hash(&self) -> Hash32 {
        h(&self.0)
    }
}
impl Keyed for Ko {
    fn content_hash(&self) -> Hash32 {
        h(&self.0)
    }
}
impl Channel for Ch {}
impl Datum for Da {}
impl Cont for Ko {}
impl Pattern for Pa {}

struct M;
impl Matcher<Pa, Da, Ko> for M {
    fn get(&self, p: &Pa, a: &Da) -> Option<Da> {
        match p {
            Pa::Any => Some(a.clone()),
            Pa::Lit(s) => {
                if *s == a.0 {
                    Some(a.clone())
                } else {
                    None
                }
            }
        }
    }
}

type S = Space<Ch, Pa, Da, Ko, M, NoObserver>;

fn space() -> S {
    Space::new(Config::default(), M, NoObserver)
}

fn c(s: &str) -> Ch {
    Ch(s.to_string())
}
fn d(s: &str) -> Da {
    Da(s.to_string())
}
fn k(s: &str) -> Ko {
    Ko(s.to_string())
}

#[test]
fn produce_then_consume() {
    let s = space();
    assert!(s.produce(c("a"), d("1"), false).unwrap().is_none());
    let r = s
        .consume(&[c("a")], &[Pa::Any], k("K"), false, PeekSet::empty())
        .unwrap()
        .expect("should react");
    assert_eq!(r.bound[0].binding, d("1"));
    assert_eq!(s.stats(), (0, 0));
}

#[test]
fn consume_then_produce() {
    let s = space();
    assert!(s
        .consume(&[c("a")], &[Pa::Any], k("K"), false, PeekSet::empty())
        .unwrap()
        .is_none());
    let r = s.produce(c("a"), d("1"), false).unwrap().expect("reacts");
    assert_eq!(r.cont, k("K"));
    assert_eq!(r.bound[0].datum, d("1"));
    assert_eq!(s.stats(), (0, 0));
}

#[test]
fn pattern_selects_among_data() {
    let s = space();
    s.produce(c("a"), d("x"), false).unwrap();
    s.produce(c("a"), d("y"), false).unwrap();
    let r = s
        .consume(
            &[c("a")],
            &[Pa::Lit("y".into())],
            k("K"),
            false,
            PeekSet::empty(),
        )
        .unwrap()
        .expect("reacts");
    assert_eq!(r.bound[0].datum, d("y"));
    // x is still there.
    assert_eq!(s.get_data(&c("a")), vec![d("x")]);
}

#[test]
fn no_match_leaves_both_at_rest() {
    let s = space();
    s.produce(c("a"), d("x"), false).unwrap();
    assert!(s
        .consume(
            &[c("a")],
            &[Pa::Lit("y".into())],
            k("K"),
            false,
            PeekSet::empty()
        )
        .unwrap()
        .is_none());
    assert_eq!(s.stats(), (1, 1));
}

#[test]
fn join_waits_for_all_members() {
    let s = space();
    assert!(s
        .consume(
            &[c("a"), c("b")],
            &[Pa::Any, Pa::Any],
            k("J"),
            false,
            PeekSet::empty()
        )
        .unwrap()
        .is_none());
    assert!(s.produce(c("a"), d("1"), false).unwrap().is_none());
    let r = s.produce(c("b"), d("2"), false).unwrap().expect("reacts");
    assert_eq!(r.bound.len(), 2);
    assert_eq!(r.bound[0].datum, d("1"));
    assert_eq!(r.bound[1].datum, d("2"));
    assert_eq!(s.stats(), (0, 0));
}

#[test]
fn join_on_one_channel_twice_needs_two_data() {
    // for (x <- a & y <- a): distinct occurrences are required.
    let s = space();
    s.consume(
        &[c("a"), c("a")],
        &[Pa::Any, Pa::Any],
        k("J"),
        false,
        PeekSet::empty(),
    )
    .unwrap();
    assert!(s.produce(c("a"), d("1"), false).unwrap().is_none());
    let r = s.produce(c("a"), d("2"), false).unwrap().expect("reacts");
    assert_eq!(r.bound.len(), 2);
    assert_eq!(s.stats(), (0, 0));
}

#[test]
fn persistent_continuation_stays() {
    let s = space();
    s.consume(&[c("a")], &[Pa::Any], k("K"), true, PeekSet::empty())
        .unwrap();
    s.produce(c("a"), d("1"), false).unwrap().expect("reacts");
    s.produce(c("a"), d("2"), false).unwrap().expect("reacts");
    assert_eq!(s.stats(), (0, 1));
}

#[test]
fn persistent_datum_stays_and_the_reissue_contract_holds() {
    let s = space();
    // A persistent datum that reacts is NOT stored: the caller re-issues.
    s.consume(&[c("a")], &[Pa::Any], k("K"), false, PeekSet::empty())
        .unwrap();
    assert!(s.produce(c("a"), d("1"), true).unwrap().is_some());
    assert_eq!(s.stats(), (0, 0));
    // Re-issued: nothing waiting now, so it comes to rest.
    assert!(s.produce(c("a"), d("1"), true).unwrap().is_none());
    assert_eq!(s.stats(), (1, 0));
    // And it survives being consumed.
    s.consume(&[c("a")], &[Pa::Any], k("K2"), false, PeekSet::empty())
        .unwrap()
        .expect("reacts");
    assert_eq!(s.get_data(&c("a")), vec![d("1")]);
}

#[test]
fn peek_leaves_the_datum() {
    let s = space();
    s.produce(c("a"), d("1"), false).unwrap();
    let r = s
        .consume(
            &[c("a")],
            &[Pa::Any],
            k("P"),
            false,
            PeekSet::from_indices(&[0]),
        )
        .unwrap()
        .expect("reacts");
    assert_eq!(r.bound[0].datum, d("1"));
    assert_eq!(s.get_data(&c("a")), vec![d("1")]);
}

#[test]
fn install_refuses_to_react_and_survives_clear() {
    let s = space();
    s.produce(c("a"), d("1"), false).unwrap();
    assert_eq!(
        s.install(&[c("a")], &[Pa::Any], k("I")),
        Err(SpaceError::InstallWouldReact)
    );
    s.remove_all_data(&c("a"));
    s.install(&[c("a")], &[Pa::Any], k("I")).unwrap();
    s.produce(c("a"), d("1"), false).unwrap().expect("reacts");
    // Installed continuations are permanent.
    s.produce(c("a"), d("2"), false).unwrap().expect("reacts");
    s.clear();
    assert_eq!(s.stats(), (0, 1));
    s.produce(c("a"), d("3"), false).unwrap().expect("reacts");
}

#[test]
fn installed_is_preferred_to_waiting() {
    let s = space();
    s.install(&[c("a")], &[Pa::Any], k("I")).unwrap();
    s.consume(&[c("a")], &[Pa::Any], k("W"), false, PeekSet::empty())
        .unwrap();
    let r = s.produce(c("a"), d("1"), false).unwrap().expect("reacts");
    assert_eq!(r.cont, k("I"));
}

#[test]
fn arity_and_width_are_checked() {
    let s = space();
    assert_eq!(
        s.consume(&[c("a")], &[], k("K"), false, PeekSet::empty())
            .err(),
        Some(SpaceError::ArityMismatch)
    );
    assert_eq!(
        s.consume(&[], &[], k("K"), false, PeekSet::empty()).err(),
        Some(SpaceError::EmptyGroup)
    );
    let wide: Vec<Ch> = (0..65).map(|i| c(&format!("c{i}"))).collect();
    let pats: Vec<Pa> = (0..65).map(|_| Pa::Any).collect();
    assert_eq!(
        s.consume(&wide, &pats, k("K"), false, PeekSet::empty()).err(),
        Some(SpaceError::GroupTooWide)
    );
}

#[test]
fn observer_can_abort_the_cut() {
    struct Veto;
    impl Observer<Ch, Pa, Da, Ko> for Veto {
        fn observe_comm(&self, _k: &Ko, _p: bool, _d: &[(&Da, bool)]) -> Result<(), Abort> {
            Err(Abort)
        }
    }
    let s: Space<Ch, Pa, Da, Ko, M, Veto> = Space::new(Config::default(), M, Veto);
    s.consume(&[c("a")], &[Pa::Any], k("K"), false, PeekSet::empty())
        .unwrap();
    assert_eq!(
        s.produce(c("a"), d("1"), false).err(),
        Some(SpaceError::Aborted)
    );
}

#[test]
fn checkpoint_and_revert() {
    let s = space();
    s.produce(c("a"), d("1"), false).unwrap();
    let cp = s.soft_checkpoint();
    s.produce(c("b"), d("2"), false).unwrap();
    s.consume(&[c("a")], &[Pa::Any], k("K"), false, PeekSet::empty())
        .unwrap()
        .expect("reacts");
    s.revert(&cp);
    assert_eq!(s.get_data(&c("a")), vec![d("1")]);
    assert!(s.get_data(&c("b")).is_empty());
}

#[test]
fn the_log_records_the_cuts() {
    let s = space();
    s.produce(c("a"), d("1"), false).unwrap();
    s.consume(&[c("a")], &[Pa::Any], k("K"), false, PeekSet::empty())
        .unwrap();
    let ev = s.take_log();
    assert_eq!(ev.len(), 2);
    assert!(matches!(ev[0], Event::Produce { .. }));
    assert!(matches!(ev[1], Event::Comm { .. }));
    assert!(s.take_log().is_empty());
}

#[test]
fn determinism_does_not_depend_on_shard_count() {
    // The same script in two differently sharded stores yields the same log.
    fn run(shards: u32) -> Vec<Event> {
        let cfg = Config {
            shards,
            ..Config::default()
        };
        let s: S = Space::new(cfg, M, NoObserver);
        for i in 0..8 {
            s.produce(c(&format!("ch{}", i % 3)), d(&format!("v{i}")), false)
                .unwrap();
        }
        for i in 0..3 {
            s.consume(
                &[c(&format!("ch{i}"))],
                &[Pa::Any],
                k(&format!("K{i}")),
                false,
                PeekSet::empty(),
            )
            .unwrap();
        }
        s.take_log()
    }
    assert_eq!(run(1), run(64));
    assert_eq!(run(4), run(64));
}

#[test]
fn replay_admits_only_the_rigged_comms() {
    let s0 = space();
    s0.produce(c("a"), d("x"), false).unwrap();
    s0.produce(c("a"), d("y"), false).unwrap();
    s0.consume(&[c("a")], &[Pa::Any], k("K"), false, PeekSet::empty())
        .unwrap();
    let chosen: Vec<Hash32> = s0
        .take_log()
        .into_iter()
        .filter_map(|e| match e {
            Event::Comm { cont, data, .. } => Some((cont, data)),
            _ => None,
        })
        .map(|(_, d)| d[0])
        .collect();

    let cfg = Config {
        profile: Profile::Replay,
        ..Config::default()
    };
    let s: S = Space::new(cfg, M, NoObserver);
    // Rig the opposite choice and check the space refuses the other one.
    let other = if chosen[0] == h("x") { h("y") } else { h("x") };
    s.rig(vec![(h("K"), vec![other])]);
    s.produce(c("a"), d("x"), false).unwrap();
    s.produce(c("a"), d("y"), false).unwrap();
    let r = s
        .consume(&[c("a")], &[Pa::Any], k("K"), false, PeekSet::empty())
        .unwrap()
        .expect("the rigged comm is available");
    assert_eq!(r.bound[0].datum.content_hash(), other);
    assert!(s.check_replay().is_ok());
}

#[test]
fn joins_are_visible_and_exports_are_stable() {
    let s = space();
    s.consume(
        &[c("a"), c("b")],
        &[Pa::Any, Pa::Any],
        k("J"),
        false,
        PeekSet::empty(),
    )
    .unwrap();
    assert_eq!(s.get_joins(&c("a")), vec![vec![c("a"), c("b")]]);
    let e1 = s.export();
    let e2 = s.export();
    assert_eq!(e1, e2);
}

#[test]
fn many_threads_agree_on_the_count() {
    use std::sync::Arc;
    let s = Arc::new(space());
    let mut hs = Vec::new();
    for t in 0..8 {
        let s = Arc::clone(&s);
        hs.push(std::thread::spawn(move || {
            for i in 0..50 {
                s.produce(c(&format!("c{}", (t + i) % 7)), d(&format!("{t}-{i}")), false)
                    .unwrap();
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(s.stats().0, 400);
    // Drain: while a channel has data, a linear consume must react.
    let mut got = 0;
    for ch in 0..7 {
        let chan = c(&format!("c{ch}"));
        while !s.get_data(&chan).is_empty() {
            s.consume(&[chan.clone()], &[Pa::Any], k("D"), false, PeekSet::empty())
                .unwrap()
                .expect("data present, so it must react");
            got += 1;
        }
    }
    assert_eq!(got, 400);
    assert_eq!(s.stats(), (0, 0));
}

#[test]
fn a_row_key_leads_back_to_its_channel() {
    let s = space();
    s.produce(c("a"), d("1"), false).unwrap();
    assert_eq!(s.channel_of(&h("a")), Some(c("a")));
    assert_eq!(s.channel_of(&h("never-seen")), None);
}
