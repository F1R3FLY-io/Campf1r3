//! A very small JSON writer.
//!
//! The executive has no dependencies, so it carries its own. This is a writer
//! only: everything crossing the boundary to Swift goes out as JSON and comes
//! back as a handful of scalars, so no parser is needed.

use crate::scene::{Card, Scene};
use crate::{Diag, Step};

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn s(key: &str, v: &str) -> String {
    format!("\"{}\":\"{}\"", esc(key), esc(v))
}
fn n(key: &str, v: u64) -> String {
    format!("\"{}\":{}", esc(key), v)
}
fn b(key: &str, v: bool) -> String {
    format!("\"{}\":{}", esc(key), v)
}
fn arr(key: &str, items: Vec<String>) -> String {
    format!("\"{}\":[{}]", esc(key), items.join(","))
}
fn strs(key: &str, items: &[String]) -> String {
    arr(
        key,
        items.iter().map(|x| format!("\"{}\"", esc(x))).collect(),
    )
}

pub fn card(c: &Card) -> String {
    let mut f = vec![
        s("id", &c.id),
        s("kind", c.kind),
        s("label", &c.label),
        n("size", c.size as u64),
        n("depth", c.depth as u64),
    ];
    match &c.chan {
        Some(ch) => f.push(s("chan", ch)),
        None => f.push("\"chan\":null".to_string()),
    }
    if !c.group.is_empty() {
        f.push(strs("group", &c.group));
    }
    format!("{{{}}}", f.join(","))
}

pub fn scene(sc: &Scene) -> String {
    let f = vec![
        n("tick", sc.tick),
        b("quiescent", sc.quiescent),
        n("queued", sc.counts.0 as u64),
        n("data", sc.counts.1 as u64),
        n("waiting", sc.counts.2 as u64),
        n("comms", sc.counts.3),
        arr("rhobots", sc.rhobots.iter().map(card).collect()),
        arr("tuples", sc.tuples.iter().map(card).collect()),
        arr("listeners", sc.listeners.iter().map(card).collect()),
    ];
    format!("{{{}}}", f.join(","))
}

pub fn step(st: &Step) -> String {
    let f = match st {
        Step::Inert { term } => vec![s("kind", "inert"), s("term", &term.hex())],
        Step::Minted { count, first } => vec![
            s("kind", "mint"),
            n("count", *count as u64),
            s("first", &first.hex()),
        ],
        Step::Produced { chan, datum } => vec![
            s("kind", "produce"),
            s("chan", &chan.hex()),
            s("datum", &datum.hex()),
        ],
        Step::Consumed { group, cont } => vec![
            s("kind", "consume"),
            s("cont", &cont.hex()),
            strs(
                "group",
                &group.iter().map(|h| h.hex()).collect::<Vec<_>>(),
            ),
        ],
        Step::Comm {
            chan,
            cont,
            spawned,
        } => vec![
            s("kind", "comm"),
            s("chan", &chan.hex()),
            s("cont", &cont.hex()),
            n("spawned", *spawned as u64),
        ],
        Step::Reissued { chan } => vec![s("kind", "reissue"), s("chan", &chan.hex())],
        Step::Outbound { aperture, chan } => vec![
            s("kind", "outbound"),
            n("aperture", *aperture as u64),
            s("chan", &chan.hex()),
        ],
    };
    format!("{{{}}}", f.join(","))
}

pub fn steps(v: &[Step]) -> String {
    format!(
        "[{}]",
        v.iter().map(step).collect::<Vec<_>>().join(",")
    )
}

pub fn diag(d: &Diag) -> String {
    format!(
        "{{{},{},{},{}}}",
        n("lo", d.lo as u64),
        n("hi", d.hi as u64),
        s("code", &d.code),
        s("message", &d.message)
    )
}

pub fn error(diags: &[Diag]) -> String {
    format!(
        "{{\"ok\":false,{}}}",
        arr("diags", diags.iter().map(diag).collect())
    )
}

pub fn ok(body: &str) -> String {
    format!("{{\"ok\":true,\"result\":{body}}}")
}
