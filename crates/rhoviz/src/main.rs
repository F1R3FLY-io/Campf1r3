//! `rhoviz` — draw a rho term and animate its reduction.
//!
//!     rhoviz <file.rho | - | -e 'TERM'> [-n STEPS] [-o out.html]
//!
//! The term is deployed to the `f1r3x` executive at one core in lockstep, so
//! exactly one component advances per tick. After every tick the whole world
//! is read back as one parallel composition: the components still queued, the
//! data at rest (as sends) and the continuations at rest (as receipts). A
//! frame is recorded whenever that composition changes, and the run stops
//! after `STEPS` comm events or at quiescence.
//!
//! Each frame is laid out by the *chromatic realisation* of the term: the
//! constructor is a colour, the argument position is one of three orthogonal
//! spatial axes, and the children of a node sit at distance
//! `sqrt(2) * r_max + 1` along those axes, where `r_max` is the largest child
//! radius. That figure is an embedding of the syntax tree in R^3: no two
//! spokes touch except at a shared hub. The output is one self-contained HTML
//! file with no external resources.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Read;
use std::process::ExitCode;

use b3ll0ws::{Direct, Machine, Sequential};
use f1r3x::{Executive, Mode, Settings, Step};
use k1ndl1ng_campf1r3::Chan;
use k1ndl1ng_norm::{show_pretty, NBind, Name, Node, Norm};

const VIEWER: &str = include_str!("viewer.html");
const GAP: f64 = 1.0;
/// A guard against a term that ticks forever without a comm.
const MAX_TICKS: u64 = 1_000_000;

// ---- the drawable tree ------------------------------------------------------

/// One node of the figure. At most three children, so three axes suffice.
struct V {
    kind: &'static str,
    label: String,
    kids: Vec<V>,
}

fn leaf(kind: &'static str, label: impl Into<String>) -> V {
    V { kind, label: label.into(), kids: Vec::new() }
}

/// A list of two or more things as a right-nested chain of `,` nodes.
fn chain(mut xs: Vec<V>) -> V {
    let mut acc = xs.pop().expect("chain of nothing");
    while let Some(x) = xs.pop() {
        acc = V { kind: "list", label: ",".into(), kids: vec![x, acc] };
    }
    acc
}

/// Parallel composition is associative, so the bracketing is ours to choose.
/// A balanced tree keeps the height, and so the radius, logarithmic in the
/// number of components where a right-nested comb would make it linear.
fn balanced(mut xs: Vec<V>) -> V {
    match xs.len() {
        0 => leaf("nil", "Nil"),
        1 => xs.pop().unwrap(),
        n => {
            let right = xs.split_off(n.div_ceil(2));
            V { kind: "par", label: "|".into(), kids: vec![balanced(xs), balanced(right)] }
        }
    }
}

