//! The scene: what the headset draws.
//!
//! Three populations, which is the whole Rhobots metaphor. **Rhobots** are
//! components waiting to run — the things that move. **Tuples** are data at
//! rest on a channel — the things that sit. **Listeners** are continuations at
//! rest over a group of channels — the things that wait. A comm is a rhobot
//! meeting a tuple at a listener, and every one of them carries the 32-byte
//! identity the store keys by, so the renderer can follow one object across
//! frames without the executive inventing handles.

use b3ll0ws::{Direct, Machine, Sequential};
use campf1r3_core::Keyed;
use k1ndl1ng_campf1r3::Chan;
use k1ndl1ng_norm::{show_pretty, Hash32, Name, Norm, Subst, Val};

/// One drawable thing.
#[derive(Clone, Debug)]
pub struct Card {
    /// The content hash, hex. Stable across frames and across runs.
    pub id: String,
    /// What it is: `rhobot`, `tuple`, or `listener`.
    pub kind: &'static str,
    /// The channel it sits on, if any.
    pub chan: Option<String>,
    /// Channel names for a listener's whole group.
    pub group: Vec<String>,
    /// Source-shaped text for the label.
    pub label: String,
    /// Size of the underlying term: how big to draw it.
    pub size: u32,
    /// Nesting depth of the underlying term.
    pub depth: u32,
}

#[derive(Clone, Debug)]
pub struct Scene {
    pub tick: u64,
    pub rhobots: Vec<Card>,
    pub tuples: Vec<Card>,
    pub listeners: Vec<Card>,
    pub quiescent: bool,
    /// Components queued, data at rest, continuations at rest, comms so far.
    pub counts: (usize, usize, usize, u64),
}

/// How a channel reads on a label. An unforgeable has no source form, so it is
/// shown by the first four bytes of its hash, which is also how the store
/// tells two of them apart.
pub fn name_label(n: &Name) -> String {
    match n {
        Name::Quote(p) => format!("@{{{}}}", show_pretty(p)),
        // The printer names an unforgeable by the first four bytes of its key,
        // not by the hash of its encoding. Follow it exactly: a channel that
        // reads `u_3375c662` in a card's `chan` must read the same inside the
        // card's label, or the renderer is drawing two names for one channel.
        Name::Unforgeable(b) => {
            format!("u_{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3])
        }
        Name::Bound(i) => format!("n{i}"),
        Name::Free(l) => format!("f{l}"),
    }
}

fn term_card(kind: &'static str, t: &Norm, chan: Option<&Chan>) -> Card {
    Card {
        id: t.hash().hex(),
        kind,
        chan: chan.map(|c| name_label(c.name())),
        group: Vec::new(),
        label: show_pretty(t),
        size: t.size(),
        depth: t.depth(),
    }
}

pub fn build(m: &Machine<Direct, Sequential>, tick: u64) -> Scene {
    let rhobots: Vec<Card> = m.jobs().map(|j| term_card("rhobot", &j.term, None)).collect();

    let chans = m.channels();
    let mut tuples: Vec<Card> = Vec::new();
    for c in &chans {
        let mut at = 0usize;
        for d in m.space().get_data(c) {
            // A datum is an argument list; show it as the send it came from.
            let term = Norm::send(c.name().clone(), false, d.args.clone());
            let mut card = term_card("tuple", &term, Some(c));
            // The store keys a datum by its content alone, so the same tuple on
            // two channels has one hash. A card has to be one drawable object,
            // so its id carries the channel and the position in the row too.
            card.id = format!(
                "{}@{}#{}",
                hex(d.content_hash()),
                &hex(c.content_hash())[..8],
                at
            );
            at += 1;
            tuples.push(card);
        }
    }

    let mut listeners: Vec<Card> = Vec::new();
    for c in &chans {
        // Singleton groups, then every join this channel is a member of. The
        // store returns a join once per member, so only take it at its first.
        let mut groups: Vec<Vec<Chan>> = vec![vec![c.clone()]];
        for g in m.space().get_joins(c) {
            if g.first() == Some(c) && g.len() > 1 {
                groups.push(g);
            }
        }
        for g in groups {
            let mut at = 0usize;
            for (pats, k, persist) in m.space().get_waiting(&g) {
                // Show the body with its binders standing where the pattern's
                // do, so the label reads as one `for` rather than as a body
                // full of loose de Bruijn indices.
                let shown = if k.binders > 0 {
                    let slots: Vec<Val> = (0..k.binders as u32)
                        .map(|s| Val::Name(Name::Free(s)))
                        .collect();
                    k.body.substitute(&Subst::from_slots(slots))
                } else {
                    k.body.clone()
                };
                let mut card = term_card("listener", &shown, Some(c));
                card.id = format!(
                    "{}@{}#{}",
                    hex(k.content_hash()),
                    &hex(c.content_hash())[..8],
                    at
                );
                at += 1;
                card.group = g.iter().map(|x| name_label(x.name())).collect();
                card.label = format!(
                    "for({}){{ {} }}",
                    g.iter()
                        .zip(pats.iter())
                        .map(|(ch, p)| format!(
                            "{} {} {}",
                            p.pats
                                .iter()
                                .map(name_label)
                                .collect::<Vec<_>>()
                                .join(", "),
                            p.kind.arrow(),
                            name_label(ch.name())
                        ))
                        .collect::<Vec<_>>()
                        .join(" & "),
                    card.label
                );
                if persist {
                    card.label.push_str("  // persistent");
                }
                listeners.push(card);
            }
        }
    }

    // Deterministic order, so two runs draw the same frame.
    tuples.sort_by(|a, b| a.id.cmp(&b.id));
    listeners.sort_by(|a, b| a.id.cmp(&b.id));

    let (data, conts) = m.space().stats();
    let (_, comms) = m.counters();
    Scene {
        tick,
        quiescent: m.is_quiescent(),
        counts: (rhobots.len(), data, conts, comms),
        rhobots,
        tuples,
        listeners,
    }
}

fn hex(h: campf1r3_core::Hash32) -> String {
    Hash32(h.0).hex()
}
