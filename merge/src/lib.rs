//! CRDT merge rules over a small concrete value model.
//!
//! A merge rule is data ([`Rule`]), so a host, a script or a TypeScript client can read it at runtime.
//! Each rule has one state shape inside [`Value`], and [`merge`] joins two states of that shape. Every
//! `merge` is a join-semilattice (commutative, associative, idempotent) on well-formed states; the Lean
//! proofs in `lean/` are checked against the Aeneas translation of this file.
//!
//! The code stays inside the subset Aeneas translates: no unsafe, no traits, no generics, no iterators,
//! no `?`. Collections are sorted cons lists, so each merge is a plain recursive sorted union. Their
//! depth is the recursion depth, which is fine for component-sized states (hundreds of entries).
//!
//! Well-formed means every list is sorted by key, strictly ascending (no duplicate keys). The functions
//! in this file only build well-formed lists from well-formed inputs; [`validate`] checks untrusted ones.

#![no_std]
#![forbid(unsafe_code)]
// Plain `match` and `if` (no `matches!`, no guards) keep the Aeneas translation simple to reason about.
#![allow(clippy::collapsible_match, clippy::match_like_matches_macro)]

extern crate alloc;

use alloc::boxed::Box;

/// Hybrid logical clock stamp. Ordered by `wall`, then `counter`, then `actor` (the tie-break), so two
/// distinct stamps are never equal in the order.
#[derive(Clone, Copy)]
pub struct Hlc {
    pub wall: u64,
    pub counter: u32,
    pub actor: u64,
}

/// What can go wrong: a value of the wrong shape for its rule (or a field count that differs), or a
/// counter or clock that would pass its integer range.
#[derive(Clone, Copy)]
pub enum Error {
    Mismatch,
    Overflow,
}

/// The merge rule a component (or one member of it) declares.
#[derive(Clone, Copy)]
pub enum Rule {
    LastWriter,
    AddWins,
    Counter,
    KeepBoth,
}

/// A set of stamps, sorted ascending.
pub enum Stamps {
    Nil,
    Cons(Hlc, Box<Stamps>),
}

/// A map from stamp to value, sorted ascending by stamp. Two entries under one stamp join by `max`.
pub enum Tagged {
    Nil,
    Cons(Hlc, u64, Box<Tagged>),
}

/// A map from actor to count, sorted ascending by actor. Two counts for one actor join by `max`.
pub enum Counts {
    Nil,
    Cons(u64, u64, Box<Counts>),
}

/// LastWriter state: the value with the greatest stamp wins (the value breaks a tie of equal stamps).
#[derive(Clone, Copy)]
pub struct Register {
    pub stamp: Hlc,
    pub value: u64,
}

/// AddWins (OR-set) and KeepBoth (multi-value register) state: every write tagged with a unique stamp,
/// and the stamps that were removed or overwritten. Visible: entries whose stamp is not in `removed`.
pub struct Tombstoned {
    pub entries: Tagged,
    pub removed: Stamps,
}

/// Counter (PN-counter) state: per-actor increments and decrements. Value: sum(inc) - sum(dec).
pub struct Pn {
    pub inc: Counts,
    pub dec: Counts,
}

/// One rule's state. The variant must match the rule it is merged under.
pub enum Value {
    Register(Register),
    Set(Tombstoned),
    Counter(Pn),
    Multi(Tombstoned),
}

/// An operation, already prepared at its source (an effector): it carries the stamps it observed.
pub enum Op {
    /// LastWriter: write `value` at `stamp`.
    Write(Hlc, u64),
    /// AddWins: add `element` under the fresh tag `tag`.
    Add(Hlc, u64),
    /// AddWins: remove the tags the source saw for an element.
    Remove(Stamps),
    /// Counter: add `n` to `actor`'s increments.
    Inc(u64, u64),
    /// Counter: add `n` to `actor`'s decrements.
    Dec(u64, u64),
    /// KeepBoth: write `value` under the fresh tag `tag`, overwriting the tags the source saw.
    Put(Hlc, u64, Stamps),
}

/// A struct component's members, in declaration order.
pub enum Fields {
    Nil,
    Cons(Value, Box<Fields>),
}

/// The rule of each member of a struct component, in declaration order.
pub enum Rules {
    Nil,
    Cons(Rule, Box<Rules>),
}

// ---------------------------------------------------------------------------------------------------
// Hybrid logical clock