fn name_tree(n: &Name) -> V {
    match n {
        Name::Quote(p) => V { kind: "quote", label: "@".into(), kids: vec![proc_tree(p)] },
        Name::Bound(i) => leaf("var", format!("n{i}")),
        Name::Free(l) => leaf("var", format!("f{l}")),
        Name::Unforgeable(b) => {
            leaf("unf", format!("u_{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3]))
        }
    }
}

fn proc_tree(t: &Norm) -> V {
    match t.node() {
        Node::Nil => leaf("nil", "Nil"),
        Node::Par(v) => balanced(v.iter().map(proc_tree).collect()),
        Node::Send { chan, persistent, args } => {
            let mut kids = vec![name_tree(chan)];
            if !args.is_empty() {
                kids.push(chain(args.iter().map(proc_tree).collect()));
            }
            V { kind: "send", label: if *persistent { "!!" } else { "!" }.into(), kids }
        }
        Node::Receive { binds, body } => {
            // A join is a chain of receipt nodes, one per bind, ending in the
            // body: (patterns, channel, rest). A single bind is (y, x, P).
            let mut acc = proc_tree(body);
            for (i, b) in binds.iter().enumerate().rev() {
                let pats = if b.pats.is_empty() {
                    leaf("var", "()")
                } else {
                    chain(b.pats.iter().map(name_tree).collect())
                };
                let head = if i == 0 { "for" } else { "&" };
                acc = V {
                    kind: "recv",
                    label: format!("{head} {}", b.kind.arrow()),
                    kids: vec![pats, name_tree(&b.chan), acc],
                };
            }
            acc
        }
        Node::New { count, body } => {
            V { kind: "new", label: format!("new {count}"), kids: vec![proc_tree(body)] }
        }
        Node::Eval(n) => V { kind: "drop", label: "*".into(), kids: vec![name_tree(n)] },
        Node::BoundVar(i) => leaf("var", format!("p{i}")),
        Node::FreeVar(l) => leaf("var", format!("P{l}")),
        Node::Wild => leaf("var", "_"),
    }
}

// ---- the chromatic realisation ----------------------------------------------

/// One placed hub: identity, colour, label, position, parent index, flags.
struct Hub {
    id: String,
    kind: &'static str,
    label: String,
    pos: [f64; 3],
    parent: i64,
    flag: u8,
}

/// Radius of the figure about its hub: 0 for a leaf, else `lambda + r_max`.
fn radius(v: &V) -> f64 {
    if v.kids.is_empty() {
        return 0.0;
    }
    let rmax = v.kids.iter().map(radius).fold(0.0, f64::max);
    2f64.sqrt() * rmax + GAP + rmax
}

/// Place `v` with its hub at `at`; child `i` goes along axis `i`.
fn place(v: &V, id: String, at: [f64; 3], parent: i64, flag: u8, out: &mut Vec<Hub>) {
    let me = out.len() as i64;
    out.push(Hub { id: id.clone(), kind: v.kind, label: v.label.clone(), pos: at, parent, flag });
    if v.kids.is_empty() {
        return;
    }
    let rmax = v.kids.iter().map(radius).fold(0.0, f64::max);
    let lambda = 2f64.sqrt() * rmax + GAP;
    for (i, k) in v.kids.iter().enumerate() {
        let mut p = at;
        p[i] += lambda;
        place(k, format!("{id}.{i}"), p, me, flag, out);
    }
}

// ---- reading the world back as one term -------------------------------------

/// Everything alive, as parallel components: queued jobs, data at rest shown
/// as the sends they came from, continuations at rest shown as receipts.
fn world(m: &Machine<Direct, Sequential>) -> Vec<Norm> {
    let mut out: Vec<Norm> = Vec::new();
    for j in m.jobs() {
        out.extend(j.term.top_level());
    }
    let chans = m.channels();
    for c in &chans {
        for d in m.space().get_data(c) {
            out.push(Norm::send(c.name().clone(), false, d.args.clone()));
        }
    }
    for c in &chans {
        let mut groups: Vec<Vec<Chan>> = vec![vec![c.clone()]];
        for g in m.space().get_joins(c) {
            if g.first() == Some(c) && g.len() > 1 {
                groups.push(g);
            }
        }
        for g in groups {
            for (pats, k, _persist) in m.space().get_waiting(&g) {
                let binds: Vec<NBind> = g
                    .iter()
                    .zip(pats.iter())
                    .map(|(ch, p)| NBind::new(p.kind, ch.name().clone(), p.pats.clone(), p.binders))
                    .collect();
                out.push(Norm::receive(binds, k.body.clone()));
            }
        }
    }
    out
}

/// One recorded state: components in display order, each with a stable id.
struct Frame {
    comps: Vec<(String, Norm)>,
    note: String,
}

/// Give each component an id that survives across frames: its content hash,
/// plus an occurrence number so that two equal components are two objects.
fn identify(terms: Vec<Norm>) -> Vec<(String, Norm)> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut out: Vec<(String, Norm)> = terms
        .into_iter()
        .map(|t| (t.hash().hex(), t))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    for (h, _) in out.iter_mut() {
        let n = seen.entry(h.clone()).or_insert(0);
        let id = format!("{}#{}", &h[..16], *n);
        *n += 1;
        *h = id;
    }
    out
}

/// Keep survivors where they were and put arrivals where the first departure
/// was, so that a component which did not take part in a step does not jump.
fn reorder(prev: &[(String, Norm)], next: Vec<(String, Norm)>) -> Vec<(String, Norm)> {
    let mut by_id: HashMap<String, Norm> = next.into_iter().collect();
    let mut out: Vec<(String, Norm)> = Vec::new();
    let mut hole: Option<usize> = None;
    for (id, _) in prev {
        match by_id.remove(id) {
            Some(t) => out.push((id.clone(), t)),
            None => {
                hole.get_or_insert(out.len());
            }
        }
    }
    let mut fresh: Vec<(String, Norm)> = by_id.into_iter().collect();
    fresh.sort_by(|a, b| a.0.cmp(&b.0));
    let at = hole.unwrap_or(out.len());
    out.splice(at..at, fresh);
    out
}

fn same_ids(a: &[(String, Norm)], b: &[(String, Norm)]) -> bool {
    let x: HashSet<&String> = a.iter().map(|c| &c.0).collect();
    let y: HashSet<&String> = b.iter().map(|c| &c.0).collect();
    x == y
}

// ---- output ----------------------------------------------------------------

fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn frame_json(f: &Frame, prev: Option<&Frame>, next: Option<&Frame>, out: &mut String) {
    let ids = |g: Option<&Frame>| -> Option<HashSet<String>> {
        g.map(|g| g.comps.iter().map(|c| c.0.clone()).collect())
    };
    let (before, after) = (ids(prev), ids(next));
    // Lay out the top-level composition as one balanced par over the components.
    // Each component keeps its own id prefix, so its hubs are the same objects
    // in every frame it survives; the par spine is identified by position.
    struct Slot<'a> {
        id: &'a str,
        tree: V,
        flag: u8,
    }
    let slots: Vec<Slot> = f
        .comps
        .iter()
        .map(|(id, t)| {
            let leaving = after.as_ref().is_some_and(|s| !s.contains(id));
            let arrived = before.as_ref().is_some_and(|s| !s.contains(id));
            Slot { id, tree: proc_tree(t), flag: leaving as u8 | (arrived as u8) << 1 }
        })
        .collect();
    let mut hubs: Vec<Hub> = Vec::new();
    fn spine(slots: &[Slot], path: String, at: [f64; 3], parent: i64, out: &mut Vec<Hub>) {
        match slots.len() {
            0 => out.push(Hub { id: "top".into(), kind: "nil", label: "Nil".into(), pos: at, parent, flag: 0 }),
            1 => place(&slots[0].tree, slots[0].id.to_string(), at, parent, slots[0].flag, out),
            n => {
                let (l, r) = slots.split_at(n.div_ceil(2));
                let rad = |s: &[Slot]| radius(&balanced_shape(s));
                let rmax = rad(l).max(rad(r));
                let lambda = 2f64.sqrt() * rmax + GAP;
                let me = out.len() as i64;
                out.push(Hub { id: format!("top{path}"), kind: "par", label: "|".into(), pos: at, parent, flag: 0 });
                for (i, half) in [l, r].into_iter().enumerate() {
                    let mut p = at;
                    p[i] += lambda;
                    spine(half, format!("{path}.{i}"), p, me, out);
                }
            }
        }
    }
    /// The shape of a balanced par over `slots`, for measuring only.
    fn balanced_shape(slots: &[Slot]) -> V {
        fn copy(v: &V) -> V {
            V { kind: v.kind, label: String::new(), kids: v.kids.iter().map(copy).collect() }
        }
        balanced(slots.iter().map(|s| copy(&s.tree)).collect())
    }
    spine(&slots, String::new(), [0.0; 3], -1, &mut hubs);

    let text = if f.comps.is_empty() {
        "Nil".to_string()
    } else {
        f.comps.iter().map(|c| show_pretty(&c.1)).collect::<Vec<_>>().join("\n| ")
    };
    out.push_str("{\"t\":");
    json_str(&text, out);
    out.push_str(",\"note\":");
    json_str(&f.note, out);
    out.push_str(",\"n\":[");
    for (i, h) in hubs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        json_str(&h.id, out);
        out.push(',');
        json_str(h.kind, out);
        out.push(',');
        json_str(&h.label, out);
        let _ = write!(
            out,
            ",{:.4},{:.4},{:.4},{},{}]",
            h.pos[0], h.pos[1], h.pos[2], h.parent, h.flag
        );
    }
    out.push_str("]}");
}

