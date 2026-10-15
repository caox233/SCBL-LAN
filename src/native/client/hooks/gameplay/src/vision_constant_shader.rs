//! Native PS creation-time read proof. No bytecode/disassembler work in draw hooks.
//! b0 admits only the independently proved leading [4,16) gap; b4/b5 remain prefixes.
//! Applied only under the parent's inspected-addon environment gate and PS-only
//! native binding contract. Existing/unknown shaders are never inferred from Size.
//! Exact addon 1f3a1aab..4c66 has only four replacement sources; all read neither
//! b0, b4 nor b5. Original read proofs therefore bound replacement reads too. This is
//! the inspected source-writer closure in ADDON-SOURCE-BOUNDARY.md (2026-10-11),
//! NOT a consequence of merely seeing an addon SHA or an absent Map callback.
//! Any changed addon/environment must fall back until its consumers are reviewed.
#[path = "constant_shader_consumer.rs"]
mod consumer;
use retour::GenericDetour;
use std::{cell::Cell, collections::HashMap, sync::{Mutex, OnceLock}};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU32, Ordering};
const CTOR_VA: usize = 0x1677ad0;
const DTOR_VA: usize = 0x1677a50;
const DELETE_VA: usize = 0x1677c00;
const PS_VTABLE: usize = 0x2c02eb0;
const MAX_SHADERS: usize = 16384;
type CtorFn = unsafe extern "thiscall" fn(usize, usize, usize, usize, usize) -> usize;
type DtorFn = unsafe extern "thiscall" fn(usize);
type DeleteFn = unsafe extern "thiscall" fn(usize, u32) -> usize;
static CTOR: OnceLock<GenericDetour<CtorFn>> = OnceLock::new();
static DTOR: OnceLock<GenericDetour<DtorFn>> = OnceLock::new();
static DELETE: OnceLock<GenericDetour<DeleteFn>> = OnceLock::new();
static CAPTURE: AtomicBool = AtomicBool::new(false);
static EPOCH: AtomicU64 = AtomicU64::new(1);
static NEXT: AtomicU64 = AtomicU64::new(1);
static CREATED: AtomicU32 = AtomicU32::new(0);
static PROVED: AtomicU32 = AtomicU32::new(0);
#[derive(Clone, Copy)]
struct Record { serial: u64, shader: usize, ends: [usize; 2], b0_gap_end: Option<usize> }
static REGISTRY: OnceLock<Mutex<HashMap<usize, Record>>> = OnceLock::new();
#[derive(Clone, Copy)]
struct Cached { epoch: u64, wrapper: usize, shader: usize, record: Option<Record> }
thread_local! { static LAST: Cell<Option<Cached>> = const { Cell::new(None) }; }
fn registry() -> &'static Mutex<HashMap<usize, Record>> { REGISTRY.get_or_init(|| Mutex::new(HashMap::new())) }
pub(super) fn epoch() -> u64 { EPOCH.load(Ordering::Acquire) }
fn invalidate_epoch() { if EPOCH.fetch_add(1, Ordering::AcqRel) == u64::MAX { CAPTURE.store(false, Ordering::Release); } }
fn remove(wrapper: usize) {
    if let Ok(mut map) = registry().lock() { map.remove(&wrapper); }
    invalidate_epoch();
}
fn begin(wrapper: usize) -> Option<u64> {
    remove(wrapper);
    if !CAPTURE.load(Ordering::Acquire) { return None; }
    let serial = NEXT.fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1)).ok()?;
    let mut map = registry().lock().ok()?;
    if map.len() >= MAX_SHADERS { return None; }
    map.insert(wrapper, Record { serial, shader: 0, ends: [usize::MAX; 2], b0_gap_end: None });
    Some(serial)
}
unsafe fn word(p: usize, offset: usize) -> usize { ((p + offset) as *const u32).read_unaligned() as usize }

