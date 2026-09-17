//! The event log.
//!
//! The specification asks that the space be able to say what it did, in the
//! order it did it, without that record being load bearing for the semantics.
//! Every entry here is appended at a cut: a produce that came to rest, a
//! consume that came to rest, or a comm.  Nothing in this module is consulted
//! when deciding whether a match is admissible; the replay profile keeps its
//! own expectation list.

use crate::Hash32;
use crate::GroupKey;

/// One entry in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A datum came to rest on a channel.
    Produce {
        chan: Hash32,
        datum: Hash32,
        persist: bool,
    },
    /// A continuation came to rest over a group.
    Consume {
        group: GroupKey,
        cont: Hash32,
        persist: bool,
        installing: bool,
    },
    /// A continuation and a tuple of data met.
    Comm {
        group: GroupKey,
        cont: Hash32,
        data: Vec<Hash32>,
    },
}

/// An append-only list of events, drained by `Space::take_log`.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    pub events: Vec<Event>,
}

impl EventLog {
    pub fn produce(&mut self, chan: Hash32, datum: Hash32, persist: bool) {
        self.events.push(Event::Produce {
            chan,
            datum,
            persist,
        });
    }

    pub fn consume(&mut self, group: GroupKey, cont: Hash32, persist: bool, installing: bool) {
        self.events.push(Event::Consume {
            group,
            cont,
            persist,
            installing,
        });
    }

    pub fn comm(&mut self, group: GroupKey, cont: Hash32, data: Vec<Hash32>) {
        self.events.push(Event::Comm { group, cont, data });
    }

    /// Drain the log.
    pub fn take(&mut self) -> Vec<Event> {
        core::mem::take(&mut self.events)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}
