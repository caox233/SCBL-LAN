//! Bounded Win32 reads and exact-byte patch publication for the core input owner.
use sha2::{Digest, Sha256};
use std::ffi::c_void;
#[repr(C)]
#[derive(Default)]
struct MemoryInfo {
    base: usize,
    allocation: usize,
    allocation_protect: u32,
    size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}
#[link(name = "kernel32")]
extern "system" {
    fn VirtualQuery(p: *const c_void, m: *mut MemoryInfo, n: usize) -> usize;
    fn VirtualProtect(p: *mut c_void, n: usize, protect: u32, old: *mut u32) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(p: *mut c_void, address: *const c_void, n: usize) -> i32;
}
pub(super) unsafe fn readable(p: usize, n: usize) -> bool {
    if p < 0x10000 || n == 0 {
        return false;
    }
    let Some(end) = p.checked_add(n) else {
        return false;
    };
    let mut m = MemoryInfo::default();
    VirtualQuery(p as _, &mut m, std::mem::size_of::<MemoryInfo>()) != 0
        && m.state == 0x1000
        && m.protect & 0x101 == 0
        && m.protect & 0xee != 0
        && m.base.checked_add(m.size).is_some_and(|limit| end <= limit)
}
pub(super) unsafe fn word(p: usize, o: usize) -> Option<u32> {
    let a = p.checked_add(o)?;
    readable(a, 4).then(|| (a as *const u32).read_unaligned())
}
pub(super) unsafe fn float(p: usize, o: usize) -> Option<f32> {
    word(p, o).map(f32::from_bits)
}
pub(crate) unsafe fn patch(address: usize, expected: &[u8], replacement: &[u8]) -> bool {
    if expected.len() != replacement.len()
        || !readable(address, expected.len())
        || std::slice::from_raw_parts(address as *const u8, expected.len()) != expected
    {
        return false;
    }
    let mut old = 0;
    if VirtualProtect(address as _, replacement.len(), 0x40, &mut old) == 0 {
        return false;
    }
    // Opening write access is a failure boundary, not ownership of these bytes.
    // Revalidate before publication; another owner must never be overwritten.
    if !readable(address, expected.len())
        || std::slice::from_raw_parts(address as *const u8, expected.len()) != expected
    {
        let mut discard = 0;
        let _ = VirtualProtect(address as _, replacement.len(), old, &mut discard);
        return false;
    }
    std::ptr::copy_nonoverlapping(replacement.as_ptr(), address as _, replacement.len());
    let mut discard = 0;
    let restored = VirtualProtect(address as _, replacement.len(), old, &mut discard) != 0;
    let flushed = FlushInstructionCache(GetCurrentProcess(), address as _, replacement.len()) != 0;
    if restored && flushed {
        return true;
    }
    // Undo bytes if publishing the patch could not complete. Preserve the mapping
    // even on failure; an in-flight branch must never point to freed storage.
    let mut temp = 0;
    if readable(address, replacement.len())
        && std::slice::from_raw_parts(address as *const u8, replacement.len()) == replacement
        && VirtualProtect(address as _, replacement.len(), 0x40, &mut temp) != 0
    {
        // Failure callbacks may have changed ownership since publication.
        if readable(address, replacement.len())
            && std::slice::from_raw_parts(address as *const u8, replacement.len()) == replacement
        {
            std::ptr::copy_nonoverlapping(expected.as_ptr(), address as _, expected.len());
        }
        let _ = VirtualProtect(address as _, expected.len(), old, &mut temp);
        let _ = FlushInstructionCache(GetCurrentProcess(), address as _, expected.len());
    }
    false
}
pub(super) unsafe fn signature(address: usize, size: usize, expected: &str) -> bool {
    readable(address, size)
        && format!(
            "{:x}",
            Sha256::digest(std::slice::from_raw_parts(address as *const u8, size))
        ) == expected
}