// ---- driver ----------------------------------------------------------------

struct Opts {
    source: String,
    steps: u64,
    out: String,
}

fn usage() {
    eprintln!(
        "rhoviz — draw a rho term in three dimensions and animate its reduction

  rhoviz <file.rho | -> [-n STEPS] [-o OUT.html]
  rhoviz -e 'TERM'      [-n STEPS] [-o OUT.html]

  -n, --steps N   stop after N comm events (default 10); 0 draws the term only
  -o, --out FILE  where to write the animation (default rho.html)
  -e TERM         take the term from the command line"
    );
}

fn parse_args() -> Result<Opts, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut source, mut steps, mut out) = (None, 10u64, "rho.html".to_string());
    let mut i = 0;
    let need = |i: usize| args.get(i + 1).cloned().ok_or(format!("{} needs a value", args[i]));
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Err(String::new()),
            "-n" | "--steps" => {
                steps = need(i)?.parse().map_err(|_| "-n takes a whole number".to_string())?;
                i += 1;
            }
            "-o" | "--out" => {
                out = need(i)?;
                i += 1;
            }
            "-e" => {
                source = Some(need(i)?);
                i += 1;
            }
            "-" => {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
                source = Some(s);
            }
            p if !p.starts_with('-') => {
                source = Some(std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?);
            }
            other => return Err(format!("unknown option `{other}`")),
        }
        i += 1;
    }
    Ok(Opts { source: source.ok_or("no term given")?, steps, out })
}

