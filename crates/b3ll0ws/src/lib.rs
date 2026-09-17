//! `b3ll0ws` — the interpreter.
//!
//! Per the bytecode analysis, v1 executes the normal form directly: there is no
//! bytecode and no VM between `k1ndl1ng-norm` and `campf1r3-core`. What the
//! analysis asked for instead is kept: a bounded cache of *plans*, keyed by the
//! continuation's content hash, and a `Backend` seam whose unit of work is a
//! prepared continuation, so a compiled backend can be dropped in later without
//! the machine above it changing.
//!
//! The machine is a work queue of top-level components. A `Send` produces, a
//! `Receive` consumes, a `New` mints, and a comm result substitutes the
//! bindings into the continuation and spawns whatever that yields. The store's
//! re-issue contract is honoured here and nowhere else: an arriving persistent
//! record that reacts was never stored, so the machine re-issues it until it
//! comes to rest.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};

use campf1r3_core::{Config, Keyed, PeekSet, SpaceError};
use k1ndl1ng_campf1r3::{BindPat, Chan, Cont, Data, RSpace};
use k1ndl1ng_norm::{BindKind, BinderPath, Hash32, NBind, Name, NameMinter, Node, Norm, Subst, Val};

pub use k1ndl1ng_norm::Sequential;

/// What one step of the machine did. This is the animation stream: the Vision
/// Pro renderer reads these and nothing else.
#[derive(Clone, Debug)]
pub enum Step {
    /// A component was taken off the queue and found to be inert.
    Inert { term: Hash32 },
    /// `new` minted unforgeable names.
    Minted { count: u32, first: Hash32 },
    /// A datum came to rest.
    Produced { chan: Hash32, datum: Hash32 },
    /// A continuation came to rest.
    Consumed { group: Vec<Hash32>, cont: Hash32 },
    /// A send and a receipt met. `spawned` is what the continuation yielded.
    Comm {
        chan: Hash32,
        cont: Hash32,
        spawned: usize,
    },
    /// A persistent record that reacted is being re-issued (Req. 2.7).
    Reissued { chan: Hash32 },
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct PlanId(pub u32);

/// The seam the bytecode analysis asked for. The unit of work is a prepared
/// continuation: `prepare` is offered a body once, `run` is handed the bindings
/// for one comm and returns the components to spawn.
pub trait Backend {
    fn prepare(&mut self, body: &Norm, binders: u16) -> PlanId;
    fn run(&mut self, plan: PlanId, env: &Subst) -> Vec<Norm>;
    fn plans(&self) -> usize;
}

/// The default backend: substitute into the normal form and take its top-level
/// components. The cache is bounded and evicts the least recently prepared,
/// because a gesture interface can mint continuations faster than it retires
/// them.
pub struct Direct {
    cap: usize,
    by_hash: BTreeMap<Hash32, PlanId>,
    bodies: BTreeMap<PlanId, (Norm, u16, u64)>,
    next: u32,
    clock: u64,
    hits: u64,
    misses: u64,
}

impl Default for Direct {
    fn default() -> Self {
        Direct::with_capacity(1024)
    }
}

impl Direct {
    pub fn with_capacity(cap: usize) -> Direct {
        Direct {
            cap: cap.max(1),
            by_hash: BTreeMap::new(),
            bodies: BTreeMap::new(),
            next: 0,
            clock: 0,
            hits: 0,
            misses: 0,
        }
    }
    /// Cache hits and misses, for the panel that shows the machine working.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
    fn evict(&mut self) {
        while self.bodies.len() > self.cap {
            let victim = self
                .bodies
                .iter()
                .min_by_key(|(_, (_, _, t))| *t)
                .map(|(id, (b, _, _))| (*id, b.hash()));
            match victim {
                None => break,
                Some((id, h)) => {
                    self.bodies.remove(&id);
                    if self.by_hash.get(&h) == Some(&id) {
                        self.by_hash.remove(&h);
                    }
                }
            }
        }
    }
}

impl Backend for Direct {
    fn prepare(&mut self, body: &Norm, binders: u16) -> PlanId {
        self.clock += 1;
        let h = body.hash();
        if let Some(id) = self.by_hash.get(&h).copied() {
            self.hits += 1;
            if let Some(e) = self.bodies.get_mut(&id) {
                e.2 = self.clock;
            }
            return id;
        }
        self.misses += 1;
        let id = PlanId(self.next);
        self.next = self.next.wrapping_add(1);
        self.by_hash.insert(h, id);
        self.bodies.insert(id, (body.clone(), binders, self.clock));
        self.evict();
        id
    }

