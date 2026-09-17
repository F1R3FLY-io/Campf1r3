//! The kernel spatial matcher: `m(p, a)` for K1ndl1ng normal forms.
//!
//! A receipt's pattern is a list of *names*; a send carries a list of
//! *processes*. `for(y <- x)P | x!(Q)` binds `y` to `@Q`, so the matcher is
//! always asked to match a name pattern against the name `@Q`.
//!
//! Holes are the two things the normaliser leaves behind in a pattern:
//! `Node::FreeVar(slot)` (a process variable, binds a process) and
//! `Name::Free(slot)` (a name variable, binds a name), plus `Node::Wild` and
//! its name form `@_`, which match and bind nothing. Pattern free levels are
//! local to their bind (spec §7), so the slots this module returns are local
//! too; the caller concatenates them across a receipt's binds, in bind order,
//! to get the body's de Bruijn group.
//!
//! Matching under `|` is a search: a pattern with `n` hole-bearing components
//! against `m` target components. Two things keep it cheap. Pattern components
//! with no holes are matched first, by encoding equality, which is a multiset
//! removal and needs no backtracking, since equal encodings are
//! interchangeable. Only what is left is searched, and the search is bounded
//! (`MAX_PAR_SEARCH`): a wider one returns no match rather than running for a
//! year. That bound is a v1 decision and is the matcher's only incompleteness.
//!
//! The machine is iterative — an explicit goal stack, a trail of bindings, and
//! a stack of choice frames — because pattern depth is user input.

use k1ndl1ng_norm::{Name, NBind, Node, Norm, Val};

/// The widest `|` search this matcher will attempt: hole-bearing pattern
/// components, and target components left after the ground ones are removed.
pub const MAX_PAR_SEARCH: usize = 8;
pub const MAX_PAR_TARGETS: usize = 16;

/// Match a receipt's pattern list against a send's argument list.
/// Returns the bindings in slot order, or `None`.
pub fn match_pats(pats: &[Name], args: &[Norm], binders: u16) -> Option<Vec<Val>> {
    if pats.len() != args.len() {
        return None;
    }
    let mut goals: Vec<Goal> = Vec::with_capacity(pats.len());
    // Pushed in reverse so that position 0 is examined first: the order does
    // not affect the result, only which mismatch is found soonest.
    for (p, a) in pats.iter().zip(args.iter()).rev() {
        goals.push(Goal::N(p.clone(), Name::Quote(a.clone())));
    }
    run(goals, binders as usize)
}

/// Match a single name pattern against a single name.
pub fn match_name(pat: &Name, tgt: &Name, binders: u16) -> Option<Vec<Val>> {
    run(vec![Goal::N(pat.clone(), tgt.clone())], binders as usize)
}

/// Match a single process pattern against a process.
pub fn match_proc(pat: &Norm, tgt: &Norm, binders: u16) -> Option<Vec<Val>> {
    run(vec![Goal::P(pat.clone(), tgt.clone())], binders as usize)
}

// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Goal {
    P(Norm, Norm),
    N(Name, Name),
}

struct Frame {
    goals: Vec<Goal>,
    trail_len: usize,
    pats: Vec<Norm>,
    tgts: Vec<Norm>,
    /// A bare `FreeVar` or `Wild` component absorbing whatever is left over.
    rem: Option<Option<u32>>,
    choice: Vec<usize>,
    first: bool,
}

fn run(mut goals: Vec<Goal>, binders: usize) -> Option<Vec<Val>> {
    let mut binds: Vec<Option<Val>> = vec![None; binders];
    let mut trail: Vec<usize> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();

    'search: loop {
        // Work the goal stack until it empties or something fails.
        let ok = loop {
            let g = match goals.pop() {
                None => break true,
                Some(g) => g,
            };
            if !step(g, &mut goals, &mut binds, &mut trail, &mut frames) {
                break false;
            }
        };
        if ok {
            // Every slot a well-formed pattern declares is reached on some
            // structural path, so an unbound slot means a malformed pattern;
            // `Nil` keeps the function total rather than panicking.
            return Some(
                binds
                    .into_iter()
                    .map(|b| b.unwrap_or(Val::Proc(Norm::nil())))
                    .collect(),
            );
        }
        // Backtrack to the most recent frame with another assignment.
        loop {
            let f = match frames.last_mut() {
                None => return None,
                Some(f) => f,
            };
            let k = f.pats.len();
            let n = f.tgts.len();
            let advanced = bump(&mut f.choice, n, k, f.first);
            f.first = false;
            if !advanced {
                frames.pop();
                continue;
            }
            // Restore the state saved at the choice point.
            while trail.len() > f.trail_len {
                if let Some(s) = trail.pop() {
                    binds[s] = None;
                }
            }
            goals = f.goals.clone();
            if !emit_assignment(f, &mut goals) {
                continue;
            }
            continue 'search;
        }
    }
}

