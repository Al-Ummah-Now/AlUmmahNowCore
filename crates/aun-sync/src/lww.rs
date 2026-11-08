//! Last-writer-wins registers.
//!
//! A [`Stamp`] is `(timestamp ms, device id)`, totally ordered: the later timestamp wins and equal
//! timestamps are broken by device id. Joining (merging) two [`Lww`] registers keeps the one with the greater
//! stamp. If two registers carry the *same* stamp but different values (which only happens if a
//! device reuses a stamp), the larger serialised value wins. That keeps `merge` a deterministic
//! maximum, so it is commutative, associative and idempotent: a state-based CRDT.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

/// When and where a write happened.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Stamp {
    pub ts_ms: i64,
    pub device: String,
}

impl Stamp {
    pub fn new(ts_ms: i64, device: impl Into<String>) -> Self {
        Self {
            ts_ms,
            device: device.into(),
        }
    }

    /// The stamp of a field that was never written; every real write beats it.
    pub fn zero() -> Self {
        Self::default()
    }

    pub fn is_zero(&self) -> bool {
        self.ts_ms == 0 && self.device.is_empty()
    }
}

impl PartialOrd for Stamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Stamp {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ts_ms
            .cmp(&other.ts_ms)
            .then_with(|| self.device.cmp(&other.device))
    }
}

/// A state-based CRDT join: `join` must be commutative, associative and idempotent.
pub trait Merge {
    fn join(&mut self, other: &Self);
}

/// A last-writer-wins register.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lww<T> {
    pub value: T,
    pub stamp: Stamp,
}

impl<T: Default> Default for Lww<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            stamp: Stamp::zero(),
        }
    }
}

impl<T: Clone + PartialEq + Serialize> Lww<T> {
    pub fn new(value: T, stamp: Stamp) -> Self {
        Self { value, stamp }
    }

    /// Writes `value` if `stamp` is newer than the current one; returns whether it applied.
    pub fn set(&mut self, value: T, stamp: Stamp) -> bool {
        let candidate = Lww { value, stamp };
        if candidate.beats(self) {
            *self = candidate;
            true
        } else {
            false
        }
    }

    /// A local user action: always applies. The stamp is bumped past the one already observed
    /// (Lamport-style), so a register holding a server stamp or a future stamp from a skewed clock
    /// cannot swallow the edit. Returns the stamp used.
    pub fn write(&mut self, value: T, stamp: Stamp) -> Stamp {
        let stamp = if stamp > self.stamp {
            stamp
        } else {
            Stamp::new(self.stamp.ts_ms.saturating_add(1), stamp.device)
        };
        self.value = value;
        self.stamp = stamp.clone();
        stamp
    }

    fn beats(&self, other: &Self) -> bool {
        match self.stamp.cmp(&other.stamp) {
            Ordering::Greater => true,
            Ordering::Less => false,
            Ordering::Equal => {
                self.value != other.value
                    && serde_json::to_string(&self.value).unwrap_or_default()
                        > serde_json::to_string(&other.value).unwrap_or_default()
            }
        }
    }
}

impl<T: Clone + PartialEq + Serialize> Merge for Lww<T> {
    fn join(&mut self, other: &Self) {
        if other.beats(self) {
            *self = other.clone();
        }
    }
}

impl<T: Merge + Clone> Merge for Option<T> {
    fn join(&mut self, other: &Self) {
        match (self.as_mut(), other) {
            (_, None) => {}
            (None, Some(o)) => *self = Some(o.clone()),
            (Some(s), Some(o)) => s.join(o),
        }
    }
}

impl<V: Merge + Clone> Merge for std::collections::BTreeMap<String, V> {
    fn join(&mut self, other: &Self) {
        for (k, v) in other {
            match self.get_mut(k) {
                Some(mine) => mine.join(v),
                None => {
                    self.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_write_beats_observed_stamp() {
        let mut r = Lww::new(1u32, Stamp::new(5_000, "~server"));
        let used = r.write(2, Stamp::new(5_000, "phone"));
        assert_eq!((r.value, used.ts_ms), (2, 5_001));
        let mut future = Lww::new(1u32, Stamp::new(9_999_999, "tv"));
        future.write(3, Stamp::new(10, "phone"));
        assert_eq!(future.value, 3);
        assert!(future.stamp > Stamp::new(9_999_999, "tv"));
        let used = future.write(4, Stamp::new(99_999_999, "phone"));
        assert_eq!(used.ts_ms, 99_999_999, "a newer clock stamp is used as is");
    }

    use super::*;

    #[test]
    fn stamp_order() {
        assert!(Stamp::new(2, "a") > Stamp::new(1, "z"));
        assert!(Stamp::new(1, "b") > Stamp::new(1, "a"));
        assert!(Stamp::new(1, "a") > Stamp::zero());
        assert!(Stamp::zero().is_zero());
    }

    #[test]
    fn set_and_merge() {
        let mut r = Lww::new(1u64, Stamp::new(10, "a"));
        assert!(!r.set(2, Stamp::new(9, "b")), "older write ignored");
        assert!(r.set(3, Stamp::new(10, "b")), "device tiebreak");
        assert_eq!(r.value, 3);
        let mut x = Lww::new(5u64, Stamp::new(1, "a"));
        x.join(&r);
        assert_eq!(x, r);
        // Same stamp, different values: deterministic either way round.
        let p = Lww::new("x".to_string(), Stamp::new(1, "a"));
        let q = Lww::new("y".to_string(), Stamp::new(1, "a"));
        let mut pq = p.clone();
        pq.join(&q);
        let mut qp = q.clone();
        qp.join(&p);
        assert_eq!(pq, qp);
    }
}