/// The stamp order: a strict total order, `actor` breaking ties.
pub fn hlc_lt(a: &Hlc, b: &Hlc) -> bool {
    if a.wall < b.wall {
        true
    } else if b.wall < a.wall {
        false
    } else if a.counter < b.counter {
        true
    } else if b.counter < a.counter {
        false
    } else {
        a.actor < b.actor
    }
}

/// The stamp after (`wall`, `counter`) for `actor`: the counter moves up by one.
pub fn hlc_bump(wall: u64, counter: u32, actor: u64) -> Result<Hlc, Error> {
    if counter == u32::MAX {
        Err(Error::Overflow)
    } else {
        Ok(Hlc {
            wall,
            counter: counter + 1,
            actor,
        })
    }
}

/// A local event (or send) at physical time `now`: the new stamp is greater than `local`.
pub fn hlc_send(local: &Hlc, now: u64) -> Result<Hlc, Error> {
    if local.wall < now {
        Ok(Hlc {
            wall: now,
            counter: 0,
            actor: local.actor,
        })
    } else {
        hlc_bump(local.wall, local.counter, local.actor)
    }
}

fn max_u64(a: u64, b: u64) -> u64 {
    if a < b { b } else { a }
}

fn max_u32(a: u32, b: u32) -> u32 {
    if a < b { b } else { a }
}

/// A receive of `remote` at physical time `now`: the new stamp is greater than `local`, and its
/// (wall, counter) is greater than `remote`'s, so it is greater than `remote` whatever the actors.
pub fn hlc_recv(local: &Hlc, remote: &Hlc, now: u64) -> Result<Hlc, Error> {
    let wall = max_u64(max_u64(local.wall, remote.wall), now);
    if wall == local.wall {
        if wall == remote.wall {
            hlc_bump(wall, max_u32(local.counter, remote.counter), local.actor)
        } else {
            hlc_bump(wall, local.counter, local.actor)
        }
    } else if wall == remote.wall {
        hlc_bump(wall, remote.counter, local.actor)
    } else {
        Ok(Hlc {
            wall,
            counter: 0,
            actor: local.actor,
        })
    }
}

// ---------------------------------------------------------------------------------------------------
// Sorted lists

pub fn stamps_copy(a: &Stamps) -> Stamps {
    match a {
        Stamps::Nil => Stamps::Nil,
        Stamps::Cons(x, rest) => Stamps::Cons(*x, Box::new(stamps_copy(rest))),
    }
}

/// Sorted union of two stamp sets.
pub fn stamps_union(a: &Stamps, b: &Stamps) -> Stamps {
    match a {
        Stamps::Nil => stamps_copy(b),
        Stamps::Cons(x, ra) => match b {
            Stamps::Nil => stamps_copy(a),
            Stamps::Cons(y, rb) => {
                if hlc_lt(x, y) {
                    Stamps::Cons(*x, Box::new(stamps_union(ra, b)))
                } else if hlc_lt(y, x) {
                    Stamps::Cons(*y, Box::new(stamps_union(a, rb)))
                } else {
                    Stamps::Cons(*x, Box::new(stamps_union(ra, rb)))
                }
            }
        },
    }
}

pub fn tagged_copy(a: &Tagged) -> Tagged {
    match a {
        Tagged::Nil => Tagged::Nil,
        Tagged::Cons(k, v, rest) => Tagged::Cons(*k, *v, Box::new(tagged_copy(rest))),
    }
}

/// Sorted union of two stamp maps; one stamp in both keeps the larger value.
pub fn tagged_union(a: &Tagged, b: &Tagged) -> Tagged {
    match a {
        Tagged::Nil => tagged_copy(b),
        Tagged::Cons(x, u, ra) => match b {
            Tagged::Nil => tagged_copy(a),
            Tagged::Cons(y, v, rb) => {
                if hlc_lt(x, y) {
                    Tagged::Cons(*x, *u, Box::new(tagged_union(ra, b)))
                } else if hlc_lt(y, x) {
                    Tagged::Cons(*y, *v, Box::new(tagged_union(a, rb)))
                } else {
                    let w = if *u < *v { *v } else { *u };
                    Tagged::Cons(*x, w, Box::new(tagged_union(ra, rb)))
                }
            }
        },
    }
}

pub fn counts_copy(a: &Counts) -> Counts {
    match a {
        Counts::Nil => Counts::Nil,
        Counts::Cons(k, v, rest) => Counts::Cons(*k, *v, Box::new(counts_copy(rest))),
    }
}

