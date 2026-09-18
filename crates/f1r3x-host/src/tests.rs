//! Tests for the host layer, and nothing below it.
//!
//! The executive's behaviour is tested in its own crates. What is tested here
//! is what this layer adds: the JSON a client reads, the admission contract,
//! and the command vocabulary all three hosts share. None of it needs a
//! socket, which is the point — an earlier version of these tests could only
//! be run by opening one.

use super::*;
use crate::proto::After;

use f1r3x::Executive;
use k1ndl1ng_norm::json::{tree_to_string, Budget};
use k1ndl1ng_norm::{normalise, Options as NOpts};
use k1ndl1ng_parse::{parse, Options as POpts};

fn term(src: &str) -> k1ndl1ng_norm::Norm {
    let p = parse(src, &POpts::default());
    assert!(p.ok(), "parse failed for `{src}`");
    let n = normalise(&p.tree, &NOpts::default()).expect("normalise");
    let free: Vec<String> = n.free.iter().map(|f| f.ident.clone()).collect();
    n.term.ground_free_by(&free)
}

/// Balanced braces and brackets is a cheap total check that the
/// explicit-stack walk pushes its closers in the right order — exactly the
/// mistake that kind of walk invites.
fn balanced(s: &str) -> bool {
    let mut stack = Vec::new();
    let (mut in_str, mut esc) = (false, false);
    for c in s.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' | '[' => stack.push(c),
            '}' => {
                if stack.pop() != Some('{') {
                    return false;
                }
            }
            ']' => {
                if stack.pop() != Some('[') {
                    return false;
                }
            }
            _ => {}
        }
    }
    stack.is_empty() && !in_str
}

const RELAY: &str = "for(y <- @{Nil}){ *y } | @{Nil}!(@{Nil}!(Nil))";

// ---------------------------------------------------------------------------
// The tree encoding (k1ndl1ng-norm::json, exercised from here because this is
// the crate that depends on its being right)

#[test]
fn every_form_encodes_to_balanced_json() {
    for src in [
        "Nil",
        "@{Nil}!(Nil)",
        "for(y <- @{Nil}){ *y }",
        RELAY,
        "@{Nil}!(@{Nil}!(Nil))",
        "new x in { x!(Nil) }",
        "for(y <= @{Nil}){ Nil }",
        "for(y <<- @{Nil}){ Nil }",
        "for(a <- @{Nil} & b <- @{@{Nil}!(Nil)}){ *a | *b }",
        "for(y <- @{Nil}){ for(z <- y){ *z } } | @{Nil}!(@{Nil}!(Nil))",
    ] {
        let j = tree_to_string(&term(src), Budget::default());
        assert!(balanced(&j), "unbalanced tree for `{src}`:\n{j}");
        assert!(j.starts_with("{\"f\":\""), "no form tag for `{src}`: {j}");
    }
}

#[test]
fn a_send_carries_its_channel_and_its_argument() {
    let j = tree_to_string(&term("@{Nil}!(Nil)"), Budget::default());
    assert!(j.contains("\"f\":\"send\""), "{j}");
    assert!(j.contains("\"chan\":{\"g\":\"quote\""), "{j}");
    assert!(j.contains("\"args\":["), "{j}");
}

#[test]
fn a_receive_carries_its_binds_and_its_continuation() {
    let j = tree_to_string(&term("for(y <- @{Nil}){ *y }"), Budget::default());
    assert!(j.contains("\"f\":\"receive\""), "{j}");
    assert!(j.contains("\"kind\":\"linear\""), "{j}");
    assert!(j.contains("\"arrow\":\"<-\""), "{j}");
    assert!(j.contains("\"body\":"), "{j}");
    assert!(j.contains("\"f\":\"drop\""), "{j}");
}

#[test]
fn the_three_bind_kinds_are_distinguished() {
    for (src, kind, arrow) in [
        ("for(y <- @{Nil}){ Nil }", "linear", "<-"),
        ("for(y <= @{Nil}){ Nil }", "persistent", "<="),
        ("for(y <<- @{Nil}){ Nil }", "peek", "<<-"),
    ] {
        let j = tree_to_string(&term(src), Budget::default());
        assert!(j.contains(&format!("\"kind\":\"{kind}\"")), "{src}: {j}");
        assert!(j.contains(&format!("\"arrow\":\"{arrow}\"")), "{src}: {j}");
    }
}

