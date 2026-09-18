//! The scene, with the shape of each term on it.
//!
//! `f1r3x::scene::build` names the three populations and gives each card a
//! *label*: source-shaped text, which is right for a list and useless for a
//! renderer. This writes the same cards out as JSON and adds the one thing a
//! renderer needs and a label cannot give — the term's structure, from
//! `k1ndl1ng-norm`'s encoder.
//!
//! The walk is not repeated. `Card` carries the `Norm` it was built from, so a
//! card's `id` and its tree are guaranteed to describe one object; an earlier
//! version of this file walked the machine a second time to get the terms, and
//! the two walks agreeing was a property nobody could check.

use b3ll0ws::{Direct, Machine, Sequential};
use f1r3x::scene::{Card, Scene};
use k1ndl1ng_norm::json::{write_tree, Budget, Buf};

pub fn write(out: &mut Buf, sc: &Scene, _m: &Machine<Direct, Sequential>, budget: Budget) {
    out.raw("{");
    out.field_num("tick", sc.tick);
    out.comma();
    out.field_bool("quiescent", sc.quiescent);
    out.comma();
    out.field_num("queued", sc.counts.0 as u64);
    out.comma();
    out.field_num("data", sc.counts.1 as u64);
    out.comma();
    out.field_num("waiting", sc.counts.2 as u64);
    out.comma();
    out.field_num("comms", sc.counts.3);
    out.comma();
    population(out, "rhobots", &sc.rhobots, budget);
    out.comma();
    population(out, "tuples", &sc.tuples, budget);
    out.comma();
    population(out, "listeners", &sc.listeners, budget);
    out.raw("}");
}

fn population(out: &mut Buf, key: &str, cards: &[Card], budget: Budget) {
    out.key(key);
    out.raw("[");
    for (i, c) in cards.iter().enumerate() {
        if i > 0 {
            out.comma();
        }
        card(out, c, budget);
    }
    out.raw("]");
}

fn card(out: &mut Buf, c: &Card, budget: Budget) {
    out.raw("{");
    out.field_str("id", &c.id);
    out.comma();
    out.field_str("kind", c.kind);
    out.comma();
    out.field_str("label", &c.label);
    out.comma();
    out.field_num("size", c.size as u64);
    out.comma();
    out.field_num("depth", c.depth as u64);
    out.comma();
    match &c.chan {
        Some(ch) => out.field_str("chan", ch),
        None => out.field_raw("chan", "null"),
    }
    out.comma();
    match &c.chan_fp {
        Some(fp) => out.field_str("chanFp", fp),
        None => out.field_raw("chanFp", "null"),
    }
    if !c.group.is_empty() {
        out.comma();
        out.key("group");
        out.raw("[");
        for (i, g) in c.group.iter().enumerate() {
            if i > 0 {
                out.comma();
            }
            out.str(g);
        }
        out.raw("]");
    }
    out.comma();
    out.key("tree");
    write_tree(out, &c.term, budget);
    out.raw("}");
}

/// The channel identity a card sits on, for a consumer matching a commit-log
/// entry to the robots it consumed.
pub fn chan_fp(c: &Card) -> Option<&String> {
    c.chan_fp.as_ref()
}