#[link(name="kernel32")]
extern "system" {
    fn GetSystemDirectoryW(path: *mut u16, capacity: u32) -> u32;
    fn GetSystemWow64DirectoryW(path: *mut u16, capacity: u32) -> u32;
    fn LoadLibraryExW(name: *const u16, file: isize, flags: u32) -> isize;
    fn GetModuleFileNameW(module: isize, path: *mut u16, capacity: u32) -> u32;
    fn GetProcAddress(module: isize, name: *const u8) -> usize;
}
type Disassemble = unsafe extern "system" fn(*const u8, usize, u32, *const u8, *mut usize) -> i32;
// Keep one system-module reference for process lifetime; no unload races with
// concurrent constructors. A local/game-directory compiler is never requested.
fn disassembler() -> Option<Disassemble> {
    static API: OnceLock<Option<Disassemble>> = OnceLock::new();
    *API.get_or_init(|| unsafe {
        let mut buf = [0u16; 1024];
        let mut n = GetSystemWow64DirectoryW(buf.as_mut_ptr(), buf.len() as u32) as usize;
        if n == 0 { n = GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) as usize; }
        if n == 0 || n >= buf.len() { return None; }
        let mut path = buf[..n].to_vec(); path.extend("\\d3dcompiler_47.dll".encode_utf16()); path.push(0);
        let module = LoadLibraryExW(path.as_ptr(), 0, 0x800); if module == 0 { return None; }
        // WOW64 may report the redirected system DLL under System32 even when
        // loaded by the explicit SysWOW64 path. Accept only those two OS paths.
        let mut system = [0u16; 1024];
        let sys_len = GetSystemDirectoryW(system.as_mut_ptr(), system.len() as u32) as usize;
        if sys_len == 0 || sys_len >= system.len() { return None; }
        let system_path = String::from_utf16_lossy(&system[..sys_len]) + "\\d3dcompiler_47.dll";
        let n = GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) as usize;
        if n == 0 || n >= buf.len() { return None; }
        let actual = String::from_utf16_lossy(&buf[..n]);
        if !actual.eq_ignore_ascii_case(&String::from_utf16_lossy(&path[..path.len()-1]))
            && !actual.eq_ignore_ascii_case(&system_path) { return None; }
        let p = GetProcAddress(module, b"D3DDisassemble\0".as_ptr());
        (p != 0).then(|| std::mem::transmute::<usize, Disassemble>(p))
    })
}
struct Blob(usize);
impl Drop for Blob { fn drop(&mut self) { unsafe {
    let table = *(self.0 as *const *const usize);
    let release: unsafe extern "system" fn(usize) -> u32 = std::mem::transmute(*table.add(2));
    release(self.0);
}}}
fn analyze(bytes: &[u8]) -> Option<([usize; 2], Option<usize>)> { unsafe {
    let api = disassembler()?; let mut out = 0usize;
    let hr = api(bytes.as_ptr(), bytes.len(), 0, std::ptr::null(), &mut out);
    if out == 0 { return None; }
    let blob = Blob(out); if hr < 0 { return None; }
    let table = *(blob.0 as *const *const usize);
    let data: unsafe extern "system" fn(usize) -> *const u8 = std::mem::transmute(*table.add(3));
    let size: unsafe extern "system" fn(usize) -> usize = std::mem::transmute(*table.add(4));
    let n = size(blob.0); let p = data(blob.0);
    if n == 0 || n > consumer::MAX_TEXT_BYTES + 1 || p.is_null() { return None; }
    let text = std::slice::from_raw_parts(p, n);
    let text = text.strip_suffix(&[0]).unwrap_or(text);
    let text = std::str::from_utf8(text).ok()?;
    Some((consumer::read_ends(text)?, consumer::read_b0_gap_end(text)))
}}
unsafe fn snapshot(p: usize, n: usize) -> Option<Vec<u32>> {
    if !(32..=512*1024).contains(&n) || n % 4 != 0 || !super::super::readable(p, n) { return None; }
    if word(p, 0) != u32::from_le_bytes(*b"DXBC") as usize || word(p, 24) != n { return None; }
    let mut copy = vec![0u32; n / 4];
    std::ptr::copy_nonoverlapping(p as *const u8, copy.as_mut_ptr() as *mut u8, n); Some(copy)
}
unsafe extern "thiscall" fn construct(wrapper: usize, device: usize, bytes: usize, n: usize, meta: usize) -> usize {
    let serial = begin(wrapper);
    let copy = if serial.is_some() { snapshot(bytes, n) } else { None };
    let input = copy.as_ref().map_or(bytes, |b| b.as_ptr() as usize);
    // Reflection AND CreatePixelShader consume the same immutable snapshot that
    // is analyzed below; no caller pointer is retained or asynchronously read.
    let result = CTOR.get().expect("published").call(wrapper, device, input, n, meta);
    CREATED.fetch_add(1, Ordering::Relaxed);
    if result != wrapper || !CAPTURE.load(Ordering::Acquire) { return result; }
    if let (Some(serial), Some(copy)) = (serial, copy) {
        if word(wrapper, 0) != PS_VTABLE || word(wrapper, 8) == 0 { return result; }
        let bytes = std::slice::from_raw_parts(copy.as_ptr() as *const u8, n);
        if let Some((ends, b0_gap_end)) = analyze(bytes) {
            if let Ok(mut map) = registry().lock() {
                if CAPTURE.load(Ordering::Acquire) {
                    if let Some(record) = map.get_mut(&wrapper).filter(|p| p.serial == serial) {
                        record.shader = word(wrapper, 8); record.ends = ends; record.b0_gap_end = b0_gap_end;
                        PROVED.fetch_add(1, Ordering::Relaxed);
                        invalidate_epoch();
                    }
                }
            }
        }
    }
    result
}
unsafe extern "thiscall" fn destroy(wrapper: usize) { remove(wrapper); DTOR.get().expect("published").call(wrapper); }
unsafe extern "thiscall" fn delete(wrapper: usize, flags: u32) -> usize { remove(wrapper); DELETE.get().expect("published").call(wrapper, flags) }