#[test]
fn a_quotation_budget_elides_and_keeps_the_fingerprint() {
    let t = term("@{Nil}!(@{Nil}!(@{Nil}!(@{Nil}!(Nil))))");
    let deep = tree_to_string(&t, Budget::with_quote_depth(10));
    let shallow = tree_to_string(&t, Budget::with_quote_depth(1));
    assert!(!deep.contains("\"elided\":true"), "nothing should elide deep");
    assert!(shallow.contains("\"elided\""), "should elide at depth 1");
    assert!(shallow.len() < deep.len());
    assert!(balanced(&shallow), "{shallow}");
    // The point of the elision glyph: the name stays comparable.
    assert!(shallow.contains("\"fp\":\""));
}

#[test]
fn congruent_terms_encode_to_the_same_tree() {
    // Order in a parallel composition carries no meaning and the normaliser
    // has already absorbed it; the tree inherits that, or a renderer would
    // redraw the world every time two components swapped places.
    let a = tree_to_string(&term("@{Nil}!(Nil) | for(y <- @{Nil}){ *y }"), Budget::default());
    let b = tree_to_string(&term("for(y <- @{Nil}){ *y } | @{Nil}!(Nil)"), Budget::default());
    assert_eq!(a, b);
}

#[test]
fn matching_channels_give_matching_fingerprints() {
    let s = tree_to_string(&term("@{Nil}!(Nil)"), Budget::default());
    let r = tree_to_string(&term("for(y <- @{Nil}){ Nil }"), Budget::default());
    let fp = |j: &str| {
        let i = j.find("\"chan\":{").expect("a chan");
        let k = j[i..].find("\"fp\":\"").expect("a fingerprint") + i + 6;
        j[k..k + 64].to_string()
    };
    assert_eq!(fp(&s), fp(&r));
}

// ---------------------------------------------------------------------------
// The scene frame

fn loaded(src: &str) -> Session {
    let mut s = Session::new(Config {
        cores_max: 4,
        ..Config::default()
    });
    s.executive_mut().deploy(src).expect("deploy");
    s
}

#[test]
fn a_scene_names_the_three_populations_and_gives_each_a_tree() {
    let s = loaded(RELAY);
    let j = s.scene_frame().json;
    assert!(balanced(&j), "unbalanced scene:\n{j}");
    for key in ["\"rhobots\":[", "\"tuples\":[", "\"listeners\":[", "\"tick\":"] {
        assert!(j.contains(key), "scene missing {key}");
    }
    assert_eq!(j.matches("\"tree\":").count(), j.matches("\"id\":").count());
}

#[test]
fn a_card_here_is_the_card_f1r3x_built() {
    // The host must not invent handles, and must not walk the machine twice:
    // `Card` carries its own term, so an id and a tree cannot describe
    // different objects.
    let mut s = loaded(RELAY);
    let _ = s.executive_mut().tick();
    let _ = s.executive_mut().tick();
    let theirs = f1r3x::json::scene(&s.executive().scene());
    let ours = s.scene_frame().json;
    let ids: Vec<String> = {
        let mut out = Vec::new();
        let mut rest = theirs.as_str();
        while let Some(i) = rest.find("\"id\":\"") {
            rest = &rest[i + 6..];
            if let Some(k) = rest.find('"') {
                out.push(rest[..k].to_string());
            }
        }
        out
    };
    assert!(!ids.is_empty(), "the reference scene had no cards");
    for id in ids {
        assert!(
            ours.contains(&format!("\"id\":\"{id}\"")),
            "card {id} is in f1r3x's scene but not in the host frame"
        );
    }
}

// ---------------------------------------------------------------------------
// Admission: lockstep and free-running are one rule

fn admission(pacing: Pacing, buffer: u64, in_flight: u64) -> bool {
    Control {
        pacing,
        buffer,
        in_flight,
        ..Control::default()
    }
    .admission_open()
}