fn main() -> ExitCode {
    let o = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("rhoviz: {e}\n");
            }
            usage();
            return ExitCode::from(2);
        }
    };

    let mut exec = Executive::new(Settings { cores: 1, mode: Mode::Lockstep, ..Settings::default() });
    if let Err(e) = exec.deploy(&o.source) {
        for d in e.diags {
            eprintln!("rhoviz: {}..{}: {} {}", d.lo, d.hi, d.code, d.message);
        }
        return ExitCode::FAILURE;
    }

    let mut frames = vec![Frame { comps: identify(world(exec.machine())), note: "the term as deployed".into() }];
    let (mut comms, mut ticks) = (0u64, 0u64);
    while comms < o.steps && !exec.is_quiescent() && ticks < MAX_TICKS {
        ticks += 1;
        let steps = match exec.tick() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rhoviz: the store refused a step: {e:?}");
                return ExitCode::FAILURE;
            }
        };
        let fired = steps.iter().filter(|s| matches!(s, Step::Comm { .. })).count() as u64;
        let minted: u32 = steps
            .iter()
            .map(|s| if let Step::Minted { count, .. } = s { *count } else { 0 })
            .sum();
        comms += fired;
        let now = identify(world(exec.machine()));
        let last = &frames.last().unwrap().comps;
        if same_ids(last, &now) {
            continue;
        }
        let note = if fired > 0 {
            format!("comm {comms}: a send met a receipt")
        } else if minted > 0 {
            format!("new: {minted} fresh name{} minted", if minted == 1 { "" } else { "s" })
        } else {
            "structural step".to_string()
        };
        let comps = reorder(last, now);
        frames.push(Frame { comps, note });
    }
    let status = if exec.is_quiescent() {
        format!("quiescent after {comms} comm{}", if comms == 1 { "" } else { "s" })
    } else {
        format!("stopped after {comms} comm{} with work still queued", if comms == 1 { "" } else { "s" })
    };

    let mut data = String::from("{\"status\":");
    json_str(&status, &mut data);
    data.push_str(",\"frames\":[");
    for i in 0..frames.len() {
        if i > 0 {
            data.push(',');
        }
        let prev = if i > 0 { Some(&frames[i - 1]) } else { None };
        frame_json(&frames[i], prev, frames.get(i + 1), &mut data);
    }
    data.push_str("]}");

    let html = VIEWER.replace("/*DATA*/null", &data);
    if let Err(e) = std::fs::write(&o.out, html) {
        eprintln!("rhoviz: {}: {e}", o.out);
        return ExitCode::FAILURE;
    }
    for (i, f) in frames.iter().enumerate() {
        let text: Vec<String> = f
            .comps
            .iter()
            .map(|c| show_pretty(&c.1).split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        println!("[{i}] {}\n    {}", f.note, if text.is_empty() { "Nil".into() } else { text.join(" | ") });
    }
    println!("{status}; {} frames written to {}", frames.len(), o.out);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames_of(src: &str, steps: u64) -> Vec<Vec<Hub>> {
        let mut exec = Executive::new(Settings { cores: 1, mode: Mode::Lockstep, ..Settings::default() });
        exec.deploy(src).expect("deploys");
        let mut out = Vec::new();
        let mut comms = 0;
        loop {
            let mut hubs = Vec::new();
            let trees: Vec<V> = world(exec.machine()).iter().map(proc_tree).collect();
            place(&balanced(trees), "t".into(), [0.0; 3], -1, 0, &mut hubs);
            out.push(hubs);
            if comms >= steps || exec.is_quiescent() {
                return out;
            }
            let s = exec.tick().expect("ticks");
            comms += s.iter().filter(|s| matches!(s, Step::Comm { .. })).count() as u64;
        }
    }

    /// The figure is an embedding: hubs are pairwise distinct, every spoke is
    /// parallel to one axis, and no hub sits in the interior of a spoke.
    #[test]
    fn figure_is_an_embedding() {
        let src = "new x in { for(y <- x){ x!(*y) | *y } | x!( for(y <- x){ x!(*y) | *y } | stdout!(Nil) ) }";
        for hubs in frames_of(src, 5) {
            for (i, a) in hubs.iter().enumerate() {
                for b in &hubs[i + 1..] {
                    let d: f64 = (0..3).map(|k| (a.pos[k] - b.pos[k]).powi(2)).sum();
                    assert!(d > 1e-9, "two hubs coincide");
                }
                if a.parent < 0 {
                    continue;
                }
                let p = &hubs[a.parent as usize];
                let moved: Vec<usize> = (0..3).filter(|&k| (a.pos[k] - p.pos[k]).abs() > 1e-9).collect();
                assert_eq!(moved.len(), 1, "a spoke leaves its axis");
                let k = moved[0];
                for c in &hubs {
                    let on_line = (0..3).all(|j| j == k || (c.pos[j] - p.pos[j]).abs() < 1e-9);
                    let inside = c.pos[k] > p.pos[k] + 1e-9 && c.pos[k] < a.pos[k] - 1e-9;
                    assert!(!(on_line && inside), "a hub lies inside a spoke");
                }
            }
        }
    }

    #[test]
    fn relay_takes_three_comms() {
        let src = "new a, b, c in { a!(Nil) | for(_ <- a){ b!(Nil) } | for(_ <- b){ c!(Nil) } | for(_ <- c){ stdout!(Nil) } }";
        let mut exec = Executive::new(Settings { cores: 1, mode: Mode::Lockstep, ..Settings::default() });
        exec.deploy(src).unwrap();
        let steps = exec.run_to_rest().unwrap();
        assert_eq!(steps.iter().filter(|s| matches!(s, Step::Comm { .. })).count(), 3);
        assert_eq!(world(exec.machine()).len(), 1);
    }
}
