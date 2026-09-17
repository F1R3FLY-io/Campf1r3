//! `k1ndl1ng-campf1r3` — the glue that makes an RSpace out of normal forms.
//!
//! The instantiation the specs call for: channels are names in normal form,
//! data are a send's argument lists, patterns are a bind's pattern list, and a
//! continuation is a receipt body together with the size of the binder group
//! it expects. The store hashes nothing: every content hash here is BLAKE2b-256
//! of a K1ndl1ng encoding, which is what makes name equality a 32-byte
//! comparison.

#![forbid(unsafe_code)]

pub mod spatial;

use campf1r3_core::{
    Channel, Cont as ContTrait, Datum, Hash32 as SHash, Keyed, Matcher, Pattern,
};
use k1ndl1ng_norm::term::{leb, T_PAR};
use k1ndl1ng_norm::hash::blake2b_256;
use k1ndl1ng_norm::{BindKind, NBind, Name, Norm, Val};

pub use campf1r3_core::{
    Abort, Bound, CommKind, CommResult, Config, Event, NoObserver, Observer, PeekSet, Profile,
    Space, SpaceError, Strategy,
};

fn conv(h: k1ndl1ng_norm::Hash32) -> SHash {
    SHash(h.0)
}

/// A channel: a name in normal form.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chan(pub Name);

impl Keyed for Chan {
    fn content_hash(&self) -> SHash {
        conv(self.0.content_hash())
    }
}
impl Channel for Chan {}

impl Chan {
    pub fn quote(p: Norm) -> Chan {
        Chan(Name::Quote(p))
    }
    pub fn unforgeable(b: [u8; 32]) -> Chan {
        Chan(Name::Unforgeable(b))
    }
    pub fn name(&self) -> &Name {
        &self.0
    }
}

/// A datum: the argument list of a send, plus — once it has been through the
/// matcher — the bindings that matching it produced.
///
/// The hash covers the arguments only. Bindings are a result, not content:
/// two data with the same arguments are the same datum, whatever a pattern
/// later made of them, and the order key must not depend on who matched it.
#[derive(Clone, Debug)]
pub struct Data {
    pub args: Vec<Norm>,
    pub bound: Vec<Val>,
}

impl Data {
    pub fn new(args: Vec<Norm>) -> Data {
        Data {
            args,
            bound: Vec::new(),
        }
    }
    pub fn arity(&self) -> usize {
        self.args.len()
    }
}

impl Keyed for Data {
    fn content_hash(&self) -> SHash {
        // The arguments as a tuple, encoded the way a par of them would be, so
        // that arity is part of the identity.
        let mut enc = vec![T_PAR];
        leb(self.args.len() as u32, &mut enc);
        for a in &self.args {
            enc.extend_from_slice(a.encode());
        }
        conv(blake2b_256(&enc))
    }
}
impl Datum for Data {}

/// A pattern: one bind's pattern list, with the number of slots it binds.
#[derive(Clone, Debug)]
pub struct BindPat {
    pub pats: Vec<Name>,
    pub binders: u16,
    pub kind: BindKind,
}

impl BindPat {
    pub fn of(b: &NBind) -> BindPat {
        BindPat {
            pats: b.pats.clone(),
            binders: b.binders,
            kind: b.kind,
        }
    }
}
impl Pattern for BindPat {}

/// A continuation: a receipt body and the size of the binder group it expects.
/// The body's de Bruijn indices `0 .. binders-1` are the group; the caller
/// substitutes with `Subst::from_slots`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cont {
    pub body: Norm,
    pub binders: u16,
}

impl Cont {
    pub fn new(body: Norm, binders: u16) -> Cont {
        Cont { body, binders }
    }
}

impl Keyed for Cont {
    fn content_hash(&self) -> SHash {
        conv(self.body.hash())
    }
}
impl ContTrait for Cont {}

/// The spatial matcher, as `campf1r3` wants it.
pub struct Kernel;

impl Matcher<BindPat, Data, Cont> for Kernel {
    fn get(&self, p: &BindPat, a: &Data) -> Option<Data> {
        let bound = spatial::match_pats(&p.pats, &a.args, p.binders)?;
        Some(Data {
            args: a.args.clone(),
            bound,
        })
    }
}

/// The store, at the types this crate fixes.
pub type RSpace<O = NoObserver> = Space<Chan, BindPat, Data, Cont, Kernel, O>;

pub fn rspace(cfg: Config) -> RSpace {
    Space::new(cfg, Kernel, NoObserver)
}

/// Collect a comm result's bindings into one substitution group, in slot
/// order: binds in the order the receipt stores them, each contributing its
/// own slots in order (spec §7).
pub fn slots_of(bound: &[campf1r3_core::Bound<Data>]) -> Vec<Val> {
    let mut v = Vec::new();
    for b in bound {
        v.extend(b.binding.bound.iter().cloned());
    }
    v
}

#[cfg(test)]
mod tests;