#[test]
fn lockstep_is_free_running_with_a_buffer_of_one() {
    assert!(admission(Pacing::Lockstep, 128, 0));
    assert!(!admission(Pacing::Lockstep, 128, 1));
    assert!(admission(Pacing::FreeRunning, 1, 0));
    assert!(!admission(Pacing::FreeRunning, 1, 1));
}

#[test]
fn free_running_admits_up_to_the_bound_and_no_further() {
    assert!(admission(Pacing::FreeRunning, 4, 3));
    assert!(!admission(Pacing::FreeRunning, 4, 4));
    assert!(!admission(Pacing::FreeRunning, 4, 9));
}

#[test]
fn an_ack_frees_exactly_the_rounds_it_names() {
    let mut s = Session::new(Config::default());
    {
        let c = s.control_mut();
        c.pacing = Pacing::FreeRunning;
        c.buffer = 8;
        c.round = 5;
        c.in_flight = 5;
    }
    s.ack(3);
    assert_eq!(s.control().in_flight, 2);
    s.ack(5);
    assert_eq!(s.control().in_flight, 0);
    // A duplicate ack after a reconnect cannot push it below zero.
    s.ack(3);
    assert_eq!(s.control().in_flight, 0);
}

// ---------------------------------------------------------------------------
// Polling: the driver, with no thread and no clock

#[test]
fn a_paused_session_admits_nothing() {
    let mut s = loaded(RELAY);
    let frames = s.poll(0);
    assert!(
        !frames.iter().any(|f| f.tag == "round"),
        "a round was admitted without `play`"
    );
}

#[test]
fn lockstep_withholds_the_next_round_until_it_is_acknowledged() {
    // The heart of the pacing contract, and — now that the driver is a state
    // machine — testable without a socket, a thread or a sleep.
    let mut s = loaded(RELAY);
    s.handle("play");
    let first: Vec<Frame> = s.poll(0);
    assert!(first.iter().any(|f| f.tag == "round"), "no first round");
    assert_eq!(s.control().in_flight, 1);

    // However many times it is polled, nothing more is admitted.
    for t in 1..50 {
        assert!(
            !s.poll(t * 10).iter().any(|f| f.tag == "round"),
            "a second round was admitted without an acknowledgment"
        );
    }
    s.handle("ack 1");
    assert!(
        s.poll(1000).iter().any(|f| f.tag == "round"),
        "the acknowledged round did not release admission"
    );
}

#[test]
fn free_running_runs_ahead_but_only_as_far_as_the_buffer() {
    let mut s = loaded(RELAY);
    s.handle("pacing free");
    s.handle("buffer 2");
    s.handle("play");
    let mut rounds = 0;
    for t in 0..50 {
        rounds += s.poll(t * 10).iter().filter(|f| f.tag == "round").count();
    }
    assert_eq!(rounds, 2, "the buffer bound was not respected");
}

#[test]
fn a_step_admits_exactly_one_round() {
    let mut s = loaded(RELAY);
    s.handle("step");
    let mut rounds = 0;
    for t in 0..50 {
        rounds += s.poll(t * 10).iter().filter(|f| f.tag == "round").count();
    }
    assert_eq!(rounds, 1);
}

#[test]
fn a_round_carries_its_own_scene() {
    let mut s = loaded(RELAY);
    s.handle("play");
    let round = s.poll(0).into_iter().find(|f| f.tag == "round").unwrap();
    assert!(round.json.contains("\"scene\":{"), "{}", round.json);
    assert!(balanced(&round.json));
}

#[test]
fn quiescence_stops_the_session_asking() {
    let mut s = loaded("@{Nil}!(Nil)");
    s.handle("play");
    let mut saw_quiescent = false;
    for t in 0..50 {
        for f in s.poll(t * 10) {
            if f.tag == "quiescent" {
                saw_quiescent = true;
            }
        }
        s.ack(s.control().round);
    }
    assert!(saw_quiescent);
    assert!(!s.control().running, "a quiescent session should stop asking");
}

