//! `campf1r3-core` — a small, multi-threaded, in-memory tuple store that serves
//! as an RSpace (CampF1R3 spec).
//!
//! The store is exactly the store monad together with the zip, generalised to
//! joins, patterns, persistence and peeks: `D : C -> Bag(datum)` and
//! `W, I : C+ -> Bag(waiting continuation)`, with `produce`, `consume` and
//! `install` as single cuts on a quiescent state (§2.4). It knows nothing about
//! processes: `C`, `P`, `A` and `K` are the caller's, the matcher is the
//! caller's, and the core never hashes a payload (Req. 4.1).
//!
//! Scope of this implementation, against the specification's open axes: bag
//! collections and flat name keys (the `KeyObject` and `Collection` seams are
//! named in `Config` but not yet parameterised), soft checkpoints, the event
//! log, and all three determinism profiles.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering as AtomOrd};
use std::sync::{RwLock, RwLockWriteGuard};

pub mod log;
pub use log::{Event, EventLog};

/// 32-byte content hash, computed once by the caller. The core never hashes
/// payloads (Req. 4.1).
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Default, Debug)]
pub struct Hash32(pub [u8; 32]);

impl Hash32 {
    pub fn short(&self) -> String {
        self.0[..4].iter().map(|b| format!("{b:02x}")).collect()
    }
}

pub trait Keyed {
    fn content_hash(&self) -> Hash32;
}

pub trait Channel: Keyed + Eq + Clone + Send + Sync + 'static {}
pub trait Datum: Keyed + Clone + Send + Sync + 'static {}
pub trait Pattern: Clone + Send + Sync + 'static {}
pub trait Cont: Keyed + Clone + Send + Sync + 'static {}

/// `m(p,a)` and the guard `gamma(k, bound)`. This is `rspace++`'s `Match` trait,
/// so the node's spatial matcher implements it unchanged.
pub trait Matcher<P, A, K>: Send + Sync + 'static {
    fn get(&self, p: &P, a: &A) -> Option<A>;
    fn check_commit(&self, _k: &K, _bound: &[A]) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abort;

/// Accounting hooks, called under the operation's locks, before any write.
/// The cut is where resources are debited, and it is the only place the store
/// hooks accounting (§10).
pub trait Observer<C, P, A, K>: Send + Sync + 'static {
    fn observe_produce(&self, _c: &C, _a: &A, _persist: bool) -> Result<(), Abort> {
        Ok(())
    }
    fn observe_consume(
        &self,
        _g: &[C],
        _ps: &[P],
        _k: &K,
        _persist: bool,
        _peeks: PeekSet,
    ) -> Result<(), Abort> {
        Ok(())
    }
    fn observe_comm(&self, _k: &K, _k_persist: bool, _data: &[(&A, bool)]) -> Result<(), Abort> {
        Ok(())
    }
}

/// An observer that does nothing, for callers with no budget to keep.
pub struct NoObserver;
impl<C, P, A, K> Observer<C, P, A, K> for NoObserver {}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct PeekSet(pub u64);

