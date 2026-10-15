//! Four gameplay code sites, split into independently owned Intel/Overcharge groups.
//! Synchronous startup/ability-window/shutdown contract; not a hot-patch API.
use std::ffi::c_void;

#[derive(Clone, Copy)]
pub(super) struct Patch {
    pub address: usize,
    pub original: &'static [u8],
    pub modified: &'static [u8],
}

pub(super) struct PatchGroup {
    pending: u8,
    active: bool,
}
impl PatchGroup {
    pub(super) const fn new() -> Self { Self { pending: 0, active: false } }

    pub(super) unsafe fn set(&mut self, patches: &[Patch], enabled: bool) -> bool {
        let mut matches = |address, bytes: &[u8]| matches_bytes(address, bytes);
        let mut write = |address, expected: &[u8], next: &[u8]| write_code(address, expected, next);
        if enabled { self.install_with(patches, &mut matches, &mut write) }
        else { self.restore_with(patches, &mut matches, &mut write) }
    }

    fn install_with(
        &mut self, patches: &[Patch],
        matches: &mut impl FnMut(usize, &[u8]) -> bool,
        write: &mut impl FnMut(usize, &[u8], &[u8]) -> bool,
    ) -> bool {
        if self.active { return true; }
        // A previous failed end may retry before a new window, but cannot be
        // adopted as an already-installed group or discarded on cleanup failure.
        if !self.restore_with(patches, matches, write) { return false; }
        if patches.is_empty() || patches.len() > 8
            || patches.iter().any(|p| p.original.is_empty()
                || p.original.len() != p.modified.len() || p.original == p.modified
                || !matches(p.address, p.original))
        { return false; }
        for (index, p) in patches.iter().enumerate() {
            self.pending |= 1 << index; // Before the potentially partial write.
            if !write(p.address, p.original, p.modified) {
                self.restore_with(patches, matches, write);
                return false;
            }
        }
        self.active = true;
        true
    }

    fn restore_with(
        &mut self, patches: &[Patch],
        matches: &mut impl FnMut(usize, &[u8]) -> bool,
        write: &mut impl FnMut(usize, &[u8], &[u8]) -> bool,
    ) -> bool {
        self.active = false;
        if patches.len() > 8 { return false; }
        for (index, p) in patches.iter().enumerate().rev() {
            if self.pending & (1 << index) == 0 { continue; }
            let expected = if matches(p.address, p.original) { p.original }
                else if matches(p.address, p.modified) { p.modified }
                else { continue; }; // Foreign, partial or unreadable: keep the record.
            if write(p.address, expected, p.original) { self.pending &= !(1 << index); }
        }
        self.pending == 0
    }
}

