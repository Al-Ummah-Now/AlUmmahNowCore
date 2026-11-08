#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Property-style tests of the CRDT join: random operation histories on several replicas must
//! converge no matter the merge order, and `join` must be commutative, associative and idempotent.

use aun_sync::{Lww, Merge, Note, Stamp, SyncState, VideoMeta, merged};

/// xorshift64*: deterministic randomness without a dev-dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const VIDEOS: &[&str] = &["v1", "v2", "v3", "v4"];
const DEVICES: &[&str] = &["phone", "tv", "laptop"];

/// Applies one random local operation, as a device would.
fn random_op(rng: &mut Rng, st: &mut SyncState, device: &str, ts: i64) {
    let v = VIDEOS[rng.below(VIDEOS.len() as u64) as usize].to_owned();
    // Small timestamp range so equal timestamps (device tiebreaks) happen often.
    let stamp = Stamp::new(ts, device);
    match rng.below(9) {
        0 | 1 => {
            let w = st.watch.entry(v).or_default();
            w.position_ms.set(rng.below(600) * 1000, stamp.clone());
            w.duration_ms.set(600_000, stamp);
        }
        2 => {
            st.watch
                .entry(v)
                .or_default()
                .completed
                .set(rng.below(2) == 0, stamp);
        }
        3 => {
            st.watch.entry(v).or_default().removed.set(true, stamp);
        }
        4 => {
            let mut c = st.history_cleared.clone().unwrap_or_default();
            c.set(true, stamp);
            st.history_cleared = Some(c);
        }
        5 => {
            st.saved.entry(v).or_default().set(rng.below(2) == 0, stamp);
        }
        6 => {
            st.liked.entry(v).or_default().set(rng.below(2) == 0, stamp);
        }
        7 => {
            let id = format!("n{}", rng.below(5));
            let note = st.notes.entry(id).or_insert_with(|| Note {
                video_id: v.clone(),
                created_at_ms: ts,
                ..Note::default()
            });
            match rng.below(3) {
                0 => {
                    note.text.set(format!("text {}", rng.below(100)), stamp);
                }
                1 => {
                    note.position_ms.set(rng.below(100_000), stamp);
                }
                _ => {
                    note.removed.set(true, stamp);
                }
            }
        }
        _ => {
            let mut order: Vec<String> = VIDEOS.iter().map(|s| (*s).to_owned()).collect();
            let k = rng.below(order.len() as u64) as usize;
            order.rotate_left(k);
            st.saved_order.set(order, stamp.clone());
            st.meta.entry(v).or_default().set(
                VideoMeta {
                    title: format!("t{}", rng.below(3)),
                    thumbnail_url: None,
                    channel_name: None,
                },
                stamp,
            );
        }
    }
}

fn random_states(seed: u64) -> Vec<SyncState> {
    let mut rng = Rng(seed | 1);
    let mut replicas = vec![SyncState::default(); DEVICES.len()];
    for _ in 0..40 {
        let d = rng.below(DEVICES.len() as u64) as usize;
        let ts = 1_000 + rng.below(20) as i64;
        random_op(&mut rng, &mut replicas[d], DEVICES[d], ts);
        // Occasionally gossip: one replica merges another mid-history.
        if rng.below(6) == 0 {
            let from = rng.below(DEVICES.len() as u64) as usize;
            let other = replicas[from].clone();
            replicas[d].join(&other);
        }
    }
    replicas
}

#[test]
fn join_is_commutative_associative_idempotent() {
    for seed in 1..400u64 {
        let r = random_states(seed);
        let (a, b, c) = (&r[0], &r[1], &r[2]);
        assert_eq!(merged(a, b), merged(b, a), "commutative, seed {seed}");
        assert_eq!(
            merged(&merged(a, b), c),
            merged(a, &merged(b, c)),
            "associative, seed {seed}"
        );
        assert_eq!(merged(a, a), *a, "idempotent, seed {seed}");
        let ab = merged(a, b);
        assert_eq!(merged(&ab, b), ab, "absorbs, seed {seed}");
    }
}

#[test]
fn replicas_converge_in_any_merge_order() {
    for seed in 1..200u64 {
        let r = random_states(seed);
        let orders: [[usize; 3]; 6] = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
        let results: Vec<SyncState> = orders
            .iter()
            .map(|o| {
                let mut s = r[o[0]].clone();
                s.join(&r[o[1]]);
                s.join(&r[o[2]]);
                s
            })
            .collect();
        for res in &results[1..] {
            assert_eq!(res, &results[0], "seed {seed}");
        }
    }
}

#[test]
fn lww_concurrent_writes_resolve_by_time_then_device() {
    let mut phone = SyncState::default();
    let mut tv = SyncState::default();
    phone
        .watch
        .entry("v".into())
        .or_default()
        .position_ms
        .set(100_000, Stamp::new(5_000, "phone"));
    tv.watch
        .entry("v".into())
        .or_default()
        .position_ms
        .set(200_000, Stamp::new(5_000, "tv"));
    tv.watch
        .entry("v".into())
        .or_default()
        .completed
        .set(true, Stamp::new(4_000, "tv"));
    let m = merged(&phone, &tv);
    assert_eq!(
        m.watch["v"].position_ms.value, 200_000,
        "same ms: device id breaks the tie"
    );
    assert!(m.watch["v"].completed.value, "fields merge independently");
    // Serialisation round trip keeps the state (what the store and FFI exchange use).
    let json = serde_json::to_string(&m).unwrap();
    assert_eq!(serde_json::from_str::<SyncState>(&json).unwrap(), m);
    let _ = Lww::new(1u8, Stamp::zero());
}
