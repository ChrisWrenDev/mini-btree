use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};

use crate::error::{CustomError, CustomResult};

type FrameId = usize;

#[derive(Debug)]
pub struct LRUKNode {
    /// K parameter: distance is measured to the K-th most recent access.
    k: usize,
    /// At most `k` timestamps in ascending recency:
    /// - `front()` is the K-th most recent (oldest in the kept window)
    /// - `back()` is the most recent
    history: VecDeque<u64>,
    /// Whether this frame is allowed to be evicted.
    pub is_evictable: bool,
}

impl LRUKNode {
    fn new(k: usize) -> Self {
        Self {
            k,
            history: VecDeque::with_capacity(k),
            is_evictable: false,
        }
    }

    /// Record a new access at timestamp `ts`.
    /// Keeps at most `k` entries: drops oldest when exceeding k.
    fn record_access(&mut self, ts: u64) {
        if self.history.len() == self.k {
            self.history.pop_front();
        }
        self.history.push_back(ts);
    }

    /// Number of accesses we currently remember (≤ k).
    #[inline]
    fn len(&self) -> usize {
        self.history.len()
    }

    /// Most recent access time (if any).
    #[inline]
    fn last_ts(&self) -> Option<u64> {
        self.history.back().copied()
    }

    /// K-th most recent access time (only defined if len() == k).
    #[inline]
    fn kth_ts(&self) -> Option<u64> {
        if self.history.len() == self.k {
            self.history.front().copied()
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub struct LRUKReplacer {
    /// Count of evictable frames currently tracked.
    current_size: usize,
    /// Maximum number of frames the replacer can track.
    capacity: usize,
    /// K parameter.
    k: usize,
    /// Map from frame id to node.
    pub node_store: HashMap<FrameId, LRUKNode>,
    /// Monotonic logical time for ordering accesses.
    current_timestamp: u64,
}

impl LRUKReplacer {
    /// Create a new LRU-K replacer with `capacity` frames and parameter `k`.
    ///
    /// # Panics
    /// Panics if `k == 0` or `capacity == 0`.
    pub fn new(capacity: usize, k: usize) -> Self {
        assert!(k >= 1, "k must be >= 1");
        assert!(capacity >= 1, "capacity must be >= 1");
        Self {
            current_size: 0,
            capacity,
            k,
            node_store: HashMap::with_capacity(capacity),
            current_timestamp: 0,
        }
    }

    /// Record an access to `frame_id`.
    ///
    /// - Creates the node if it doesn't exist (as long as there is room for bookkeeping).
    /// - Increments the global logical timestamp per access.
    /// - Returns an error if the number of **tracked frames** would exceed capacity.
    pub fn record_access(&mut self, frame_id: FrameId) -> CustomResult<()> {
        // Bump logical time (monotonic). This avoids subtle underflow later.
        // If you prefer overflow-wrapping semantics, replace with `self.current_timestamp = self.current_timestamp.wrapping_add(1);`
        if let Some(next) = self.current_timestamp.checked_add(1) {
            self.current_timestamp = next;
        } else {
            // Extremely unlikely in practice. Reset to 0 and continue deterministically.
            self.current_timestamp = 0;
        }

        if let Some(node) = self.node_store.get_mut(&frame_id) {
            node.record_access(self.current_timestamp);
            return Ok(());
        }

        // New frame: ensure we don't exceed tracking capacity.
        if self.node_store.len() >= self.capacity {
            return Err(CustomError::Internal(
                "replacer bookkeeping exceeds capacity".to_string(),
            ));
        }

        let mut node = LRUKNode::new(self.k);
        node.record_access(self.current_timestamp);
        self.node_store.insert(frame_id, node);
        Ok(())
    }

    /// Set whether a frame is evictable.
    ///
    /// Adjusts `current_size` accordingly. Returns an error if the frame does not exist.
    pub fn set_evictable(&mut self, frame_id: FrameId, set_evictable: bool) -> CustomResult<()> {
        match self.node_store.get_mut(&frame_id) {
            None => Err(CustomError::Internal("frame not found".into())),
            Some(node) => {
                let was = node.is_evictable;
                node.is_evictable = set_evictable;
                match (was, set_evictable) {
                    (false, true) => self.current_size += 1,
                    (true, false) => self.current_size -= 1,
                    _ => {}
                }
                debug_assert_eq!(
                    self.current_size,
                    self.node_store.values().filter(|n| n.is_evictable).count()
                );
                Ok(())
            }
        }
    }

    /// Remove a frame from the replacer.
    ///
    /// - Returns an error if the frame exists but is **not evictable**.
    /// - Returns `Ok(())` if the frame does not exist (idempotent remove).
    pub fn remove(&mut self, frame_id: FrameId) -> CustomResult<()> {
        match self.node_store.get(&frame_id) {
            None => Ok(()), // idempotent
            Some(node) if !node.is_evictable => {
                Err(CustomError::Internal("frame is not evictable".into()))
            }
            Some(_) => {
                let node = self.node_store.remove(&frame_id).expect("present");
                if node.is_evictable {
                    self.current_size -= 1;
                }
                debug_assert_eq!(
                    self.current_size,
                    self.node_store.values().filter(|n| n.is_evictable).count()
                );
                Ok(())
            }
        }
    }

    /// Choose a victim frame to evict, if any, and remove it from the replacer.
    ///
    /// Eviction policy (LRU-K):
    /// - Prefer frames with **fewer than K references** (treated as ∞ K-distance).
    /// - Among equals, prefer the one with **older most-recent access**.
    /// - Final deterministic tiebreak by `FrameId` (smaller first).
    ///
    /// Returns `Some(frame_id)` on success and `None` if no evictable frame exists.
    pub fn evict(&mut self) -> Option<FrameId> {
        // Candidate we will evict (if any), represented as a comparable key.
        // We pick the MAX key according to our ordering.
        #[derive(Copy, Clone, Debug)]
        struct Key {
            /// K-distance: (now - kth_ts) for nodes with ≥ K references; ∞ otherwise.
            k_dist: u128,
            /// Most recent access (we invert comparison: older last_ts should win eviction).
            last_ts: u64,
            /// Final tiebreaker for determinism (smaller id should be evicted earlier).
            frame_id: FrameId,
        }

        // Manual comparator implementing:
        // 1) larger k_dist first (∞ beats finite)
        // 2) if equal, smaller last_ts first (older beats newer)
        // 3) if equal, smaller frame_id first
        fn better(a: Key, b: Key) -> bool {
            match a.k_dist.cmp(&b.k_dist) {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => match a.last_ts.cmp(&b.last_ts) {
                    Ordering::Less => true, // older wins
                    Ordering::Greater => false,
                    Ordering::Equal => a.frame_id < b.frame_id,
                },
            }
        }

        let mut best: Option<(Key, FrameId)> = None;

        for (&frame_id, node) in self.node_store.iter() {
            if !node.is_evictable {
                continue;
            }

            // ∞ distance for nodes with < K references.
            let k_dist = match node.kth_ts() {
                None => u128::MAX,
                Some(kth) => (self.current_timestamp as u128).saturating_sub(kth as u128),
            };

            // For tie-breaking we want the most recent access time (older is "better" to evict).
            let last_ts = node.last_ts().unwrap_or(0);

            let key = Key {
                k_dist,
                last_ts,
                frame_id,
            };

            if let Some((cur_key, _)) = best {
                if better(key, cur_key) {
                    best = Some((key, frame_id));
                }
            } else {
                best = Some((key, frame_id));
            }
        }

        if let Some((_, victim)) = best {
            // Remove safely; if this errors it means a logic bug because we only
            // selected evictable frames above.
            let _ = self.remove(victim);
            return Some(victim);
        }
        None
    }

    /// Return the number of **evictable** frames.
    #[inline]
    pub fn size(&self) -> usize {
        self.current_size
    }
}