unsafe fn matches_bytes(address: usize, expected: &[u8]) -> bool {
    !expected.is_empty() && crate::equipment_native::readable(address, expected.len())
        && std::slice::from_raw_parts(address as *const u8, expected.len()) == expected
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(process: *mut c_void, address: *const c_void, size: usize) -> i32;
}
unsafe fn write_code(address: usize, expected: &[u8], next: &[u8]) -> bool {
    if expected.len() != next.len() || !matches_bytes(address, expected) { return false; }
    let pointer = address as *mut u8;
    if expected != next {
        let Ok(guard) = region::protect_with_handle(pointer, next.len(), region::Protection::READ_WRITE_EXECUTE)
        else { return false; };
        if !matches_bytes(address, expected) { return false; }
        std::ptr::copy_nonoverlapping(next.as_ptr(), pointer, next.len());
        drop(guard);
    }
    // A byte-equal retry still needs to finish a previously failed cache sync.
    FlushInstructionCache(GetCurrentProcess(), pointer.cast(), next.len()) != 0
        && matches_bytes(address, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    const PLAN: [Patch; 3] = [
        Patch { address: 0, original: &[1], modified: &[11] },
        Patch { address: 1, original: &[2], modified: &[12] },
        Patch { address: 2, original: &[3], modified: &[13] },
    ];
    struct Memory { bytes: [u8; 3], fail: u8, writes: Vec<(usize,u8)> }
    fn operate(group: &mut PatchGroup, memory: &RefCell<Memory>, enabled: bool) -> bool {
        let mut matches = |a: usize, b: &[u8]| memory.borrow().bytes[a] == b[0];
        let mut write = |a: usize, e: &[u8], b: &[u8]| {
            let mut m = memory.borrow_mut(); m.writes.push((a,b[0]));
            if m.fail & (1 << a) != 0 || m.bytes[a] != e[0] { return false; }
            m.bytes[a] = b[0]; true
        };
        if enabled { group.install_with(&PLAN, &mut matches, &mut write) }
        else { group.restore_with(&PLAN, &mut matches, &mut write) }
    }
    fn memory() -> RefCell<Memory> { RefCell::new(Memory { bytes: [1,2,3], fail: 0, writes: vec![] }) }
    #[test]
    fn failed_end_retains_only_unrestored_sites_and_retries_in_reverse() {
        let mut g = PatchGroup::new(); let m = memory(); assert!(operate(&mut g,&m,true));
        m.borrow_mut().fail=2; m.borrow_mut().writes.clear();
        assert!(!operate(&mut g,&m,false)); assert_eq!(g.pending,2); assert!(!g.active);
        assert_eq!(m.borrow().bytes,[1,12,3]); assert_eq!(m.borrow().writes,[(2,3),(1,2),(0,1)]);
        m.borrow_mut().fail=0; assert!(operate(&mut g,&m,false)); assert_eq!(g.pending,0);
    }
    #[test]
    fn foreign_already_modified_site_is_not_adopted_or_restored() {
        let mut g=PatchGroup::new();let m=memory();m.borrow_mut().bytes[1]=12;
        assert!(!operate(&mut g,&m,true));assert!(operate(&mut g,&m,false));
        assert_eq!(m.borrow().bytes,[1,12,3]);assert!(m.borrow().writes.is_empty());
    }
    #[test]
    fn failed_pending_restore_blocks_install_then_allows_clean_retry() {
        let mut g=PatchGroup::new();let m=memory();assert!(operate(&mut g,&m,true));
        m.borrow_mut().fail=6;assert!(!operate(&mut g,&m,false));assert!(!operate(&mut g,&m,true));
        assert!(!g.active);m.borrow_mut().fail=0;assert!(operate(&mut g,&m,true));assert_eq!(g.pending,7);
    }
    #[test]
    fn original_bytes_do_not_discard_pending_cache_sync() {
        let mut g=PatchGroup { pending: 1, active: false };let m=memory();m.borrow_mut().fail=1;
        assert!(!operate(&mut g,&m,false));assert_eq!(g.pending,1);
        m.borrow_mut().fail=0;assert!(operate(&mut g,&m,false));assert_eq!(m.borrow().writes,[(0,1),(0,1)]);
    }
    #[test]
    fn unknown_bytes_are_retained_without_blocking_other_restores() {
        let mut g=PatchGroup::new();let m=memory();assert!(operate(&mut g,&m,true));m.borrow_mut().bytes[1]=99;
        assert!(!operate(&mut g,&m,false));assert_eq!(m.borrow().bytes,[1,99,3]);assert_eq!(g.pending,2);
        m.borrow_mut().bytes[1]=12;assert!(operate(&mut g,&m,false));
    }
    #[test]
    fn malformed_plan_never_writes_or_publishes_active() {
        let mut g=PatchGroup::new();let mut writes=0;
        let p=[Patch { address: 0, original: &[1], modified: &[] }];
        assert!(!g.install_with(&p,&mut |_,_|true,&mut |_,_,_|{writes+=1;true}));
        assert_eq!(writes,0);assert!(!g.active);assert_eq!(g.pending,0);
    }
}
