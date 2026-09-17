//! Tests for the executive: the deploy path, the knobs, and the scene.

use super::*;

fn exec() -> Executive {
    Executive::new(Settings::default())
}

#[test]
fn a_bad_program_is_rejected_with_diagnostics() {
    let mut x = exec();
    let e = x.deploy("for(x <- ) {").expect_err("should not parse");
    assert!(!e.diags.is_empty());
    assert!(x.deploys().is_empty());
}

#[test]
fn a_static_error_is_rejected_too() {
    let mut x = exec();
    // Two binders of the same name in one pattern.
    let e = x.deploy("for(@{x | x} <- c){ Nil }").expect_err("should fail");
    assert!(e.diags.iter().any(|d| d.code == "dup-pattern-var"));
}

#[test]
fn a_deploy_records_its_free_names() {
    let mut x = exec();
    let id = x.deploy("stdout!(Nil) | for(y <- stdin){ Nil }").unwrap();
    let d = &x.deploys()[id as usize];
    assert_eq!(d.free.len(), 2);
    assert!(d.free.iter().any(|f| f == "stdout"));
    assert!(d.free.iter().any(|f| f == "stdin"));
}

#[test]
fn two_deploys_meet_on_a_grounded_free_name() {
    let mut x = exec();
    x.deploy("stdout!(Nil)").unwrap();
    x.deploy("for(y <- stdout){ Nil }").unwrap();
    x.run_to_rest().unwrap();
    assert!(x.is_quiescent());
    assert_eq!(x.machine().counters().1, 1);
}

#[test]
fn lockstep_advances_exactly_the_cores_setting() {
    let mut x = Executive::new(Settings {
        cores: 1,
        mode: Mode::Lockstep,
        ..Settings::default()
    });
    x.deploy("new a, b, c in { a!(Nil) | b!(Nil) | c!(Nil) }").unwrap();
    // One component per tick: the `new`, then the three sends.
    let mut ticks = 0;
    while !x.is_quiescent() {
        x.tick().unwrap();
        ticks += 1;
        assert!(ticks < 20);
    }
    assert_eq!(ticks, 4);
    assert_eq!(x.machine().space().stats(), (3, 0));
}

#[test]
fn the_cores_knob_changes_speed_and_not_the_answer() {
    let src = "new a, b, c in {
        a!(Nil) | b!(Nil) | c!(Nil)
      | for(_ <- a){ b!(Nil) }
      | for(_ <- b & _ <- c){ Nil }
    }";
    let mut ticks = Vec::new();
    let mut states = Vec::new();
    for cores in [1u32, 2, 4, 64] {
        let mut x = Executive::new(Settings {
            cores,
            mode: Mode::Lockstep,
            ..Settings::default()
        });
        x.deploy(src).unwrap();
        let mut t = 0;
        while !x.is_quiescent() {
            x.tick().unwrap();
            t += 1;
            assert!(t < 1000);
        }
        ticks.push(t);
        states.push((x.machine().space().export(), x.machine().counters().1));
    }
    // Same final state and same number of comms every time.
    assert!(states.windows(2).all(|w| w[0] == w[1]));
    // But fewer ticks as the knob goes up.
    assert!(ticks[0] > ticks[3]);
}

#[test]
fn the_cores_knob_is_clamped_to_the_device() {
    let mut x = exec();
    x.set_cores(0, 8);
    assert_eq!(x.settings().cores, 1);
    x.set_cores(999, 8);
    assert_eq!(x.settings().cores, 8);
}

#[test]
fn free_running_reaches_quiescence_in_one_tick() {
    let mut x = Executive::new(Settings {
        mode: Mode::FreeRunning,
        ..Settings::default()
    });
    x.deploy("new a in { a!(Nil) | for(_ <- a){ Nil } }").unwrap();
    x.tick().unwrap();
    assert!(x.is_quiescent());
}

#[test]
fn the_scene_shows_what_is_at_rest() {
    let mut x = exec();
    x.deploy("new a, b in { a!(Nil) | for(y <- b){ Nil } }").unwrap();
    x.run_to_rest().unwrap();
    let sc = x.scene();
    assert!(sc.quiescent);
    assert_eq!(sc.rhobots.len(), 0);
    assert_eq!(sc.tuples.len(), 1);
    assert_eq!(sc.listeners.len(), 1);
    assert!(sc.tuples[0].label.contains("!("));
    assert!(sc.listeners[0].label.starts_with("for("));
    assert!(sc.listeners[0].chan.as_deref().unwrap().starts_with("u_"));
}

