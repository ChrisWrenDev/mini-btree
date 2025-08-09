use crate::buffer::LRUKReplacer;
use crate::error::CustomError;

type FrameId = usize;

// --- Helpers -------------------------------------------------------------

/// Drain up to `n` evictions and return the victim order.
fn evict_n(r: &mut LRUKReplacer, n: usize) -> Vec<FrameId> {
    let mut out = Vec::new();
    for _ in 0..n {
        match r.evict() {
            Some(id) => out.push(id),
            None => break,
        }
    }
    out
}

/// Count evictable frames by scanning the store (cross-check for invariants).
fn count_evictable_scan(r: &LRUKReplacer) -> usize {
    r.node_store.values().filter(|n| n.is_evictable).count()
}

// --- Construction / parameter guards ------------------------------------

#[test]
#[should_panic(expected = "k must be >= 1")]
fn new_panics_when_k_zero() {
    let _ = LRUKReplacer::new(4, 0);
}

#[test]
#[should_panic(expected = "capacity must be >= 1")]
fn new_panics_when_capacity_zero() {
    let _ = LRUKReplacer::new(0, 2);
}

// --- Basic flow and size accounting -------------------------------------

#[test]
fn size_reflects_evictable_frames() {
    let mut r = LRUKReplacer::new(8, 2);
    r.record_access(1).unwrap();
    r.record_access(2).unwrap();

    assert_eq!(r.size(), 0);
    r.set_evictable(1, true).unwrap();
    assert_eq!(r.size(), 1);
    r.set_evictable(2, true).unwrap();
    assert_eq!(r.size(), 2);

    r.set_evictable(1, false).unwrap();
    assert_eq!(r.size(), 1);
    assert_eq!(r.size(), count_evictable_scan(&r));
}

// --- Record access creates node; capacity enforcement --------------------

#[test]
fn record_access_creates_node_up_to_capacity() {
    let mut r = LRUKReplacer::new(2, 2);
    r.record_access(10).unwrap();
    r.record_access(11).unwrap();
    // Next *new* frame would exceed bookkeeping capacity.
    let err = r.record_access(12).unwrap_err();
    match err {
        CustomError::Internal(s) => assert!(s.contains("capacity")),
        _ => {}
    }
}

// --- Eviction when no evictables ----------------------------------------

#[test]
fn evict_none_when_no_evictables() {
    let mut r = LRUKReplacer::new(4, 2);
    r.record_access(1).unwrap();
    r.record_access(2).unwrap();
    assert_eq!(r.evict(), None);
}

// --- Remove behavior -----------------------------------------------------

#[test]
fn remove_rules() {
    let mut r = LRUKReplacer::new(4, 2);

    // Removing a non-existent frame is OK (idempotent).
    assert!(r.remove(99).is_ok());

    // Not evictable -> error to remove.
    r.record_access(7).unwrap();
    assert!(r.remove(7).is_err());

    // Make evictable and remove -> size decremented.
    r.set_evictable(7, true).unwrap();
    assert_eq!(r.size(), 1);
    r.remove(7).unwrap();
    assert_eq!(r.size(), 0);

    // Removing again is fine.
    assert!(r.remove(7).is_ok());
}

// --- LRU-K semantics: <K has infinite distance --------------------------

#[test]
fn infinite_distance_wins_before_reaching_k() {
    // k=2: frames with only 1 access are "infinite" K-distance.
    let mut r = LRUKReplacer::new(8, 2);

    // Frame 1 (one access), evictable
    r.record_access(1).unwrap();
    r.set_evictable(1, true).unwrap();

    // Frame 2 with two accesses (distance finite)
    r.record_access(2).unwrap();
    r.record_access(2).unwrap();
    r.set_evictable(2, true).unwrap();

    // Frame 3 (one access), younger than 1
    r.record_access(3).unwrap();
    r.set_evictable(3, true).unwrap();

    // Eviction order: among infinite distances (1,3), pick older last_ts (1), then 3, then 2.
    assert_eq!(evict_n(&mut r, 3), vec![1, 3, 2]);
}

// --- Exactly K references: distance equals now - kth_ts ------------------

#[test]
fn distance_uses_kth_most_recent_after_k_accesses() {
    // k=3: need three accesses to become finite.
    let mut r = LRUKReplacer::new(8, 3);

    // A: 3 accesses -> finite, kth_ts = first ts
    r.record_access(1).unwrap();
    r.record_access(1).unwrap();
    r.record_access(1).unwrap();
    r.set_evictable(1, true).unwrap();

    // B: only 2 accesses -> infinite so far; should evict before A
    r.record_access(2).unwrap();
    r.record_access(2).unwrap();
    r.set_evictable(2, true).unwrap();

    // C: 3 accesses -> finite; choose between A and C by larger k_dist / older last_ts
    r.record_access(3).unwrap();
    r.record_access(3).unwrap();
    r.record_access(3).unwrap();
    r.set_evictable(3, true).unwrap();

    // First victim is B (infinite).
    assert_eq!(r.evict(), Some(2));

    // Now compare A vs C: both finite. The one with larger (now - kth_ts) should be evicted.
    // Since A's kth (first) is older than C's, A should go next.
    assert_eq!(r.evict(), Some(1));
    assert_eq!(r.evict(), Some(3));
}