#[test]
fn the_round_width_changes_the_tick_count_and_nothing_else() {
    // The property the demo rests on: a person can slow the world to a single
    // step and watch exactly the run they saw at speed.
    let run_at = |cores: u32| -> String {
        let mut s = loaded(RELAY);
        s.handle(&format!("cores {cores} 8"));
        s.handle("pacing free");
        s.handle("buffer 64");
        s.handle("play");
        let mut last = String::new();
        for t in 0..200 {
            for f in s.poll(t * 10) {
                if f.tag == "round" {
                    last = f.json.clone();
                }
            }
            s.ack(s.control().round);
        }
        // Less the tick counter: how many ticks the run took is exactly what
        // the width is allowed to change.
        let scene = last.split("\"scene\":").nth(1).unwrap_or("").to_string();
        scene
            .split_once(",\"quiescent\"")
            .map(|(_, t)| t.to_string())
            .unwrap_or(scene)
    };
    assert_eq!(run_at(1), run_at(4));
}

// ---------------------------------------------------------------------------
// The vocabulary

#[test]
fn a_correlation_number_comes_back_on_the_reply() {
    let mut s = Session::new(Config::default());
    let (f, _) = s.handle("#42 status");
    assert!(f.json.contains("\"seq\":42"), "{}", f.json);
    let (f, _) = s.handle("status");
    assert!(f.json.contains("\"seq\":null"), "{}", f.json);
}

#[test]
fn a_deploy_with_a_payload_survives_the_seq_prefix() {
    let mut s = Session::new(Config::default());
    let (f, _) = s.handle("#7 deploy\n@{Nil}!(Nil)");
    assert!(f.json.contains("\"seq\":7"), "{}", f.json);
    assert!(f.json.contains("\\\"ok\\\":true") || f.json.contains("\"ok\":true"));
    assert_eq!(s.executive().deploys().len(), 1);
}

#[test]
fn a_bad_term_comes_back_as_diagnostics() {
    let mut s = Session::new(Config::default());
    let (f, _) = s.handle("deploy\nfor(y <- ){ *y");
    assert!(f.json.contains("diags"), "{}", f.json);
}

#[test]
fn the_panel_verbs_move_the_control_state() {
    let mut s = Session::new(Config::default());
    s.handle("pacing free");
    assert_eq!(s.control().pacing, Pacing::FreeRunning);
    s.handle("buffer 32");
    assert_eq!(s.control().buffer, 32);
    s.handle("play");
    assert!(s.control().running);
    s.handle("pause");
    assert!(!s.control().running);
    s.handle("step");
    assert_eq!(s.control().steps_owed, 1);
    s.handle("treedepth 2");
    assert_eq!(s.control().quote_depth, 2);
    // Out-of-range values are refused rather than clamped silently.
    let (f, _) = s.handle("buffer 0");
    assert!(f.json.contains("bad-argument"), "{}", f.json);
    assert_eq!(s.control().buffer, 32);
}

#[test]
fn reset_keeps_the_pacing_and_clears_the_world() {
    let mut s = loaded("@{Nil}!(Nil)");
    s.handle("pacing free");
    s.handle("buffer 16");
    s.handle("reset");
    assert_eq!(s.executive().deploys().len(), 0);
    assert_eq!(s.control().round, 0);
    // The client's settings are the client's, and a reset of the world is not
    // a reset of them.
    assert_eq!(s.control().pacing, Pacing::FreeRunning);
    assert_eq!(s.control().buffer, 16);
}

#[test]
fn bye_asks_the_host_to_close() {
    let mut s = Session::new(Config::default());
    let (_, after) = s.handle("bye");
    assert!(matches!(after, After::Close));
}

#[test]
fn an_unknown_command_is_refused_in_the_executives_own_words() {
    let mut s = Session::new(Config::default());
    let (f, _) = s.handle("wobble");
    assert!(f.json.contains("unknown-command"), "{}", f.json);
}

#[test]
fn the_executive_vocabulary_is_reachable_without_a_session() {
    // What `f1r3x-ffi` calls. One implementation, three surfaces.
    let mut x = Executive::new(f1r3x::Settings::default());
    assert!(proto::executive_command(&mut x, "status").is_some());
    assert!(proto::executive_command(&mut x, "deploy\n@{Nil}!(Nil)").is_some());
    // A hosting verb is not part of it, and says so by returning nothing.
    assert!(proto::executive_command(&mut x, "play").is_none());
}