/// Sorted union of two count maps; one actor in both keeps the larger count.
pub fn counts_union(a: &Counts, b: &Counts) -> Counts {
    match a {
        Counts::Nil => counts_copy(b),
        Counts::Cons(x, u, ra) => match b {
            Counts::Nil => counts_copy(a),
            Counts::Cons(y, v, rb) => {
                if *x < *y {
                    Counts::Cons(*x, *u, Box::new(counts_union(ra, b)))
                } else if *y < *x {
                    Counts::Cons(*y, *v, Box::new(counts_union(a, rb)))
                } else {
                    let w = if *u < *v { *v } else { *u };
                    Counts::Cons(*x, w, Box::new(counts_union(ra, rb)))
                }
            }
        },
    }
}

/// Add `n` to `actor`'s count (inserting it at 0 first if absent).
pub fn counts_bump(c: &Counts, actor: u64, n: u64) -> Result<Counts, Error> {
    match c {
        Counts::Nil => Ok(Counts::Cons(actor, n, Box::new(Counts::Nil))),
        Counts::Cons(k, v, rest) => {
            if actor < *k {
                Ok(Counts::Cons(actor, n, Box::new(counts_copy(c))))
            } else if *k < actor {
                match counts_bump(rest, actor, n) {
                    Ok(r) => Ok(Counts::Cons(*k, *v, Box::new(r))),
                    Err(e) => Err(e),
                }
            } else if u64::MAX - n < *v {
                Err(Error::Overflow)
            } else {
                Ok(Counts::Cons(*k, *v + n, Box::new(counts_copy(rest))))
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Rules

/// LastWriter: the greater (stamp, value) wins.
pub fn register_merge(a: &Register, b: &Register) -> Register {
    if hlc_lt(&a.stamp, &b.stamp) {
        *b
    } else if hlc_lt(&b.stamp, &a.stamp) {
        *a
    } else if a.value < b.value {
        *b
    } else {
        *a
    }
}

/// AddWins and KeepBoth: union of the entries and of the tombstones.
pub fn tombstoned_merge(a: &Tombstoned, b: &Tombstoned) -> Tombstoned {
    Tombstoned {
        entries: tagged_union(&a.entries, &b.entries),
        removed: stamps_union(&a.removed, &b.removed),
    }
}

/// Counter: per-actor max of the increments and of the decrements.
pub fn pn_merge(a: &Pn, b: &Pn) -> Pn {
    Pn {
        inc: counts_union(&a.inc, &b.inc),
        dec: counts_union(&a.dec, &b.dec),
    }
}

/// Join two states under `rule`. `Mismatch` if either value is not that rule's shape.
pub fn merge(rule: Rule, a: &Value, b: &Value) -> Result<Value, Error> {
    match rule {
        Rule::LastWriter => match a {
            Value::Register(x) => match b {
                Value::Register(y) => Ok(Value::Register(register_merge(x, y))),
                _ => Err(Error::Mismatch),
            },
            _ => Err(Error::Mismatch),
        },
        Rule::AddWins => match a {
            Value::Set(x) => match b {
                Value::Set(y) => Ok(Value::Set(tombstoned_merge(x, y))),
                _ => Err(Error::Mismatch),
            },
            _ => Err(Error::Mismatch),
        },
        Rule::Counter => match a {
            Value::Counter(x) => match b {
                Value::Counter(y) => Ok(Value::Counter(pn_merge(x, y))),
                _ => Err(Error::Mismatch),
            },
            _ => Err(Error::Mismatch),
        },
        Rule::KeepBoth => match a {
            Value::Multi(x) => match b {
                Value::Multi(y) => Ok(Value::Multi(tombstoned_merge(x, y))),
                _ => Err(Error::Mismatch),
            },
            _ => Err(Error::Mismatch),
        },
    }
}

/// Merge a struct component member by member, each under its own rule. `Mismatch` if the lists differ
/// in length or a member does not fit its rule.
pub fn merge_fields(rules: &Rules, a: &Fields, b: &Fields) -> Result<Fields, Error> {
    match rules {
        Rules::Nil => match a {
            Fields::Nil => match b {
                Fields::Nil => Ok(Fields::Nil),
                Fields::Cons(_, _) => Err(Error::Mismatch),
            },
            Fields::Cons(_, _) => Err(Error::Mismatch),
        },
        Rules::Cons(rule, rules_rest) => match a {
            Fields::Nil => Err(Error::Mismatch),
            Fields::Cons(x, ra) => match b {
                Fields::Nil => Err(Error::Mismatch),
                Fields::Cons(y, rb) => match merge(*rule, x, y) {
                    Err(e) => Err(e),
                    Ok(v) => match merge_fields(rules_rest, ra, rb) {
                        Err(e) => Err(e),
                        Ok(rest) => Ok(Fields::Cons(v, Box::new(rest))),
                    },
                },
            },
        },
    }
}

// ---------------------------------------------------------------------------------------------------
// Operations

/// Apply a prepared operation to a state. Every op except Inc and Dec is a one-entry state joined with
/// `merge`; Inc and Dec add to one actor's count. Concurrent ops commute (proven in `lean/`).
pub fn apply(rule: Rule, state: &Value, op: &Op) -> Result<Value, Error> {
    match op {
        Op::Write(stamp, value) => {
            let delta = Value::Register(Register {
                stamp: *stamp,
                value: *value,
            });
            merge(rule, state, &delta)
        }
        Op::Add(tag, element) => {
            let delta = Value::Set(Tombstoned {
                entries: Tagged::Cons(*tag, *element, Box::new(Tagged::Nil)),
                removed: Stamps::Nil,
            });
            merge(rule, state, &delta)
        }
        Op::Remove(seen) => {
            let delta = Value::Set(Tombstoned {
                entries: Tagged::Nil,
                removed: stamps_copy(seen),
            });
            merge(rule, state, &delta)
        }
        Op::Put(tag, value, seen) => {
            let delta = Value::Multi(Tombstoned {
                entries: Tagged::Cons(*tag, *value, Box::new(Tagged::Nil)),
                removed: stamps_copy(seen),
            });
            merge(rule, state, &delta)
        }
        Op::Inc(actor, n) => match rule {
            Rule::Counter => match state {
                Value::Counter(pn) => match counts_bump(&pn.inc, *actor, *n) {
                    Ok(inc) => Ok(Value::Counter(Pn {
                        inc,
                        dec: counts_copy(&pn.dec),
                    })),
                    Err(e) => Err(e),
                },
                _ => Err(Error::Mismatch),
            },
            _ => Err(Error::Mismatch),
        },
        Op::Dec(actor, n) => match rule {
            Rule::Counter => match state {
                Value::Counter(pn) => match counts_bump(&pn.dec, *actor, *n) {
                    Ok(dec) => Ok(Value::Counter(Pn {
                        inc: counts_copy(&pn.inc),
                        dec,
                    })),
                    Err(e) => Err(e),
                },
                _ => Err(Error::Mismatch),
            },
            _ => Err(Error::Mismatch),
        },
    }
}

// ---------------------------------------------------------------------------------------------------
// Validation of untrusted states

pub fn stamps_sorted(a: &Stamps) -> bool {
    match a {
        Stamps::Nil => true,
        Stamps::Cons(x, rest) => match &**rest {
            Stamps::Nil => true,
            Stamps::Cons(y, _) => {
                if hlc_lt(x, y) {
                    stamps_sorted(rest)
                } else {
                    false
                }
            }
        },
    }
}

pub fn tagged_sorted(a: &Tagged) -> bool {
    match a {
        Tagged::Nil => true,
        Tagged::Cons(x, _, rest) => match &**rest {
            Tagged::Nil => true,
            Tagged::Cons(y, _, _) => {
                if hlc_lt(x, y) {
                    tagged_sorted(rest)
                } else {
                    false
                }
            }
        },
    }
}

pub fn counts_sorted(a: &Counts) -> bool {
    match a {
        Counts::Nil => true,
        Counts::Cons(x, _, rest) => match &**rest {
            Counts::Nil => true,
            Counts::Cons(y, _, _) => {
                if *x < *y {
                    counts_sorted(rest)
                } else {
                    false
                }
            }
        },
    }
}

/// Is `value` a well-formed state for `rule`? `merge` and `apply` keep this, and the laws assume it.
pub fn validate(rule: Rule, value: &Value) -> bool {
    match rule {
        Rule::LastWriter => match value {
            Value::Register(_) => true,
            _ => false,
        },
        Rule::AddWins => match value {
            Value::Set(s) => {
                if tagged_sorted(&s.entries) {
                    stamps_sorted(&s.removed)
                } else {
                    false
                }
            }
            _ => false,
        },
        Rule::Counter => match value {
            Value::Counter(c) => {
                if counts_sorted(&c.inc) {
                    counts_sorted(&c.dec)
                } else {
                    false
                }
            }
            _ => false,
        },
        Rule::KeepBoth => match value {
            Value::Multi(s) => {
                if tagged_sorted(&s.entries) {
                    stamps_sorted(&s.removed)
                } else {
                    false
                }
            }
            _ => false,
        },
    }
}