#[derive(Clone, Copy)]
pub(super) struct Prefix { pub serial: u64, pub epoch: u64 }
/// Returns a proof ONLY for the owning PS list, PS-only native bind flags, and
/// a fully initialized prefix encompassing every conservative b4/b5 read.
pub(super) unsafe fn prefix(owner: usize, list: usize, flags: usize, slot: usize, len: usize) -> Option<Prefix> {
    if !(4..=5).contains(&slot) { return None; }
    consumer_proof(owner, list, flags, slot, len)
}
// Exact sparse layout is established by the caller: [0,4) plus [16,len).
// This gate never authorizes reading or writing GPU padding bytes.
pub(super) unsafe fn leading_gap(owner: usize, list: usize, flags: usize, slot: usize, len: usize) -> Option<Prefix> {
    if slot != 0 || len < 32 { return None; }
    consumer_proof(owner, list, flags, slot, len)
}
unsafe fn consumer_proof(owner: usize, list: usize, flags: usize, slot: usize, len: usize) -> Option<Prefix> {
    if !CAPTURE.load(Ordering::Acquire) || flags != 2 || len == 0 || len % 16 != 0 { return None; }
    let wrapper = word(owner, 0x6c);
    if wrapper == 0 || wrapper.checked_add(0x3c)? != list || word(wrapper, 0) != PS_VTABLE || word(wrapper, 0x890) != 2 { return None; }
    let shader = word(wrapper, 8); if shader == 0 { return None; }
    let version = epoch();
    let record = LAST.with(|cache| {
        if let Some(c) = cache.get().filter(|c| c.epoch == version && c.wrapper == wrapper && c.shader == shader) { return c.record; }
        let record = registry().lock().ok()?.get(&wrapper).copied().filter(|p| p.shader == shader);
        cache.set(Some(Cached { epoch: version, wrapper, shader, record })); record
    })?;
    let end = if slot == 0 { record.b0_gap_end.filter(|&end| end != 0)? } else { record.ends[slot - 4] };
    if end > len || epoch() != version { return None; }
    Some(Prefix { serial: record.serial, epoch: version })
}
pub(super) fn summary() -> (u32, u32) { (CREATED.load(Ordering::Relaxed), PROVED.load(Ordering::Relaxed)) }
pub(super) fn stop() { CAPTURE.store(false, Ordering::Release); if let Ok(mut m) = registry().lock() { m.clear(); } invalidate_epoch(); }
pub(super) unsafe fn init() -> bool {
    if !CONTRACTS.iter().all(|&(a, n, h)| super::super::signature(a, n, h)) { return false; }
    CAPTURE.store(true, Ordering::Release);
    let ok = super::super::install_direct(&DTOR, std::mem::transmute::<usize, DtorFn>(DTOR_VA), destroy as DtorFn)
        && super::super::install_direct(&DELETE, std::mem::transmute::<usize, DeleteFn>(DELETE_VA), delete as DeleteFn)
        && super::super::install_direct(&CTOR, std::mem::transmute::<usize, CtorFn>(CTOR_VA), construct as CtorFn);
    if !ok { deinit(); } ok
}
pub(super) unsafe fn deinit() {
    stop();
    if let Some(h) = CTOR.get() { let _ = h.disable(); }
    if let Some(h) = DELETE.get() { let _ = h.disable(); }
    if let Some(h) = DTOR.get() { let _ = h.disable(); }
}
// Exact approved executable; all native binding/descriptor/lifetime helpers.
// Generated from native-ps-contract.json and checked before any hook is installed.
const CONTRACTS: &[(usize, usize, &str)] = &[
    (0x1677ad0, 289, "2e17060976cdaa52144cf3b2fb1acb290191967e7043439225d5c358da56b373"),
    (0x1677a50, 126, "422f675514273bdf9e5c481470a5bf6a5248d9462e99ed89b5cc2de612fef9d3"),
    (0x1677c00, 145, "9f01cde9545d1760d63d4964e1f38160458a2db8dd707155a68329598051795d"),
    (0x166f2e0, 11, "5ee02915b5bcc3a2d261f6cf72ea6f459bcbbd475b7d495789503e38948c1205"),
    (0x166f2f0, 39, "14265515e95cb0723f96a2833f79f167b7e95541cb981ad88b8cdb5098f881a1"),
    (0x16779d0, 26, "27aa83a5d9026d036e792cb563db693ae877f7f1fed4b7edb2d47f68afbf8eb0"),
    (0x16744a0, 190, "50e1079d44a1831e99e6a14d68895202815fe3b4ac28bf57e8be7188c5234d9c"),
    (0x1676e50, 184, "0bdd72c8338cbd03dacf1e7f6b19ba6166df79c7a610d5157405b41b052f2690"),
    (0x1604cf0, 160, "42eb8245d0124a7d07af1ed4aa3c752c44675bad3ad608e29b285255dde06d42"),
    (0x1605d10, 126, "2c434d5d1a84201e9ee45ded09364c141fee3eb1e33f16ae26615a96d3b1c9af"),
    (0x1603910, 100, "025ae03a35360d2da570a9bf9d0abc02b97e932870c4a47f3b3389211fd08d24"),
    (0x1605f10, 793, "027aa3afd799955151491916ddae0e95d4451786c194f29753a9443a5100671f"),
    (0x1606290, 100, "24a4eaba97d033ec72158e5c57ef6774b96e8113bb1b2204a5052e5e7deff21e"),
    (0x1602ee0, 135, "c43f304eddd80c6de204b3be4c08da0e5846dd33ebe23b6b33ac0e1245254b16"),
];