    fn run(&mut self, plan: PlanId, env: &Subst) -> Vec<Norm> {
        match self.bodies.get(&plan) {
            None => Vec::new(),
            Some((body, _, _)) => body.substitute(env).top_level(),
        }
    }

    fn plans(&self) -> usize {
        self.bodies.len()
    }
}

// ---------------------------------------------------------------------------

/// A component waiting to run.
#[derive(Clone, Debug)]
pub struct Job {
    pub term: Norm,
    /// Where in the term tree this came from, for minting and for the renderer.
    pub path: Vec<u32>,
}

pub struct Machine<B: Backend = Direct, M: NameMinter = Sequential> {
    space: RSpace,
    backend: B,
    minter: M,
    queue: VecDeque<Job>,
    /// Every channel the machine has touched, so a renderer can ask the store
    /// what is sitting on it. The store keys by hash and never sees a `Name`.
    seen: BTreeMap<Hash32, Chan>,
    steps: u64,
    comms: u64,
}

impl Machine<Direct, Sequential> {
    pub fn new(cfg: Config) -> Machine<Direct, Sequential> {
        Machine::with(cfg, Direct::default(), Sequential::default())
    }
}

impl<B: Backend, M: NameMinter> Machine<B, M> {
    pub fn with(cfg: Config, backend: B, minter: M) -> Machine<B, M> {
        Machine {
            space: k1ndl1ng_campf1r3::rspace(cfg),
            backend,
            minter,
            queue: VecDeque::new(),
            seen: BTreeMap::new(),
            steps: 0,
            comms: 0,
        }
    }

    pub fn space(&self) -> &RSpace {
        &self.space
    }
    pub fn backend(&self) -> &B {
        &self.backend
    }
    pub fn queued(&self) -> usize {
        self.queue.len()
    }
    /// The components waiting to run, in order.
    pub fn jobs(&self) -> impl Iterator<Item = &Job> {
        self.queue.iter()
    }
    /// Every channel touched so far, in hash order.
    pub fn channels(&self) -> Vec<Chan> {
        self.seen.values().cloned().collect()
    }
    pub fn is_quiescent(&self) -> bool {
        self.queue.is_empty()
    }
    /// Steps taken and comms fired.
    pub fn counters(&self) -> (u64, u64) {
        (self.steps, self.comms)
    }

    /// Put a closed term to work. Its top-level components become jobs, in
    /// encoding order, which is what makes a run reproducible.
    pub fn spawn(&mut self, term: Norm) {
        for (i, c) in term.top_level().into_iter().enumerate() {
            self.queue.push_back(Job {
                term: c,
                path: vec![i as u32],
            });
        }
    }

    /// Run one component. `None` means the machine is quiescent.
    pub fn step(&mut self) -> Option<Result<Vec<Step>, SpaceError>> {
        let job = self.queue.pop_front()?;
        self.steps += 1;
        Some(self.run_job(job))
    }

    /// Run until quiescent or until `budget` steps have been taken. Returns the
    /// steps in order; the caller decides what to do with them.
    pub fn run(&mut self, budget: u64) -> Result<Vec<Step>, SpaceError> {
        let mut out = Vec::new();
        for _ in 0..budget {
            match self.step() {
                None => break,
                Some(r) => out.extend(r?),
            }
        }
        Ok(out)
    }

    fn push(&mut self, term: Norm, path: &[u32], i: usize) {
        let mut p = path.to_vec();
        p.push(i as u32);
        self.queue.push_back(Job { term, path: p });
    }