// --- Tie-breaking: k_dist, then last_ts (older wins), then frame_id ------

#[test]
fn tie_breaking_is_deterministic() {
    let mut r = LRUKReplacer::new(16, 2);

    // Make three frames with <k references (all infinite).
    // Access order sets last_ts increasing: 10 (oldest), 2, 5 (newest).
    for id in [10, 2, 5] {
        r.record_access(id).unwrap();
        r.set_evictable(id, true).unwrap();
    }
    // Evict: older last_ts first -> 10, then 2, then 5
    assert_eq!(evict_n(&mut r, 3), vec![10, 2, 5]);

    // Now make two frames with equal finite k_dist:
    // Give both exactly 2 accesses; stage so kth_ts is equal.
    let mut r = LRUKReplacer::new(16, 2);

    // X and Y share the same kth_ts by interleaving accesses.
    r.record_access(100).unwrap(); // X first (kth candidate)
    r.record_access(200).unwrap(); // Y first (kth candidate)
    r.record_access(100).unwrap(); // X second (k=2 reached)
    r.record_access(200).unwrap(); // Y second (k=2 reached)
    r.set_evictable(100, true).unwrap();
    r.set_evictable(200, true).unwrap();

    // Make last_ts equal by not touching either.
    // With equal k_dist and last_ts, tie-break by frame id (smaller first).
    assert_eq!(evict_n(&mut r, 2), vec![100, 200]);
}

// --- Interleaving accesses around the k-th threshold ---------------------

#[test]
fn crossing_k_threshold_moves_between_infinite_and_finite_buckets() {
    let mut r = LRUKReplacer::new(8, 3);

    // A has 2 accesses (<k) => infinite
    r.record_access(1).unwrap();
    r.record_access(1).unwrap();
    r.set_evictable(1, true).unwrap();

    // B has 3 accesses (==k) => finite
    r.record_access(2).unwrap();
    r.record_access(2).unwrap();
    r.record_access(2).unwrap();
    r.set_evictable(2, true).unwrap();

    // C has 1 access (<k) => infinite, but younger than A
    r.record_access(3).unwrap();
    r.set_evictable(3, true).unwrap();

    // Evict among infinite: A (older) then C; then B (finite).
    assert_eq!(r.evict(), Some(1));
    assert_eq!(r.evict(), Some(3));

    // Make a new frame D and immediately give it 3 accesses; it becomes finite.
    r.record_access(4).unwrap();
    r.record_access(4).unwrap();
    r.record_access(4).unwrap();
    r.set_evictable(4, true).unwrap();

    // Between finite frames B and D: whichever has larger k_dist goes first (older kth_ts).
    let victims = evict_n(&mut r, 2);
    assert_eq!(victims.len(), 2);
    assert!(victims.contains(&2) && victims.contains(&4));
}

// --- Eviction empties size; subsequent evict returns None ----------------

#[test]
fn evict_drains_and_none_after() {
    let mut r = LRUKReplacer::new(8, 2);
    for id in 0..5 {
        r.record_access(id).unwrap();
        r.set_evictable(id, true).unwrap();
    }
    let victims = evict_n(&mut r, 10);
    assert_eq!(victims.len(), 5);
    assert_eq!(r.size(), 0);
    assert_eq!(r.evict(), None);
}

// --- Making non-evictable after accesses prevents eviction ---------------

#[test]
fn non_evictable_frames_are_never_chosen() {
    let mut r = LRUKReplacer::new(8, 2);

    r.record_access(1).unwrap();
    r.record_access(1).unwrap();
    r.set_evictable(1, true).unwrap();

    r.record_access(2).unwrap();
    r.record_access(2).unwrap();
    r.set_evictable(2, true).unwrap();

    // Make 2 non-evictable; only 1 can be evicted.
    r.set_evictable(2, false).unwrap();
    assert_eq!(r.evict(), Some(1));
    assert_eq!(r.evict(), None);
}

// --- Stressy smoke test: random-ish pattern without external crates ------

#[test]
fn smoke_many_updates_interleaved() {
    let mut r = LRUKReplacer::new(64, 3);

    // Create 32 frames with a mix of access counts.
    for i in 0..32 {
        let reps = (i % 4) + 1; // 1..=4 accesses
        for _ in 0..reps {
            r.record_access(i).unwrap();
        }
        // Make every other frame evictable.
        r.set_evictable(i, i % 2 == 0).unwrap();
    }

    // Evict until empty; ensure we never get a non-evictable id and size hits 0.
    let victims = evict_n(&mut r, 1000);
    // All even ids (0..31) were evictable -> 16 victims
    assert_eq!(victims.len(), 16);
    for id in victims {
        assert_eq!(id % 2, 0);
    }
    assert_eq!(r.size(), 0);
    assert_eq!(r.evict(), None);
    assert_eq!(count_evictable_scan(&r), 0);
}