#[test]
fn the_scene_shows_a_join_as_one_listener() {
    let mut x = exec();
    x.deploy("new a, b in { for(y <- a & z <- b){ Nil } }").unwrap();
    x.run_to_rest().unwrap();
    let sc = x.scene();
    assert_eq!(sc.listeners.len(), 1);
    assert_eq!(sc.listeners[0].group.len(), 2);
    assert!(sc.listeners[0].label.contains(" & "));
}

#[test]
fn the_scene_shows_queued_rhobots_mid_run() {
    let mut x = Executive::new(Settings {
        cores: 1,
        mode: Mode::Lockstep,
        ..Settings::default()
    });
    x.deploy("new a in { a!(Nil) | a!(Nil) | a!(Nil) }").unwrap();
    x.tick().unwrap(); // opens the `new`
    let sc = x.scene();
    assert_eq!(sc.rhobots.len(), 3);
    assert!(sc.rhobots.iter().all(|r| r.kind == "rhobot"));
    assert!(!sc.quiescent);
}

#[test]
fn a_scene_is_the_same_from_one_run_to_the_next() {
    let src = "new a, b in { a!(Nil) | a!(@{Nil}!(Nil)) | for(y <= a){ b!(*y) } }";
    let render = || {
        let mut x = exec();
        x.deploy(src).unwrap();
        x.run_to_rest().unwrap();
        json::scene(&x.scene())
    };
    assert_eq!(render(), render());
}

#[test]
fn the_json_is_well_formed_enough_to_read() {
    let mut x = exec();
    x.deploy("new a in { a!(Nil) | for(y <- a){ Nil } }").unwrap();
    let steps = x.run_to_rest().unwrap();
    let js = json::steps(&steps);
    assert!(js.starts_with('['));
    assert!(js.ends_with(']'));
    assert!(js.contains("\"kind\":\"comm\""));
    let sc = json::scene(&x.scene());
    assert!(sc.contains("\"quiescent\":true"));
    assert!(sc.contains("\"rhobots\":[]"));
}

#[test]
fn json_escapes_what_it_must() {
    assert_eq!(json::esc("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
}

#[test]
fn the_store_log_records_the_run() {
    let mut x = exec();
    x.deploy("new a in { a!(Nil) | for(y <- a){ Nil } }").unwrap();
    x.run_to_rest().unwrap();
    let log = x.take_log();
    assert!(log
        .iter()
        .any(|e| matches!(e, campf1r3_core::Event::Comm { .. })));
    // Draining, like the store's own log.
    assert!(x.take_log().is_empty());
}

#[test]
fn a_program_that_cannot_finish_still_comes_to_rest() {
    // A persistent receipt with nothing to serve: quiescent, not spinning.
    let mut x = exec();
    x.deploy("new a in { for(y <= a){ a!(Nil) } }").unwrap();
    x.run_to_rest().unwrap();
    assert!(x.is_quiescent());
    assert_eq!(x.machine().space().stats(), (0, 1));
}

#[test]
fn a_persistent_loop_is_bounded_by_the_budget() {
    // `for(y <= a){ a!(Nil) }` fed one datum runs forever: the budget is what
    // stops it, and the machine is still consistent afterwards.
    let mut x = Executive::new(Settings {
        mode: Mode::FreeRunning,
        budget: 200,
        ..Settings::default()
    });
    x.deploy("new a in { a!(Nil) | for(y <= a){ a!(Nil) } }").unwrap();
    let steps = x.tick().unwrap();
    assert!(!steps.is_empty());
    assert!(!x.is_quiescent(), "the loop should still have work queued");
    assert!(x.machine().counters().1 > 10);
}

#[test]
fn a_card_names_its_channel_the_same_way_twice() {
    // The `chan` field and the channel inside the `label` must be the same
    // string, or the renderer draws two names for one channel.
    let mut x = exec();
    x.deploy("new a in { a!(Nil) | for(y <- a){ Nil } | for(z <- a){ Nil } }")
        .unwrap();
    x.run_to_rest().unwrap();
    let sc = x.scene();
    assert!(!sc.listeners.is_empty());
    for c in sc.tuples.iter().chain(sc.listeners.iter()) {
        let ch = c.chan.as_deref().expect("a card on a channel");
        assert!(c.label.contains(ch), "label {:?} lacks chan {ch}", c.label);
    }
}

#[test]
fn cards_on_different_channels_have_different_ids() {
    // The store keys a datum by content alone, so the same tuple on two
    // channels shares a hash; the card id must still tell them apart.
    let mut x = exec();
    x.deploy("new a, b in { a!(Nil) | b!(Nil) }").unwrap();
    x.run_to_rest().unwrap();
    let sc = x.scene();
    assert_eq!(sc.tuples.len(), 2);
    assert_ne!(sc.tuples[0].id, sc.tuples[1].id);
}