impl PeekSet {
    pub fn empty() -> PeekSet {
        PeekSet(0)
    }
    pub fn contains(&self, i: usize) -> bool {
        i < 64 && (self.0 >> i) & 1 == 1
    }
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
    pub fn from_indices(ix: &[usize]) -> PeekSet {
        let mut m = 0u64;
        for i in ix {
            if *i < 64 {
                m |= 1 << i;
            }
        }
        PeekSet(m)
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct OccId(pub u64);

/// Order key: the caller's content hash, then a stable arrival number. Shard
/// count does not affect it, which is why determinism survives resharding.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct OrderKey(pub Hash32, pub u64);

/// The key of a join group: its members' row keys, in the order the group was
/// issued. Comparison is lexicographic, so group iteration is deterministic and
/// no hash of a hash is needed.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct GroupKey(pub Vec<Hash32>);

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Profile {
    Free,
    Deterministic,
    Replay,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Strategy {
    Greedy,
    Complete,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Qualifier {
    Default,
    Temp,
    Seq,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub shards: u32,
    pub profile: Profile,
    pub strategy: Strategy,
    pub qualifier: Qualifier,
    pub logging: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            shards: if cfg!(target_family = "wasm") { 1 } else { 64 },
            profile: Profile::Deterministic,
            strategy: Strategy::Complete,
            qualifier: Qualifier::Default,
            logging: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceError {
    Aborted,
    ArityMismatch,
    GroupTooWide,
    EmptyGroup,
    InstallWouldReact,
    CellOccupied,
    WouldBlock,
    ReplayMismatch,
    QualifierViolation,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CommKind {
    Apply,
    Descend,
}

#[derive(Clone, Debug)]
pub struct Bound<A> {
    /// What the matcher returned: the binding result.
    pub binding: A,
    /// The datum that was removed (or peeked).
    pub datum: A,
    pub persist: bool,
}

#[derive(Clone, Debug)]
pub struct CommResult<C, P, A, K> {
    pub cont: K,
    pub cont_persist: bool,
    pub channels: Vec<C>,
    pub patterns: Vec<P>,
    pub bound: Vec<Bound<A>>,
    pub peeks: PeekSet,
    pub kind: CommKind,
}

// ---------------------------------------------------------------------------

#[derive(Clone)]
struct DatumOcc<A> {
    id: OccId,
    a: A,
    persist: bool,
    key: OrderKey,
}

#[derive(Clone)]
struct WaitOcc<P, K> {
    id: OccId,
    pats: Vec<P>,
    k: K,
    persist: bool,
    peeks: PeekSet,
    key: OrderKey,
}

#[derive(Clone)]
struct Row<C, A> {
    chan: C,
    data: Vec<DatumOcc<A>>,
    joins: BTreeSet<GroupKey>,
}

#[derive(Clone)]
struct GroupRow<C, P, K> {
    chans: Vec<C>,
    waiting: Vec<WaitOcc<P, K>>,
    installed: Vec<WaitOcc<P, K>>,
}

#[derive(Clone)]
struct Shard<C, P, A, K> {
    rows: BTreeMap<Hash32, Row<C, A>>,
    groups: BTreeMap<GroupKey, GroupRow<C, P, K>>,
}

impl<C, P, A, K> Default for Shard<C, P, A, K> {
    fn default() -> Self {
        Shard {
            rows: BTreeMap::new(),
            groups: BTreeMap::new(),
        }
    }
}

/// A snapshot for `revert` (§9.1). Soft checkpoints only: this store has no
/// history trie and no persistence.
pub struct Checkpoint<C, P, A, K> {
    shards: Vec<Shard<C, P, A, K>>,
    seq: u64,
}

pub struct Space<C, P, A, K, M, O = NoObserver> {
    cfg: Config,
    shards: Vec<RwLock<Shard<C, P, A, K>>>,
    mask: u64,
    bits: u32,
    seq: AtomicU64,
    matcher: M,
    observer: O,
    log: RwLock<EventLog>,
    expect: RwLock<Vec<(Hash32, Vec<Hash32>)>>,
}

const PHI64: u64 = 0x9E37_79B9_7F4A_7C15;

fn shard_of(r: &Hash32, bits: u32) -> usize {
    if bits == 0 {
        return 0;
    }
    let mut w = [0u8; 8];
    w.copy_from_slice(&r.0[..8]);
    let v = u64::from_le_bytes(w);
    (v.wrapping_mul(PHI64) >> (64 - bits)) as usize
}

impl<C, P, A, K, M, O> Space<C, P, A, K, M, O>
where
    C: Channel,
    P: Pattern,
    A: Datum,
    K: Cont,
    M: Matcher<P, A, K>,
    O: Observer<C, P, A, K>,
{
    pub fn new(cfg: Config, matcher: M, observer: O) -> Self {
        let n = cfg.shards.max(1).next_power_of_two() as usize;
        let bits = n.trailing_zeros();
        let mut shards = Vec::with_capacity(n);
        for _ in 0..n {
            shards.push(RwLock::new(Shard::default()));
        }
        Space {
            cfg,
            shards,
            mask: (n as u64) - 1,
            bits,
            seq: AtomicU64::new(0),
            matcher,
            observer,
            log: RwLock::new(EventLog::default()),
            expect: RwLock::new(Vec::new()),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }
    pub fn matcher(&self) -> &M {
        &self.matcher
    }
    pub fn observer(&self) -> &O {
        &self.observer
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, AtomOrd::SeqCst)
    }

    fn shard_ix(&self, r: &Hash32) -> usize {
        shard_of(r, self.bits) & self.mask as usize
    }

    /// Lock a set of shards, always in ascending index order, so the lock order
    /// is a total order and no cycle can form (§8.2).
    fn lock_many(&self, mut ix: Vec<usize>) -> Vec<(usize, RwLockWriteGuard<'_, Shard<C, P, A, K>>)> {
        ix.sort_unstable();
        ix.dedup();
        let mut guards = Vec::with_capacity(ix.len());
        for i in ix {
            match self.shards[i].write() {
                Ok(g) => guards.push((i, g)),
                Err(p) => guards.push((i, p.into_inner())),
            }
        }
        guards
    }

    fn home_of(&self, g: &GroupKey) -> usize {
        let min = g.0.iter().min().copied().unwrap_or_default();
        self.shard_ix(&min)
    }

    // -- produce ------------------------------------------------------------

    /// `produce(c, a, persist)`: store the datum, or react with one waiting
    /// continuation and return the comm result. The arriving record is never
    /// stored when it reacts, even if persistent; a caller that wants it to
    /// persist re-issues until `None` (Req. 2.7).
    pub fn produce(
        &self,
        c: C,
        a: A,
        persist: bool,
    ) -> Result<Option<CommResult<C, P, A, K>>, SpaceError> {
        let r = c.content_hash();
        let home = self.shard_ix(&r);
        loop {
            // Footprint: c's shard, plus the home shard of every group c is in.
            let joins: Vec<GroupKey> = {
                let g = self.shards[home].read().map(|g| g).unwrap_or_else(|p| p.into_inner());
                g.rows.get(&r).map(|row| row.joins.iter().cloned().collect()).unwrap_or_default()
            };
            let mut ix = vec![home];
            for gk in &joins {
                ix.push(self.home_of(gk));
                for h in &gk.0 {
                    ix.push(self.shard_ix(h));
                }
            }
            let mut guards = self.lock_many(ix);
            // Re-read the join set under the locks; retry if it moved.
            let joins_now: Vec<GroupKey> = guards
                .iter()
                .find(|(i, _)| *i == home)
                .and_then(|(_, g)| g.rows.get(&r))
                .map(|row| row.joins.iter().cloned().collect())
                .unwrap_or_default();
            if joins_now != joins {
                drop(guards);
                continue;
            }

            self.observer
                .observe_produce(&c, &a, persist)
                .map_err(|_| SpaceError::Aborted)?;

            let arriving = DatumOcc {
                id: OccId(u64::MAX),
                a: a.clone(),
                persist,
                key: OrderKey(a.content_hash(), u64::MAX),
            };

            // Candidate search: groups in GroupKey order, installed before
            // waiting, continuations and data by order key (§7.1 step 4).
            let mut groups: Vec<GroupKey> = joins_now;
            if !groups.iter().any(|g| g.0.len() == 1 && g.0[0] == r) {
                groups.push(GroupKey(vec![r]));
            }
            groups.sort();
            let mut found: Option<Fired<C, P, A, K>> = None;
            'outer: for gk in &groups {
                let ghome = self.home_of(gk);
                let positions: Vec<usize> = gk
                    .0
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| **h == r)
                    .map(|(i, _)| i)
                    .collect();
                if positions.is_empty() {
                    continue;
                }
                let conts: Vec<(bool, usize)> = {
                    let g = guards.iter().find(|(i, _)| *i == ghome);
                    match g.and_then(|(_, s)| s.groups.get(gk)) {
                        None => continue,
                        Some(row) => {
                            let mut v: Vec<(bool, usize)> = Vec::new();
                            for i in 0..row.installed.len() {
                                v.push((true, i));
                            }
                            for i in 0..row.waiting.len() {
                                v.push((false, i));
                            }
                            v
                        }
                    }
                };
                for (installed, wi) in conts {
                    for pos in &positions {
                        let attempt = self.try_candidate(
                            &guards, gk, ghome, installed, wi, Some((*pos, &arriving)),
                        );
                        if let Some(f) = attempt {
                            found = Some(f);
                            break 'outer;
                        }
                    }
                }
            }

            return match found {
                Some(f) => {
                    let res = self.commit(&mut guards, f)?;
                    Ok(Some(res))
                }
                None => {
                    let seq = self.next_seq();
                    let key = OrderKey(a.content_hash(), seq);
                    let g = guards.iter_mut().find(|(i, _)| *i == home).expect("home locked");
                    let row = g.1.rows.entry(r).or_insert_with(|| Row {
                        chan: c.clone(),
                        data: Vec::new(),
                        joins: BTreeSet::new(),
                    });
                    let occ = DatumOcc {
                        id: OccId(seq),
                        a: a.clone(),
                        persist,
                        key,
                    };
                    let at = row.data.partition_point(|d| d.key < occ.key);
                    row.data.insert(at, occ);
                    if self.cfg.logging {
                        self.log
                            .write()
                            .map(|mut l| l.produce(r, a.content_hash(), persist))
                            .ok();
                    }
                    Ok(None)
                }
            };
        }
    }

    // -- consume ------------------------------------------------------------

    pub fn consume(
        &self,
        g: &[C],
        ps: &[P],
        k: K,
        persist: bool,
        peeks: PeekSet,
    ) -> Result<Option<CommResult<C, P, A, K>>, SpaceError> {
        self.consume_inner(g, ps, k, persist, peeks, false)
    }

    /// `install(g, ps, k)`: as `consume` with `persist = true` and no peeks,
    /// except that a candidate is an error. Installed continuations survive
    /// `clear` and checkpoint reversion.
    pub fn install(&self, g: &[C], ps: &[P], k: K) -> Result<(), SpaceError> {
        self.consume_inner(g, ps, k, true, PeekSet::empty(), true)
            .map(|_| ())
    }

    fn consume_inner(
        &self,
        g: &[C],
        ps: &[P],
        k: K,
        persist: bool,
        peeks: PeekSet,
        installing: bool,
    ) -> Result<Option<CommResult<C, P, A, K>>, SpaceError> {
        if g.is_empty() {
            return Err(SpaceError::EmptyGroup);
        }
        if g.len() != ps.len() {
            return Err(SpaceError::ArityMismatch);
        }
        if g.len() > 64 {
            return Err(SpaceError::GroupTooWide);
        }
        if peeks.0 != 0 && (peeks.0 >> g.len()) != 0 {
            return Err(SpaceError::ArityMismatch);
        }
        let gk = GroupKey(g.iter().map(|c| c.content_hash()).collect());
        let ghome = self.home_of(&gk);
        let mut ix: Vec<usize> = gk.0.iter().map(|h| self.shard_ix(h)).collect();
        ix.push(ghome);
        let mut guards = self.lock_many(ix);

        self.observer
            .observe_consume(g, ps, &k, persist, peeks)
            .map_err(|_| SpaceError::Aborted)?;

        // Install the candidate continuation transiently so the search machinery
        // can treat it uniformly, then remove it whatever the outcome.
        let seq = self.next_seq();
        let w = WaitOcc {
            id: OccId(seq),
            pats: ps.to_vec(),
            k: k.clone(),
            persist,
            peeks,
            key: OrderKey(k.content_hash(), seq),
        };
        {
            let s = guards.iter_mut().find(|(i, _)| *i == ghome).expect("home locked");
            let row = s.1.groups.entry(gk.clone()).or_insert_with(|| GroupRow {
                chans: g.to_vec(),
                waiting: Vec::new(),
                installed: Vec::new(),
            });
            row.waiting.push(w.clone());
        }
        let wi = {
            let s = guards.iter().find(|(i, _)| *i == ghome).expect("home locked");
            s.1.groups.get(&gk).map(|r| r.waiting.len() - 1).unwrap_or(0)
        };
        let attempt = self.try_candidate(&guards, &gk, ghome, false, wi, None);

        // Take the transient continuation out again.
        {
            let s = guards.iter_mut().find(|(i, _)| *i == ghome).expect("home locked");
            if let Some(row) = s.1.groups.get_mut(&gk) {
                row.waiting.retain(|x| x.id != w.id);
            }
        }

        match attempt {
            Some(f) => {
                if installing {
                    return Err(SpaceError::InstallWouldReact);
                }
                let res = self.commit(&mut guards, f)?;
                Ok(Some(res))
            }
            None => {
                {
                    let s = guards.iter_mut().find(|(i, _)| *i == ghome).expect("home locked");
                    let row = s.1.groups.entry(gk.clone()).or_insert_with(|| GroupRow {
                        chans: g.to_vec(),
                        waiting: Vec::new(),
                        installed: Vec::new(),
                    });
                    let bag = if installing {
                        &mut row.installed
                    } else {
                        &mut row.waiting
                    };
                    let at = bag.partition_point(|x| x.key < w.key);
                    bag.insert(at, w.clone());
                }
                // Register the group in each member row, creating rows as needed.
                for (pos, h) in gk.0.iter().enumerate() {
                    let si = self.shard_ix(h);
                    let s = guards.iter_mut().find(|(i, _)| *i == si).expect("member locked");
                    let row = s.1.rows.entry(*h).or_insert_with(|| Row {
                        chan: g[pos].clone(),
                        data: Vec::new(),
                        joins: BTreeSet::new(),
                    });
                    row.joins.insert(gk.clone());
                }
                if self.cfg.logging {
                    self.log
                        .write()
                        .map(|mut l| l.consume(gk.clone(), k.content_hash(), persist, installing))
                        .ok();
                }
                Ok(None)
            }
        }
    }

    // -- candidate search ---------------------------------------------------

    fn try_candidate(
        &self,
        guards: &[(usize, RwLockWriteGuard<'_, Shard<C, P, A, K>>)],
        gk: &GroupKey,
        ghome: usize,
        installed: bool,
        wi: usize,
        arriving: Option<(usize, &DatumOcc<A>)>,
    ) -> Option<Fired<C, P, A, K>> {
        let ghs = guards.iter().find(|(i, _)| *i == ghome)?;
        let grow = ghs.1.groups.get(gk)?;
        let w = if installed {
            grow.installed.get(wi)?
        } else {
            grow.waiting.get(wi)?
        };
        let n = gk.0.len();
        if w.pats.len() != n {
            return None;
        }

        // Eligible occurrences per position, in order-key order, with the
        // arriving datum presented first at its own positions.
        let mut pool: Vec<Vec<DatumOcc<A>>> = Vec::with_capacity(n);
        for (pos, h) in gk.0.iter().enumerate() {
            let mut v: Vec<DatumOcc<A>> = Vec::new();
            if let Some((apos, d)) = arriving {
                if apos == pos {
                    v.push((*d).clone());
                }
            }
            let si = self.shard_ix(h);
            if let Some((_, s)) = guards.iter().find(|(i, _)| *i == si) {
                if let Some(row) = s.rows.get(h) {
                    v.extend(row.data.iter().cloned());
                }
            }
            pool.push(v);
        }
        // Every candidate must use the arriving datum (§7.1 step 4).
        let must_use = arriving.map(|(pos, _)| pos);

        // Backtracking search over positions, iterative.
        let mut choice: Vec<usize> = vec![0; n];
        let mut bound: Vec<A> = Vec::with_capacity(n);
        let mut chosen: Vec<DatumOcc<A>> = Vec::with_capacity(n);
        let mut pos = 0usize;
        loop {
            if pos == n {
                if self.matcher.check_commit(&w.k, &bound)
                    && must_use.map_or(true, |p| chosen[p].id == OccId(u64::MAX))
                    && self.replay_admits(&w.k, &chosen)
                {
                    return Some(Fired {
                        gk: gk.clone(),
                        ghome,
                        installed,
                        wid: w.id,
                        cont: w.k.clone(),
                        cont_persist: w.persist,
                        peeks: w.peeks,
                        pats: w.pats.clone(),
                        chans: grow.chans.clone(),
                        bound: bound.clone(),
                        chosen: chosen.clone(),
                    });
                }
                // Backtrack.
                pos -= 1;
                bound.pop();
                chosen.pop();
                choice[pos] += 1;
            }
            let mut advanced = false;
            while choice[pos] < pool[pos].len() {
                let cand = &pool[pos][choice[pos]];
                // Occurrences must be pairwise distinct (Def. 2.3 (1)).
                let clash = chosen.iter().any(|d| d.id == cand.id);
                if !clash {
                    if let Some(b) = self.matcher.get(&w.pats[pos], &cand.a) {
                        bound.push(b);
                        chosen.push(cand.clone());
                        pos += 1;
                        advanced = true;
                        break;
                    }
                }
                choice[pos] += 1;
                if self.cfg.strategy == Strategy::Greedy && !chosen.is_empty() {
                    // Greedy still scans within a position; it does not revisit
                    // an earlier position's choice.
                }
            }
            if advanced {
                if pos < n {
                    choice[pos] = 0;
                }
                continue;
            }
            // Exhausted this position.
            if pos == 0 {
                return None;
            }
            if self.cfg.strategy == Strategy::Greedy {
                return None;
            }
            pos -= 1;
            bound.pop();
            chosen.pop();
            choice[pos] += 1;
        }
    }

    fn replay_admits(&self, k: &K, chosen: &[DatumOcc<A>]) -> bool {
        if self.cfg.profile != Profile::Replay {
            return true;
        }
        let kh = k.content_hash();
        let ds: Vec<Hash32> = chosen.iter().map(|d| d.a.content_hash()).collect();
        self.expect
            .read()
            .map(|e| e.iter().any(|(a, b)| *a == kh && *b == ds))
            .unwrap_or(false)
    }

    fn commit(
        &self,
        guards: &mut Vec<(usize, RwLockWriteGuard<'_, Shard<C, P, A, K>>)>,
        f: Fired<C, P, A, K>,
    ) -> Result<CommResult<C, P, A, K>, SpaceError> {
        let data_for_observer: Vec<(&A, bool)> =
            f.chosen.iter().map(|d| (&d.a, d.persist)).collect();
        self.observer
            .observe_comm(&f.cont, f.cont_persist, &data_for_observer)
            .map_err(|_| SpaceError::Aborted)?;

        // Remove the continuation if linear and not installed.
        if !f.installed && !f.cont_persist {
            let s = guards.iter_mut().find(|(i, _)| *i == f.ghome).expect("group locked");
            if let Some(row) = s.1.groups.get_mut(&f.gk) {
                row.waiting.retain(|x| x.id != f.wid);
            }
        }
        // Remove each chosen datum that is linear and not at a peek position.
        for (pos, d) in f.chosen.iter().enumerate() {
            if d.id == OccId(u64::MAX) {
                continue; // the arriving datum was never stored
            }
            if d.persist || f.peeks.contains(pos) {
                continue;
            }
            let h = f.gk.0[pos];
            let si = self.shard_ix(&h);
            let s = guards.iter_mut().find(|(i, _)| *i == si).expect("member locked");
            if let Some(row) = s.1.rows.get_mut(&h) {
                row.data.retain(|x| x.id != d.id);
            }
        }
        if self.cfg.profile == Profile::Replay {
            let kh = f.cont.content_hash();
            let ds: Vec<Hash32> = f.chosen.iter().map(|d| d.a.content_hash()).collect();
            if let Ok(mut e) = self.expect.write() {
                if let Some(ix) = e.iter().position(|(a, b)| *a == kh && *b == ds) {
                    e.remove(ix);
                } else {
                    return Err(SpaceError::ReplayMismatch);
                }
            }
        }
        if self.cfg.logging {
            let ds: Vec<Hash32> = f.chosen.iter().map(|d| d.a.content_hash()).collect();
            self.log
                .write()
                .map(|mut l| l.comm(f.gk.clone(), f.cont.content_hash(), ds))
                .ok();
        }
        Ok(CommResult {
            cont: f.cont,
            cont_persist: f.cont_persist,
            channels: f.chans,
            patterns: f.pats,
            bound: f
                .bound
                .into_iter()
                .zip(f.chosen.into_iter())
                .map(|(b, d)| Bound {
                    binding: b,
                    datum: d.a,
                    persist: d.persist,
                })
                .collect(),
            peeks: f.peeks,
            kind: CommKind::Apply,
        })
    }

    // -- reads and administration -------------------------------------------

    pub fn get_data(&self, c: &C) -> Vec<A> {
        let r = c.content_hash();
        let s = self.shards[self.shard_ix(&r)]
            .read()
            .unwrap_or_else(|p| p.into_inner());
        s.rows
            .get(&r)
            .map(|row| row.data.iter().map(|d| d.a.clone()).collect())
            .unwrap_or_default()
    }

    pub fn get_waiting(&self, g: &[C]) -> Vec<(Vec<P>, K, bool)> {
        let gk = GroupKey(g.iter().map(|c| c.content_hash()).collect());
        let s = self.shards[self.home_of(&gk)]
            .read()
            .unwrap_or_else(|p| p.into_inner());
        s.groups
            .get(&gk)
            .map(|row| {
                row.installed
                    .iter()
                    .chain(row.waiting.iter())
                    .map(|w| (w.pats.clone(), w.k.clone(), w.persist))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The channel a row key stands for, if this store has ever seen it. The
    /// store keys everything by hash, so this is the only way back from a hash
    /// in the event log to the caller's own channel.
    pub fn channel_of(&self, r: &Hash32) -> Option<C> {
        let s = self.shards[self.shard_ix(r)]
            .read()
            .unwrap_or_else(|p| p.into_inner());
        s.rows.get(r).map(|row| row.chan.clone())
    }

    pub fn get_joins(&self, c: &C) -> Vec<Vec<C>> {
        let r = c.content_hash();
        let joins: Vec<GroupKey> = {
            let s = self.shards[self.shard_ix(&r)]
                .read()
                .unwrap_or_else(|p| p.into_inner());
            s.rows
                .get(&r)
                .map(|row| row.joins.iter().cloned().collect())
                .unwrap_or_default()
        };
        joins
            .iter()
            .filter_map(|gk| {
                let s = self.shards[self.home_of(gk)]
                    .read()
                    .unwrap_or_else(|p| p.into_inner());
                s.groups.get(gk).map(|r| r.chans.clone())
            })
            .collect()
    }

    /// Data and continuation counts, for a UI or a test.
    pub fn stats(&self) -> (usize, usize) {
        let mut data = 0;
        let mut conts = 0;
        for s in &self.shards {
            let s = s.read().unwrap_or_else(|p| p.into_inner());
            for row in s.rows.values() {
                data += row.data.len();
            }
            for g in s.groups.values() {
                conts += g.waiting.len() + g.installed.len();
            }
        }
        (data, conts)
    }

    pub fn is_quiescent_by_counts(&self) -> bool {
        let (d, c) = self.stats();
        d == 0 || c == 0
    }

    /// All rows and groups, sorted by key: deterministic, for diffing two runs.
    pub fn export(&self) -> Vec<(Hash32, Vec<Hash32>)> {
        let mut out: Vec<(Hash32, Vec<Hash32>)> = Vec::new();
        for s in &self.shards {
            let s = s.read().unwrap_or_else(|p| p.into_inner());
            for (h, row) in &s.rows {
                let mut ds: Vec<Hash32> = row.data.iter().map(|d| d.key.0).collect();
                ds.sort();
                if !ds.is_empty() {
                    out.push((*h, ds));
                }
            }
            for (gk, row) in &s.groups {
                let mut ks: Vec<Hash32> = row
                    .installed
                    .iter()
                    .chain(row.waiting.iter())
                    .map(|w| w.key.0)
                    .collect();
                ks.sort();
                if !ks.is_empty() {
                    let mut key = gk.0.clone();
                    key.dedup();
                    out.push((key[0], ks));
                }
            }
        }
        out.sort();
        out
    }

    pub fn remove_all_data(&self, c: &C) {
        let r = c.content_hash();
        let mut s = self.shards[self.shard_ix(&r)]
            .write()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(row) = s.rows.get_mut(&r) {
            row.data.clear();
        }
    }

    pub fn clear(&self) {
        let mut installed: Vec<(GroupKey, GroupRow<C, P, K>)> = Vec::new();
        for s in &self.shards {
            let mut s = s.write().unwrap_or_else(|p| p.into_inner());
            for (gk, row) in s.groups.iter() {
                if !row.installed.is_empty() {
                    installed.push((
                        gk.clone(),
                        GroupRow {
                            chans: row.chans.clone(),
                            waiting: Vec::new(),
                            installed: row.installed.clone(),
                        },
                    ));
                }
            }
            s.rows.clear();
            s.groups.clear();
        }
        for (gk, row) in installed {
            let home = self.home_of(&gk);
            // Read what we need before taking any lock: a member shard may be
            // the home shard, and `RwLock` is not reentrant.
            let chans = row.chans.clone();
            {
                let mut s = self.shards[home].write().unwrap_or_else(|p| p.into_inner());
                s.groups.insert(gk.clone(), row);
            }
            for (pos, h) in gk.0.iter().enumerate() {
                let si = self.shard_ix(h);
                let mut s = self.shards[si].write().unwrap_or_else(|p| p.into_inner());
                if let Some(c) = chans.get(pos) {
                    let row = s.rows.entry(*h).or_insert_with(|| Row {
                        chan: c.clone(),
                        data: Vec::new(),
                        joins: BTreeSet::new(),
                    });
                    row.joins.insert(gk.clone());
                }
            }
        }
    }

    pub fn soft_checkpoint(&self) -> Checkpoint<C, P, A, K> {
        let shards = self
            .shards
            .iter()
            .map(|s| s.read().unwrap_or_else(|p| p.into_inner()).clone())
            .collect();
        Checkpoint {
            shards,
            seq: self.seq.load(AtomOrd::SeqCst),
        }
    }

    pub fn revert(&self, cp: &Checkpoint<C, P, A, K>) {
        for (i, s) in self.shards.iter().enumerate() {
            let mut g = s.write().unwrap_or_else(|p| p.into_inner());
            *g = cp.shards[i].clone();
        }
        self.seq.store(cp.seq, AtomOrd::SeqCst);
    }

    pub fn take_log(&self) -> Vec<Event> {
        self.log
            .write()
            .map(|mut l| l.take())
            .unwrap_or_default()
    }

    pub fn peek_log(&self) -> Vec<Event> {
        self.log
            .read()
            .map(|l| l.events.clone())
            .unwrap_or_default()
    }

    /// Load the expected comm multiset for the `Replay` profile.
    pub fn rig(&self, expected: Vec<(Hash32, Vec<Hash32>)>) {
        if let Ok(mut e) = self.expect.write() {
            *e = expected;
        }
    }

    pub fn check_replay(&self) -> Result<(), SpaceError> {
        let left = self.expect.read().map(|e| e.len()).unwrap_or(0);
        if left == 0 {
            Ok(())
        } else {
            Err(SpaceError::ReplayMismatch)
        }
    }
}

struct Fired<C, P, A, K> {
    gk: GroupKey,
    ghome: usize,
    installed: bool,
    wid: OccId,
    cont: K,
    cont_persist: bool,
    peeks: PeekSet,
    pats: Vec<P>,
    chans: Vec<C>,
    bound: Vec<A>,
    chosen: Vec<DatumOcc<A>>,
}

#[cfg(test)]
mod tests;