/// Resolve one goal. `false` means this branch fails.
fn step(
    g: Goal,
    goals: &mut Vec<Goal>,
    binds: &mut [Option<Val>],
    trail: &mut Vec<usize>,
    frames: &mut Vec<Frame>,
) -> bool {
    match g {
        Goal::N(pat, tgt) => match pat {
            Name::Free(slot) => bind(slot as usize, Val::Name(tgt), binds, trail),
            Name::Quote(pp) => {
                if matches!(pp.node(), Node::Wild) {
                    return true;
                }
                match tgt {
                    Name::Quote(tq) => {
                        goals.push(Goal::P(pp, tq));
                        true
                    }
                    _ => false,
                }
            }
            other => other == tgt,
        },
        Goal::P(pat, tgt) => match pat.node() {
            Node::Wild => true,
            Node::FreeVar(slot) => bind(*slot as usize, Val::Proc(tgt), binds, trail),
            _ if !has_holes(&pat) => pat.encode() == tgt.encode(),
            Node::Nil => tgt.is_nil(),
            Node::BoundVar(_) => pat.encode() == tgt.encode(),
            // `*y` with `y` a pattern binder matches any process `P`, binding
            // `y` to `@P` — `*@P` is `P`, so the two sides agree. Only when the
            // unquoted name is itself concrete does the target have to be an
            // `Eval` too.
            Node::Eval(Name::Free(slot)) => {
                bind(*slot as usize, Val::Name(Name::Quote(tgt)), binds, trail)
            }
            Node::Eval(n) => match tgt.node() {
                Node::Eval(tn) => {
                    goals.push(Goal::N(n.clone(), tn.clone()));
                    true
                }
                _ => false,
            },
            Node::New { count, body } => match tgt.node() {
                Node::New {
                    count: tc,
                    body: tb,
                } if tc == count => {
                    goals.push(Goal::P(body.clone(), tb.clone()));
                    true
                }
                _ => false,
            },
            Node::Send {
                chan,
                persistent,
                args,
            } => match tgt.node() {
                Node::Send {
                    chan: tchan,
                    persistent: tp,
                    args: targs,
                } if tp == persistent && targs.len() == args.len() => {
                    goals.push(Goal::N(chan.clone(), tchan.clone()));
                    for (a, t) in args.iter().zip(targs.iter()) {
                        goals.push(Goal::P(a.clone(), t.clone()));
                    }
                    true
                }
                _ => false,
            },
            Node::Receive { binds: pb, body } => match tgt.node() {
                Node::Receive {
                    binds: tb,
                    body: tbody,
                } if tb.len() == pb.len() => {
                    // Binds are compared in stored (encoding) order. A hole in a
                    // bind channel can in principle re-order the two lists
                    // differently; matching a receipt whose *channels* are
                    // patterns is left to v1.1, and this is where it would go.
                    for (p, t) in pb.iter().zip(tb.iter()) {
                        if !bind_shapes_agree(p, t) {
                            return false;
                        }
                        goals.push(Goal::N(p.chan.clone(), t.chan.clone()));
                    }
                    goals.push(Goal::P(body.clone(), tbody.clone()));
                    true
                }
                _ => false,
            },
            Node::Par(ps) => {
                let mut pats: Vec<Norm> = Vec::new();
                let mut rem: Option<Option<u32>> = None;
                for c in ps {
                    match c.node() {
                        // A bare hole at the top of a `|` pattern takes the
                        // leftovers, as a rholang remainder would.
                        Node::Wild if rem.is_none() => rem = Some(None),
                        Node::FreeVar(s) if rem.is_none() => rem = Some(Some(*s)),
                        _ => pats.push(c.clone()),
                    }
                }
                let mut tgts = tgt.top_level();
                // Ground components first: encoding equality, no backtracking.
                let mut i = 0;
                while i < pats.len() {
                    if has_holes(&pats[i]) {
                        i += 1;
                        continue;
                    }
                    match tgts.iter().position(|t| t.encode() == pats[i].encode()) {
                        None => return false,
                        Some(j) => {
                            tgts.remove(j);
                            pats.remove(i);
                        }
                    }
                }
                if rem.is_none() && tgts.len() != pats.len() {
                    return false;
                }
                if pats.len() > tgts.len() {
                    return false;
                }
                if pats.len() > MAX_PAR_SEARCH || tgts.len() > MAX_PAR_TARGETS {
                    return false;
                }
                let mut f = Frame {
                    goals: goals.clone(),
                    trail_len: trail.len(),
                    pats,
                    tgts,
                    rem,
                    choice: Vec::new(),
                    first: true,
                };
                let k = f.pats.len();
                let n = f.tgts.len();
                if !bump(&mut f.choice, n, k, true) {
                    return false;
                }
                f.first = false;
                if !emit_assignment(&f, goals) {
                    // No assignment works at all for this shape; still push the
                    // frame so the backtracker can try the next one.
                    frames.push(f);
                    return false;
                }
                frames.push(f);
                true
            }
        },
    }
}

