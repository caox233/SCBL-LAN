//! Pure, bounded policy for byte-identical constant-buffer reuse within ONE pass.
//!
//! No native memory access, allocation, driver calls, resource ownership, or hash-only
//! equality. Integration must validate native pointers BEFORE building UploadRecord
//! slices, clear at every pass boundary, and keep borrowed resources alive and
//! unmodified while cached. This policy does not make cross-pass reuse safe.
//! Map flags and native fallback behavior are outside this module and unchanged.

pub const MAX_PAYLOAD: usize = 4096;
pub const CACHE_SLOTS: usize = 32;

/// Bound unproductive work without treating excluded native bindings as misses.
/// Low hit ratio is measured over eligible cache attempts only. A separate
/// 128-item no-hit window still stops all-ineligible work and stale-consumer tails.
/// Caller checks only BEFORE a new list (<=256 items), never replaying a prefix.
pub fn should_back_off(seen: u64, eligible: u64, hits: u64, last_hit_seen: u64) -> bool {
    if hits > eligible || eligible > seen || last_hit_seen > seen { return true; }
    seen >= 128 && (seen - last_hit_seen >= 128
        || (eligible != 0 && hits < eligible / 8 + u64::from(eligible % 8 != 0)))
}

/// Physical coverage needs every byte. Prefix coverage is only constructed after
/// the native adapter proves the exact consuming shader cannot read its tail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage { Physical, ShaderPrefix { bytes: u16, identity: u64 }, ShaderLeadingGap { bytes: u16, identity: u64 } }
impl Coverage {
    fn len(self, bucket: u8) -> Option<usize> {
        let width = byte_width(bucket)?;
        match self {
            Self::Physical => Some(width),
            Self::ShaderPrefix { bytes, identity } if identity != 0 && bytes != 0
                && usize::from(bytes) < width && bytes % 16 == 0 => Some(usize::from(bytes)),
            Self::ShaderLeadingGap { bytes, identity } if identity != 0 && bytes >= 32
                && usize::from(bytes) < width && bytes % 16 == 0 => Some(usize::from(bytes)),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheKey {
    pub owner: usize,
    pub device: usize,
    pub context: usize,
    pub descriptor: usize,
    /// Native descriptor bucket, 0..=12; ByteWidth is exactly 1 << bucket.
    pub bucket: u8,
    /// Caller-owned descriptor/resource generation. Not a payload hash.
    pub generation: u64,
    pub coverage: Coverage,
}

impl CacheKey {
    const EMPTY: Self = Self {
        owner: 0,
        device: 0,
        context: 0,
        descriptor: 0,
        bucket: 0,
        generation: 0,
        coverage: Coverage::Physical,
    };
}

/// Safe byte view of one native upload record, in ORIGINAL native order.
/// Native metadata: stride 16, offset:u16 +4, size:u16 +6, source:u32 +8.
/// The adapter, not this policy, reads count:u16 at descriptor+0xCD8 and
/// bucket at descriptor+0xCCC. `bytes` must describe exactly `size` source bytes.
#[derive(Clone, Copy, Debug)]
pub struct UploadRecord<'a> {
    pub offset: u16,
    pub size: u16,
    pub bytes: &'a [u8],
}

pub fn byte_width(bucket: u8) -> Option<usize> {
    if bucket <= 12 { Some(1usize << bucket) } else { None }
}

/// Require a nonempty, contiguous, non-overlapping, complete overwrite.
/// WRITE_DISCARD does not zero untouched bytes; holes are NEVER normalized.
/// Empty records are conservatively rejected, even inside a complete layout.
/// Reject reversed native order rather than sorting it or merging overlaps.
pub fn validate_complete_layout(bucket: u8, records: &[UploadRecord<'_>]) -> Option<usize> {
    let width = byte_width(bucket)?;
    let mut cursor = 0usize;
    for record in records {
        let size = usize::from(record.size);
        if size == 0 || usize::from(record.offset) != cursor || record.bytes.len() != size {
            return None;
        }
        cursor = cursor.checked_add(size)?;
        if cursor > width { return None; }
    }
    if cursor == width { Some(width) } else { None }
}

/// Validate ALL records before writing anything. On None, output is unchanged.
/// Only output[..returned_len] is initialized; bytes after it are never compared.
pub fn assemble_payload(
    bucket: u8,
    records: &[UploadRecord<'_>],
    output: &mut [u8; MAX_PAYLOAD],
) -> Option<usize> {
    let width = validate_complete_layout(bucket, records)?;
    for record in records {
        let start = usize::from(record.offset);
        let end = start + usize::from(record.size);
        output[start..end].copy_from_slice(record.bytes);
    }
    Some(width)
}

#[derive(Clone, Copy)]
struct Slot {
    epoch: u64,
    key: CacheKey,
    len: usize,
    /// Non-owning ID. The caller, never this module, controls its lifetime.
    resource: usize,
    bytes: [u8; MAX_PAYLOAD],
}

impl Slot {
    const EMPTY: Self = Self {
        epoch: 0,
        key: CacheKey::EMPTY,
        len: 0,
        resource: 0,
        bytes: [0; MAX_PAYLOAD],
    };
}

/// 32 slots; payload storage is exactly 32 * 4096 bytes, independent of workload.
/// Construction initializes storage once. Ordinary clear changes only the epoch,
/// not the 128 KiB payload storage. Caller must clear at EVERY pass entry/exit.
/// Concurrent use needs external synchronization; no native addresses are read.
pub struct PassCache {
    epoch: u64,
    slots: [Slot; CACHE_SLOTS],
}

impl PassCache {
    pub const fn new() -> Self {
        Self { epoch: 1, slots: [Slot::EMPTY; CACHE_SLOTS] }
    }

    pub fn clear(&mut self) {
        if self.epoch == u64::MAX {
            // Avoid a stale epoch becoming valid after wrap. Metadata-only reset.
            for slot in &mut self.slots { slot.epoch = 0; }
            self.epoch = 1;
        } else {
            self.epoch += 1;
        }
    }

    pub fn lookup(&self, key: CacheKey, payload: &[u8]) -> Option<usize> {
        self.lookup_with_index(key, payload).map(|(resource, _)| resource)
    }

    /// Return the exact slot used by lookup, so the adapter can check its lifetime proof
    /// without hashing the identical full key again. No borrow escapes this call.
    pub fn lookup_with_index(&self, key: CacheKey, payload: &[u8]) -> Option<(usize, usize)> {
        let width = key.coverage.len(key.bucket)?;
        if payload.len() != width { return None; }
        let index = slot_index(key);
        let slot = &self.slots[index];
        if slot.epoch == self.epoch
            && slot.key == key
            && slot.len == width
            && slot.resource != 0
            && slot.bytes[..width] == *payload
        {
            Some((slot.resource, index))
        } else {
            None
        }
    }

    /// Insert/replace one direct-mapped slot only after a successful native upload.
    /// Never insert a resource whose upload failed, whose bytes differ, or whose
    /// ownership/lifetime cannot be guaranteed until clear. Zero is not a resource.
    pub fn insert(&mut self, key: CacheKey, payload: &[u8], borrowed_resource: usize) -> bool {
        self.insert_with_index(key, payload, borrowed_resource).is_some()
    }

    /// Return the slot only after a valid insert; callers associate lifetime metadata
    /// with this exact slot. Invalid input leaves the cache unchanged.
    pub fn insert_with_index(&mut self, key: CacheKey, payload: &[u8], borrowed_resource: usize) -> Option<usize> {
        let width = key.coverage.len(key.bucket)?;
        if payload.len() != width || borrowed_resource == 0 { return None; }
        let index = slot_index(key);
        let slot = &mut self.slots[index];
        slot.epoch = 0;
        slot.key = key;
        slot.len = width;
        slot.resource = borrowed_resource;
        slot.bytes[..width].copy_from_slice(payload);
        slot.epoch = self.epoch;
        Some(index)
    }
}

impl Default for PassCache {
    fn default() -> Self { Self::new() }
}

pub fn slot_index(key: CacheKey) -> usize {
    // Hash chooses a slot ONLY. Lookup independently compares full key + payload.
    // Fold each integer byte so high address/generation bits also affect placement.
    let mut hash = 0xcbf29ce484222325u64;
    let (covered, identity) = match key.coverage {
        Coverage::Physical => (0, 0),
        Coverage::ShaderPrefix { bytes, identity } => (u64::from(bytes), identity),
        Coverage::ShaderLeadingGap { bytes, identity } => (0x1_0000 | u64::from(bytes), identity),
    };
    for value in [key.owner as u64, key.device as u64, key.context as u64,
                  key.descriptor as u64, u64::from(key.bucket), key.generation, covered, identity] {
        for byte in value.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    (hash as usize) & (CACHE_SLOTS - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returned_slot_matches_full_key_and_lifetime_metadata() {
        let mut cache = PassCache::new();
        for descriptor in 1..=256 {
            let k = CacheKey { descriptor, bucket: 4, ..key() };
            let bytes = [descriptor as u8; 16];
            assert_eq!(cache.insert_with_index(k, &bytes, descriptor), Some(slot_index(k)));
            assert_eq!(cache.lookup_with_index(k, &bytes), Some((descriptor, slot_index(k))));
            assert_eq!(cache.lookup(k, &bytes), Some(descriptor));
            assert_eq!(cache.lookup_with_index(CacheKey { generation: k.generation + 1, ..k }, &bytes), None);
            let mut other = bytes; other[0] ^= 1;
            assert_eq!(cache.lookup_with_index(k, &other), None);
        }
    }

    #[test]
    fn invalid_indexed_insert_does_not_replace_a_valid_entry() {
        let mut cache = PassCache::new(); let k = CacheKey { bucket: 4, ..key() }; let bytes = [7; 16];
        assert!(cache.insert(k, &bytes, 11));
        assert_eq!(cache.insert_with_index(k, &bytes, 0), None);
        assert_eq!(cache.insert_with_index(k, &[7; 15], 22), None);
        assert_eq!(cache.lookup_with_index(k, &bytes), Some((11, slot_index(k))));
        cache.clear();
        assert_eq!(cache.lookup_with_index(k, &bytes), None);
    }

    #[test]
    fn budget_uses_attempts_not_excluded_bindings() {
        for (seen, eligible, hits, last_hit, expected) in [
            (0,0,0,0,false), (127,0,0,0,false), (128,0,0,0,true),
            (128,128,15,128,true), (128,128,16,128,false),
            (129,129,16,129,true), (129,129,17,129,false),
            // Real FIX3 counters: excluded items must not dilute useful hits.
            (257,49,32,250,false), (162,28,20,155,false), (316,61,39,310,false),
            (3000,2000,1998,3000,false), (1000,1,1,1000,false),
            (u64::MAX,u64::MAX,u64::MAX,u64::MAX,false),
        ] { assert_eq!(should_back_off(seen,eligible,hits,last_hit),expected); }
    }

    #[test]
    fn no_hit_window_still_bounds_a_tail_after_prior_success() {
        assert!(!should_back_off(255,49,32,128));
        assert!(should_back_off(256,49,32,128));
        assert!(should_back_off(10_000,49,32,128));
        assert!(should_back_off(u64::MAX,1,1,1));
        assert!(!should_back_off(10_000,100,99,9_999));
    }

    #[test]
    fn inconsistent_budget_counters_fail_closed() {
        for (s,e,h,l) in [(1,2,0,0),(2,1,2,1),(2,1,1,3)] {
            assert!(should_back_off(s,e,h,l));
        }
    }

    #[test]
    fn prefix_cache_requires_explicit_identity_and_exact_initialized_length() {
        let mut cache = PassCache::new();
        let k = CacheKey { bucket: 6, coverage: Coverage::ShaderPrefix { bytes: 48, identity: 7 }, ..key() };
        assert!(cache.insert(k, &[2; 48], 1));
        assert_eq!(cache.lookup(k, &[2; 48]), Some(1));
        assert_eq!(cache.lookup(k, &[2; 47]), None);
        assert_eq!(cache.lookup(k, &[2; 64]), None);
        for coverage in [Coverage::Physical, Coverage::ShaderPrefix { bytes: 48, identity: 8 },
            Coverage::ShaderPrefix { bytes: 32, identity: 7 }] {
            assert_eq!(cache.lookup(CacheKey { coverage, ..k }, &[2; 48]), None);
        }
        for (bytes, identity) in [(0, 1), (64, 1), (80, 1), (47, 1), (48, 0)] {
            assert!(!cache.insert(CacheKey { coverage: Coverage::ShaderPrefix { bytes, identity }, ..k }, &[2; 48], 1));
        }
    }

    fn key() -> CacheKey {
        CacheKey { owner: 1, device: 2, context: 3, descriptor: 4, bucket: 2, generation: 5, coverage: Coverage::Physical }
    }
    fn rec(offset: u16, bytes: &[u8]) -> UploadRecord<'_> {
        UploadRecord { offset, size: bytes.len() as u16, bytes }
    }

    #[test]
    fn complete_layout_preserves_native_byte_order() {
        let records = [rec(0, &[1, 2]), rec(2, &[3]), rec(3, &[4])];
        let mut out = [0xAA; MAX_PAYLOAD];
        assert_eq!(assemble_payload(2, &records, &mut out), Some(4));
        assert_eq!(&out[..4], &[1, 2, 3, 4]);
        assert!(out[4..].iter().all(|b| *b == 0xAA));
    }

    #[test]
    fn holes_and_incomplete_tail_are_native_fallback() {
        assert_eq!(validate_complete_layout(2, &[rec(1, &[1, 2, 3])]), None);
        assert_eq!(validate_complete_layout(2, &[rec(0, &[1]), rec(2, &[2, 3])]), None);
        assert_eq!(validate_complete_layout(2, &[rec(0, &[1, 2, 3])]), None);
        assert_eq!(validate_complete_layout(2, &[]), None);
    }

    #[test]
    fn overlaps_reversal_and_empty_records_are_native_fallback() {
        assert_eq!(validate_complete_layout(2, &[rec(0, &[1, 2, 3]), rec(2, &[4, 5])]), None);
        assert_eq!(validate_complete_layout(2, &[rec(2, &[3, 4]), rec(0, &[1, 2])]), None);
        assert_eq!(validate_complete_layout(2, &[rec(0, &[]), rec(0, &[1, 2, 3, 4])]), None);
        assert_eq!(validate_complete_layout(2, &[rec(0, &[1, 2, 3, 4]), rec(4, &[])]), None);
    }

    #[test]
    fn invalid_layout_does_not_partially_write_output() {
        let mut out = [0xCC; MAX_PAYLOAD];
        assert_eq!(assemble_payload(2, &[rec(0, &[1, 2]), rec(3, &[3, 4])], &mut out), None);
        assert!(out.iter().all(|b| *b == 0xCC));
    }

    #[test]
    fn exact_bucket_bounds_and_size_metadata_are_checked() {
        assert_eq!(byte_width(0), Some(1));
        assert_eq!(byte_width(12), Some(4096));
        assert_eq!(byte_width(13), None);
        assert_eq!(byte_width(u8::MAX), None);
        assert_eq!(validate_complete_layout(0, &[rec(0, &[9])]), Some(1));
        assert_eq!(validate_complete_layout(2, &[rec(0, &[1, 2, 3, 4, 5])]), None);
        assert_eq!(validate_complete_layout(12, &[rec(u16::MAX, &[1])]), None);
        assert_eq!(validate_complete_layout(2, &[UploadRecord { offset: 0, size: 4, bytes: &[1, 2, 3] }]), None);
        let max = [0xA5; MAX_PAYLOAD];
        assert_eq!(validate_complete_layout(12, &[rec(0, &max)]), Some(MAX_PAYLOAD));
        assert_eq!(validate_complete_layout(13, &[rec(0, &max)]), None);
    }

    #[test]
    fn exact_duplicate_hits_and_every_byte_change_misses() {
        let mut cache = PassCache::new();
        let bytes = [1, 2, 3, 4];
        assert_eq!(cache.lookup(key(), &bytes), None);
        assert!(cache.insert(key(), &bytes, 101));
        assert_eq!(cache.lookup(key(), &bytes), Some(101));
        for i in 0..bytes.len() {
            let mut changed = bytes; changed[i] ^= 1;
            assert_eq!(cache.lookup(key(), &changed), None);
        }
        assert_eq!(cache.lookup(key(), &[1, 2, 3]), None);
        assert_eq!(cache.lookup(key(), &[1, 2, 3, 4, 5]), None);
    }

    #[test]
    fn all_4096_bytes_participate_in_equality() {
        let mut cache = PassCache::new();
        let k = CacheKey { bucket: 12, ..key() };
        let bytes = [0x6D; MAX_PAYLOAD];
        assert!(cache.insert(k, &bytes, 202));
        for i in 0..MAX_PAYLOAD {
            let mut changed = bytes; changed[i] ^= 0x80;
            assert_eq!(cache.lookup(k, &changed), None, "byte {i}");
        }
        assert_eq!(cache.lookup(k, &bytes), Some(202));
    }

    #[test]
    fn collision_evicts_but_never_false_hits() {
        let mut cache = PassCache::new();
        let a = key();
        let b = (a.descriptor + 1..a.descriptor + 4096)
            .map(|descriptor| CacheKey { descriptor, ..a })
            .find(|k| slot_index(*k) == slot_index(a)).expect("32-slot collision");
        let payload = [1, 2, 3, 4];
        assert!(cache.insert(a, &payload, 101));
        assert_eq!(cache.lookup(b, &payload), None);
        assert!(cache.insert(b, &payload, 202));
        assert_eq!(cache.lookup(a, &payload), None);
        assert_eq!(cache.lookup(b, &payload), Some(202));
    }

    #[test]
    fn owner_device_context_descriptor_bucket_generation_are_separate() {
        let mut cache = PassCache::new();
        let a = key(); let payload = [1, 2, 3, 4];
        assert!(cache.insert(a, &payload, 101));
        for other in [CacheKey { owner: 77, ..a }, CacheKey { device: 77, ..a },
                      CacheKey { context: 77, ..a }, CacheKey { descriptor: 77, ..a },
                      CacheKey { bucket: 3, ..a }, CacheKey { generation: 77, ..a }] {
            assert_eq!(cache.lookup(other, &payload), None);
        }
    }

    #[test]
    fn clear_invalidates_previous_pass_without_zeroing_payload() {
        let mut cache = PassCache::new(); let payload = [1, 2, 3, 4];
        assert!(cache.insert(key(), &payload, 101));
        let slot = slot_index(key()); let epoch = cache.epoch;
        cache.clear();
        assert_ne!(cache.epoch, epoch);
        assert_eq!(cache.lookup(key(), &payload), None);
        assert_eq!(&cache.slots[slot].bytes[..4], &payload);
        assert!(cache.insert(key(), &payload, 202));
        assert_eq!(cache.lookup(key(), &payload), Some(202));
        cache.clear(); assert_eq!(cache.lookup(key(), &payload), None);
    }

    #[test]
    fn epoch_wrap_cannot_revive_stale_slots() {
        let mut cache = PassCache::new(); let payload = [1, 2, 3, 4];
        assert!(cache.insert(key(), &payload, 101)); // entry epoch 1
        cache.epoch = u64::MAX;
        cache.clear(); // becomes 1, but old metadata must be invalid
        assert_eq!(cache.epoch, 1);
        assert_eq!(cache.lookup(key(), &payload), None);
        assert!(cache.slots.iter().all(|s| s.epoch == 0));
    }

    #[test]
    fn invalid_inserts_never_replace_a_valid_entry() {
        let mut cache = PassCache::new(); let payload = [1, 2, 3, 4];
        assert!(cache.insert(key(), &payload, 101));
        assert!(!cache.insert(key(), &payload, 0));
        assert!(!cache.insert(key(), &[1], 202));
        assert!(!cache.insert(CacheKey { bucket: 13, ..key() }, &payload, 202));
        assert_eq!(cache.lookup(key(), &payload), Some(101));
    }

    #[test]
    fn replacement_for_same_key_uses_new_exact_payload_and_resource() {
        let mut cache = PassCache::new();
        assert!(cache.insert(key(), &[1, 2, 3, 4], 101));
        assert!(cache.insert(key(), &[4, 3, 2, 1], 202));
        assert_eq!(cache.lookup(key(), &[1, 2, 3, 4]), None);
        assert_eq!(cache.lookup(key(), &[4, 3, 2, 1]), Some(202));
    }

    #[test]
    fn capacity_is_fixed_after_arbitrarily_many_inserts() {
        let mut cache = PassCache::new();
        let initial_size = core::mem::size_of_val(&cache);
        for descriptor in 1..10_001usize {
            let k = CacheKey { descriptor, ..key() };
            assert!(cache.insert(k, &[1, 2, 3, 4], descriptor));
            assert_eq!(cache.lookup(k, &[1, 2, 3, 4]), Some(descriptor));
        }
        assert_eq!(cache.slots.len(), CACHE_SLOTS);
        assert_eq!(core::mem::size_of_val(&cache), initial_size);
        assert!(initial_size < CACHE_SLOTS * (MAX_PAYLOAD + 128));
        assert!(cache.slots.iter().filter(|s| s.epoch == cache.epoch).count() <= CACHE_SLOTS);
    }
}