    fn run_job(&mut self, job: Job) -> Result<Vec<Step>, SpaceError> {
        let mut out = Vec::new();
        match job.term.node().clone() {
            Node::Nil => {}
            Node::Par(parts) => {
                // A par can reappear after substitution; flatten it back into
                // the queue rather than treating it as one unit.
                for (i, c) in parts.into_iter().enumerate() {
                    self.push(c, &job.path, i);
                }
            }
            Node::New { count, body } => {
                let mut slots: Vec<Val> = Vec::with_capacity(count as usize);
                let mut first = Hash32([0; 32]);
                for s in 0..count {
                    let path = BinderPath {
                        steps: job.path.clone(),
                        slot: s,
                    };
                    let k = self.minter.mint(&path);
                    let n = Name::Unforgeable(k);
                    if s == 0 {
                        first = n.content_hash();
                    }
                    slots.push(Val::Name(n));
                }
                out.push(Step::Minted { count, first });
                let opened = body.substitute(&Subst::from_slots(slots));
                for (i, c) in opened.top_level().into_iter().enumerate() {
                    self.push(c, &job.path, i);
                }
            }
            Node::Send {
                chan,
                persistent,
                args,
            } => {
                let c = Chan(chan);
                let chash = c.content_hash();
                self.seen.insert(sh(chash), c.clone());
                let d = Data::new(args);
                let dhash = d.content_hash();
                let mut pending = Some((c, d, persistent));
                // The re-issue loop: a persistent datum that reacts was never
                // stored, so it goes back in until it comes to rest.
                while let Some((c, d, p)) = pending.take() {
                    match self.space.produce(c.clone(), d.clone(), p)? {
                        None => out.push(Step::Produced {
                            chan: sh(chash),
                            datum: sh(dhash),
                        }),
                        Some(r) => {
                            let khash = r.cont.content_hash();
                            let n = self.apply(r)?;
                            out.push(Step::Comm {
                                chan: sh(chash),
                                cont: sh(khash),
                                spawned: n,
                            });
                            if p {
                                out.push(Step::Reissued { chan: sh(chash) });
                                pending = Some((c, d, p));
                            }
                        }
                    }
                }
            }
            Node::Receive { binds, body } => {
                let (chans, pats, persist, peeks) = receipt_parts(&binds);
                let binders: u16 = binds.iter().map(|b| b.binders).sum();
                let k = Cont::new(body, binders);
                let khash = k.content_hash();
                let group: Vec<Hash32> = chans.iter().map(|c| sh(c.content_hash())).collect();
                for c in &chans {
                    self.seen.insert(sh(c.content_hash()), c.clone());
                }
                // The re-issue contract is symmetric: an arriving *continuation*
                // that reacts was never stored either, so a persistent receipt
                // goes back in until it comes to rest.
                loop {
                    match self
                        .space
                        .consume(&chans, &pats, k.clone(), persist, peeks)?
                    {
                        None => {
                            out.push(Step::Consumed {
                                group,
                                cont: sh(khash),
                            });
                            break;
                        }
                        Some(r) => {
                            let chan = r
                                .channels
                                .first()
                                .map(|c| c.content_hash())
                                .unwrap_or_default();
                            let n = self.apply(r)?;
                            out.push(Step::Comm {
                                chan: sh(chan),
                                cont: sh(khash),
                                spawned: n,
                            });
                            if !persist {
                                break;
                            }
                            out.push(Step::Reissued { chan: sh(chan) });
                        }
                    }
                }
            }
            // `*x` for a name with no process behind it, and any variable left
            // at top level, has nowhere to go. A closed term should not contain
            // these; the machine records them rather than failing, so the
            // renderer can show a stuck rhobot.
            Node::Eval(_) | Node::BoundVar(_) | Node::FreeVar(_) | Node::Wild => {
                out.push(Step::Inert {
                    term: job.term.hash(),
                })
            }
        }
        Ok(out)
    }

    /// A comm fired: assemble the substitution, run the continuation through
    /// the backend, and queue what comes out.
    fn apply(
        &mut self,
        r: campf1r3_core::CommResult<Chan, BindPat, Data, Cont>,
    ) -> Result<usize, SpaceError> {
        self.comms += 1;
        let slots = k1ndl1ng_campf1r3::slots_of(&r.bound);
        let plan = self.backend.prepare(&r.cont.body, r.cont.binders);
        let spawned = self.backend.run(plan, &Subst::from_slots(slots));
        let n = spawned.len();
        for (i, c) in spawned.into_iter().enumerate() {
            self.queue.push_back(Job {
                term: c,
                path: vec![u32::MAX, i as u32],
            });
        }
        Ok(n)
    }
}

/// A receipt as the store wants it: channels, patterns, one persistence flag
/// and the peek set. Persistence is a property of the receipt, so a receipt
/// with any persistent bind is persistent.
fn receipt_parts(binds: &[NBind]) -> (Vec<Chan>, Vec<BindPat>, bool, PeekSet) {
    let chans: Vec<Chan> = binds.iter().map(|b| Chan(b.chan.clone())).collect();
    let pats: Vec<BindPat> = binds.iter().map(BindPat::of).collect();
    let persist = binds.iter().any(|b| b.kind == BindKind::Persistent);
    let peek_ix: Vec<usize> = binds
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind == BindKind::Peek)
        .map(|(i, _)| i)
        .collect();
    (chans, pats, persist, PeekSet::from_indices(&peek_ix))
}

/// The two crates each carry their own 32-byte newtype; the bytes are the same
/// BLAKE2b-256 digest of the same K1ndl1ng encoding.
fn sh(h: campf1r3_core::Hash32) -> Hash32 {
    Hash32(h.0)
}

#[cfg(test)]
mod tests;