/// Turn the frame's current assignment into goals.
fn emit_assignment(f: &Frame, goals: &mut Vec<Goal>) -> bool {
    for (i, p) in f.pats.iter().enumerate() {
        let j = match f.choice.get(i) {
            None => return false,
            Some(j) => *j,
        };
        let t = match f.tgts.get(j) {
            None => return false,
            Some(t) => t.clone(),
        };
        goals.push(Goal::P(p.clone(), t));
    }
    if let Some(slot) = f.rem {
        let left: Vec<Norm> = f
            .tgts
            .iter()
            .enumerate()
            .filter(|(j, _)| !f.choice.contains(j))
            .map(|(_, t)| t.clone())
            .collect();
        if let Some(s) = slot {
            // The remainder is a process variable: it takes the par of what is
            // left, which is `Nil` when nothing is.
            goals.push(Goal::P(Norm::free_var(s), Norm::par(left)));
        }
    }
    true
}

fn bind(slot: usize, v: Val, binds: &mut [Option<Val>], trail: &mut Vec<usize>) -> bool {
    if slot >= binds.len() {
        return false;
    }
    match &binds[slot] {
        // A slot occurs once in a well-formed pattern; if it occurs twice the
        // two sightings must agree, which is the non-linear reading.
        Some(old) => same_val(old, &v),
        None => {
            binds[slot] = Some(v);
            trail.push(slot);
            true
        }
    }
}

fn same_val(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Name(x), Val::Name(y)) => x == y,
        (Val::Proc(x), Val::Proc(y)) => x.encode() == y.encode(),
        _ => false,
    }
}

fn bind_shapes_agree(p: &NBind, t: &NBind) -> bool {
    p.kind == t.kind
        && p.pats.len() == t.pats.len()
        && p.binders == t.binders
        && p.pats
            .iter()
            .zip(t.pats.iter())
            .all(|(a, b)| a.encode() == b.encode())
}

/// Does this term contain a hole? Iterative, and the answer is cheap because
/// most pattern subterms are small.
pub fn has_holes(p: &Norm) -> bool {
    let mut procs = vec![p.clone()];
    let mut names: Vec<Name> = Vec::new();
    loop {
        while let Some(n) = names.pop() {
            match n {
                Name::Free(_) => return true,
                Name::Quote(q) => procs.push(q),
                _ => {}
            }
        }
        let cur = match procs.pop() {
            None => return false,
            Some(c) => c,
        };
        match cur.node() {
            Node::Wild | Node::FreeVar(_) => return true,
            Node::Nil | Node::BoundVar(_) => {}
            Node::Par(v) => procs.extend(v.iter().cloned()),
            Node::Send { chan, args, .. } => {
                names.push(chan.clone());
                procs.extend(args.iter().cloned());
            }
            Node::Receive { binds, body } => {
                for b in binds {
                    names.push(b.chan.clone());
                    for q in &b.pats {
                        names.push(q.clone());
                    }
                }
                procs.push(body.clone());
            }
            Node::New { body, .. } => procs.push(body.clone()),
            Node::Eval(n) => names.push(n.clone()),
        }
    }
}

/// Next injective assignment of `k` pattern positions into `n` targets, in
/// lexicographic order. `first` asks for the first one.
fn bump(choice: &mut Vec<usize>, n: usize, k: usize, first: bool) -> bool {
    let mut start;
    if first {
        choice.clear();
        if k == 0 {
            return true;
        }
        start = 0;
    } else {
        if k == 0 {
            return false;
        }
        start = match choice.pop() {
            Some(v) => v + 1,
            None => return false,
        };
    }
    loop {
        let mut v = start;
        let mut placed = false;
        while v < n {
            if !choice.contains(&v) {
                choice.push(v);
                placed = true;
                break;
            }
            v += 1;
        }
        if placed {
            if choice.len() == k {
                return true;
            }
            start = 0;
        } else {
            match choice.pop() {
                Some(x) => start = x + 1,
                None => return false,
            }
        }
    }
}
