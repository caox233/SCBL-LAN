//! Permanent DX11 adapters with builtin shipping policies. Only an explicit
//! development build installs the legacy range override; native range is unchanged.
use crate::feature_contract::*;
use hooks_plugin_api::hud::{HudFrame, HudRect};
use retour::static_detour;
use sha2::{Digest, Sha256};
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        Mutex,
    },
};
const DEVELOPMENT_MELEE_RANGE: bool = cfg!(feature = "dev-hot-reload");
const MELEE: usize = 0x014f5b00;
const RFD_SITE: usize = 0x01a73ce1;
const RFD_ORIGINAL: [u8; 6] = [0x0f, 0xb6, 0x45, 0xd7, 0x85, 0xc0];
const ARMOR_VT: u32 = 0x02ad42ac;
const SPY: u32 = 1 << 18;
const MERC: u32 = 1 << 19;
static READY: AtomicBool = AtomicBool::new(false);
static RFD_THUNK: AtomicUsize = AtomicUsize::new(0);
static MELEE_LOG: AtomicU32 = AtomicU32::new(0);
static STAMINA_LOG: AtomicU32 = AtomicU32::new(0);
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
    fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, protect: u32) -> *mut c_void;
    fn VirtualProtect(p: *mut c_void, n: usize, protect: u32, old: *mut u32) -> i32;
    fn VirtualFree(p: *mut c_void, n: usize, t: u32) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(p: *mut c_void, address: *const c_void, n: usize) -> i32;
    fn GetTickCount64() -> u64;
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
pub(super) unsafe fn writable(p: usize, n: usize) -> bool {
    if !readable(p, n) {
        return false;
    }
    let mut m = MemoryInfo::default();
    VirtualQuery(p as _, &mut m, std::mem::size_of::<MemoryInfo>()) != 0 && m.protect & 0xcc != 0
}
pub(super) unsafe fn word(p: usize, o: usize) -> Option<u32> {
    let a = p.checked_add(o)?;
    readable(a, 4).then(|| (a as *const u32).read_unaligned())
}
pub(super) unsafe fn float(p: usize, o: usize) -> Option<f32> {
    word(p, o).map(f32::from_bits)
}
pub(super) unsafe fn armor(p: usize) -> Option<usize> {
    let a = word(p, 0xcf4)? as usize;
    (word(a, 0)? == ARMOR_VT).then_some(a)
}
pub(super) unsafe fn part_name(part: usize, name: &[u8]) -> bool {
    let (Some(p), Some(n), Some(cap)) = (word(part, 0x28), word(part, 0x2c), word(part, 0x30))
    else {
        return false;
    };
    let n = n as usize;
    if !(name.len()..=name.len() + 1).contains(&n) || (cap as usize) < n || cap > 128 {
        return false;
    }
    let p = p as usize;
    if readable(p, n) {
        let s = std::slice::from_raw_parts(p as *const u8, n);
        if s == name || s.strip_suffix(&[0]) == Some(name) {
            return true;
        }
    }
    if !readable(p, n * 2) {
        return false;
    }
    std::slice::from_raw_parts(p as *const u8, n * 2)
        .chunks_exact(2)
        .enumerate()
        .all(|(i, x)| x[1] == 0 && x[0] == name.get(i).copied().unwrap_or(0))
}
pub(super) unsafe fn equipped(a: usize, name: &[u8]) -> bool {
    let (Some(p), Some(n), Some(cap)) = (word(a, 0x50), word(a, 0x54), word(a, 0x58)) else {
        return false;
    };
    if n == 0 || n > 12 || cap < n || cap > 24 || !readable(p as usize, n as usize * 4) {
        return false;
    }
    (0..n as usize)
        .any(|i| word(p as usize, i * 4).is_some_and(|part| part_name(part as usize, name)))
}
static_detour! {static MeleeRange:unsafe extern "thiscall" fn(*mut u8,*mut u8,u8)->f32;}
fn melee(this: *mut u8, target: *mut u8, context: u8) -> f32 {
    let original = unsafe { MeleeRange.call(this, target, context) };
    unsafe { adjust_melee(this as usize, original) }
}
unsafe fn adjust_melee(this: usize, original: f32) -> f32 {
    let Some(pawn) = word(this, 0x48).map(|v| v as usize) else {
        return original;
    };
    if word(pawn, 0x374).map(|f| f & (SPY | MERC)) != Some(SPY) {
        return original;
    }
    let Some(a) = armor(pawn) else {
        return original;
    };
    if !equipped(a, b"MPArmorGlovesGhost") {
        return original;
    }
    let Some(multiplier) = float(a, 0x70).filter(|f| f.is_finite() && *f > 1.0) else {
        return original;
    };
    if !original.is_finite() || original <= 0.0 {
        return original;
    }
    let mut p = FeaturePacket::new(EVENT_MELEE_RANGE);
    p.words[..3].copy_from_slice(&[original.to_bits(), multiplier.to_bits(), 1]);
    let adjusted = crate::live_rules::feature(p).map_or(original, |v| f32::from_bits(v.words[0]));
    if MELEE_LOG.fetch_add(1, Ordering::Relaxed) < 8 {
        tracing::info!(
            "SCBL_MOD_MELEE native={} multiplier={} base={} adjusted={} context=preserved",
            original,
            multiplier,
            original / multiplier,
            adjusted
        );
    }
    adjusted
}
#[cfg(test)]
fn ability_kind(vtable: u32) -> u32 {
    match vtable {
        0x02a3eef4 => 1,
        0x02a3f2dc => 2,
        0x02a3f3a4 => 3,
        _ => 0,
    }
}
// Native detection remains intact; the child adapter classifies display sources.
pub(crate) fn ready() -> bool { READY.load(Ordering::Acquire) }
unsafe extern "system" fn rfd_filter(frame: usize) {
    if READY.load(Ordering::Acquire) && readable(frame.saturating_sub(0x208), 0x208) {
        crate::rfd_presentation::visibility(frame);
    }
}
// x86 mid-function bridge: save EFLAGS/GPR + aligned FXSAVE (x87/SSE);
// borrow the native frame synchronously; restore machine state; reproduce
// exactly the six displaced instructions and resume the original JE.
fn bridge_bytes(base: u32, callback: u32, resume: u32) -> Vec<u8> {
    let mut b = vec![
        0x9c, 0x60, 0x89, 0xe0, 0x81, 0xec, 0x20, 0x02, 0, 0, 0x83, 0xe4, 0xf0, 0x89, 0x84, 0x24,
        0, 0x02, 0, 0, 0x0f, 0xae, 0x04, 0x24, 0xfc, 0x55, 0xb8,
    ];
    b.extend(callback.to_le_bytes());
    b.extend([
        0xff, 0xd0, 0x0f, 0xae, 0x0c, 0x24, 0x8b, 0xa4, 0x24, 0, 0x02, 0, 0, 0x61, 0x9d,
    ]);
    b.extend(RFD_ORIGINAL);
    let end = base.wrapping_add(b.len() as u32 + 5);
    b.push(0xe9);
    b.extend(resume.wrapping_sub(end).to_le_bytes());
    b
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
    if VirtualProtect(address as _, replacement.len(), 0x40, &mut temp) != 0 {
        std::ptr::copy_nonoverlapping(expected.as_ptr(), address as _, expected.len());
        let _ = VirtualProtect(address as _, expected.len(), old, &mut temp);
        let _ = FlushInstructionCache(GetCurrentProcess(), address as _, expected.len());
    }
    false
}
fn jump(from: usize, to: usize) -> [u8; 6] {
    let mut b = [0xe9, 0, 0, 0, 0, 0x90];
    b[1..5].copy_from_slice(&(to as u32).wrapping_sub(from as u32 + 5).to_le_bytes());
    b
}
pub(super) unsafe fn signature(address: usize, size: usize, expected: &str) -> bool {
    readable(address, size)
        && format!(
            "{:x}",
            Sha256::digest(std::slice::from_raw_parts(address as *const u8, size))
        ) == expected
}
pub(crate) fn init(world: usize) {
    if world != 0x00589860 {
        return;
    }
    unsafe {
        if (DEVELOPMENT_MELEE_RANGE && !signature(
            MELEE,
            0x305,
            "07235c0f8df4fda435ec785b51e59d35b036abcd2089f130963fb6983727b61b",
        )) || !signature(
            0x1a73860,
            0x1200,
            "3c7af93c59a49fc895f0efde1135bbd58fca3e9e5ab68d972058221af366d045",
        ) || !signature(
            0xd59b30,
            0x20c,
            "65a99bbd6e629e2da2ecd08384c3b64e35c523ad71cdf10bbf7dac538ce50e97",
        ) {
            tracing::error!("SCBL_MOD_EQUIPMENT enabled=false reason=native-signature");
            return;
        }
        if DEVELOPMENT_MELEE_RANGE && (MeleeRange
            .initialize(
                std::mem::transmute::<
                    usize,
                    unsafe extern "thiscall" fn(*mut u8, *mut u8, u8) -> f32,
                >(MELEE),
                melee,
            )
            .is_err()
            || MeleeRange.enable().is_err())
        {
            return;
        }
        let memory = VirtualAlloc(std::ptr::null_mut(), 128, 0x3000, 0x04);
        if memory.is_null() {
            if DEVELOPMENT_MELEE_RANGE { let _ = MeleeRange.disable(); }
            return;
        }
        let code = bridge_bytes(
            memory as u32,
            rfd_filter as *const () as u32,
            (RFD_SITE + 6) as u32,
        );
        std::ptr::copy_nonoverlapping(code.as_ptr(), memory as _, code.len());
        let mut old = 0;
        if VirtualProtect(memory, 128, 0x20, &mut old) == 0
            || FlushInstructionCache(GetCurrentProcess(), memory, code.len()) == 0
        {
            let _ = VirtualFree(memory, 0, 0x8000); // no native branch references it
            if DEVELOPMENT_MELEE_RANGE { let _ = MeleeRange.disable(); }
            return;
        }
        // Track ownership before patch: a failed publish may still leave a jump.
        RFD_THUNK.store(memory as usize, Ordering::Release);
        if !patch(RFD_SITE, &RFD_ORIGINAL, &jump(RFD_SITE, memory as usize)) {
            if DEVELOPMENT_MELEE_RANGE { let _ = MeleeRange.disable(); }
            return;
        }
        READY.store(true, Ordering::Release);
        tracing::info!("SCBL_MOD_EQUIPMENT adapters=true revision=52 stamina=current1918-max1904 rfd=source-aware-presentation melee=native-range rules=builtin-or-explicit-development");
    }
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let address = RFD_THUNK.load(Ordering::Acquire);
    if address != 0 {
        let _ = patch(RFD_SITE, &jump(RFD_SITE, address), &RFD_ORIGINAL);
    }
    if DEVELOPMENT_MELEE_RANGE { let _ = MeleeRange.disable(); }
    // Thunk mappings remain until process exit; never free an in-flight branch.
    if let Ok(mut s) = HUD.try_lock() {
        s.frame = HudFrame::default();
    }
}
struct Snapshot {
    frame: HudFrame,
    stamp: u64,
    pawn: usize,
    object: u32,
    epoch: u32,
}
static HUD: Mutex<Snapshot> = Mutex::new(Snapshot {
    frame: HudFrame {
        abi: 1,
        size: 176,
        count: 0,
        reserved: 0,
        rects: [HudRect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            rgba: 0,
        }; 8],
    },
    stamp: 0,
    pawn: 0,
    object: 0,
    epoch: 0,
});
pub(crate) fn observe_stamina(hud: usize, dt: f32, foreground: bool) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let Ok(mut s) = HUD.try_lock() else { return };
    unsafe {
        let pawn = word(hud, 0x25c)
            .and_then(|owner| word(owner as usize, 0x310))
            .unwrap_or(0) as usize;
        let object = word(pawn, 0x20).unwrap_or(0);
        if pawn != s.pawn || object != s.object {
            s.pawn = pawn;
            s.object = object;
            s.epoch = s.epoch.wrapping_add(1).max(1);
        }
        let current = float(pawn, 0x1918).unwrap_or(f32::NAN);
        let maximum = float(pawn, 0x1904).unwrap_or(f32::NAN);
        let valid = foreground && word(pawn, 0x374).map(|f| f & (SPY | MERC)) == Some(MERC);
        let mut p = FeaturePacket::new(EVENT_STAMINA);
        p.words[..5].copy_from_slice(&[
            u32::from(valid),
            s.epoch,
            current.to_bits(),
            maximum.to_bits(),
            dt.to_bits(),
        ]);
        s.frame = crate::live_rules::feature(p).map_or_else(HudFrame::default, rectangles);
        s.stamp = GetTickCount64();
        if valid
            && current.is_finite()
            && maximum.is_finite()
            && maximum > 0.0
            && current / maximum <= 0.5
            && STAMINA_LOG.fetch_add(1, Ordering::Relaxed) < 4
        {
            tracing::info!(
                "SCBL_MOD_STAMINA current={} maximum={} drawn={} epoch={}",
                current,
                maximum,
                s.frame.count,
                s.epoch
            );
        }
    }
}
fn rectangles(packet: FeaturePacket) -> HudFrame {
    let mut frame = HudFrame::default();
    let ratio = f32::from_bits(packet.words[0]);
    let alpha = f32::from_bits(packet.words[1]);
    if !alpha.is_finite() || alpha <= 0.0 {
        return frame;
    }
    let x = f32::from_bits(packet.words[2]);
    let y = f32::from_bits(packet.words[3]);
    let w = f32::from_bits(packet.words[4]);
    let h = f32::from_bits(packet.words[5]);
    let a = (alpha * 210.0).round().clamp(0.0, 255.0) as u8;
    frame.count = 2;
    frame.rects[0] = HudRect {
        x: x - w * 0.5,
        y,
        w,
        h,
        rgba: u32::from_le_bytes([12, 28, 30, a]),
    };
    frame.rects[1] = HudRect {
        x: x - w * 0.5,
        y,
        w: w * ratio,
        h,
        rgba: u32::from_le_bytes([145, 223, 211, a]),
    };
    if frame.valid() {
        frame
    } else {
        HudFrame::default()
    }
}
/// # Safety
/// The host must supply writable, properly aligned storage of exactly HudFrame size.
#[no_mangle]
pub unsafe extern "system" fn scbl_mod_hud_snapshot_v1(out: *mut HudFrame, size: u32) -> i32 {
    if out.is_null() || size != std::mem::size_of::<HudFrame>() as u32 {
        return -1;
    }
    let Ok(s) = HUD.try_lock() else { return -2 };
    let frame = if READY.load(Ordering::Acquire) && GetTickCount64().saturating_sub(s.stamp) <= 250
    {
        s.frame
    } else {
        HudFrame::default()
    };
    out.write(frame);
    0
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_preserves_machine_state_and_replays_exact_native_bytes() {
        let b = bridge_bytes(0x1000, 0x2000, 0x3000);
        assert_eq!(&b[..4], &[0x9c, 0x60, 0x89, 0xe0]);
        assert!(b.windows(4).any(|x| x == [0x0f, 0xae, 0x04, 0x24]));
        assert!(b.windows(4).any(|x| x == [0x0f, 0xae, 0x0c, 0x24]));
        assert_eq!(&b[b.len() - 11..b.len() - 5], &RFD_ORIGINAL);
        let rel = u32::from_le_bytes(b[b.len() - 4..].try_into().unwrap());
        assert_eq!(
            0x1000u32.wrapping_add(b.len() as u32).wrapping_add(rel),
            0x3000
        );
    }
    #[test]
    fn known_ability_classes_only() {
        assert_eq!(ability_kind(0x2a3eef4), 1);
        assert_eq!(ability_kind(0x2a3f2dc), 2);
        assert_eq!(ability_kind(0x2a3f3a4), 3);
        assert_eq!(ability_kind(0x2a3efbc), 0);
    }
    #[test]
    fn hud_geometry_is_bounded_and_draws_no_unknown_state() {
        let mut p = FeaturePacket::new(EVENT_STAMINA);
        assert_eq!(rectangles(p).count, 0);
        for (i, f) in [0.5f32, 1.0, 0.5, 0.775, 0.145, 0.006].iter().enumerate() {
            p.words[i] = f.to_bits();
        }
        let f = rectangles(p);
        assert!(f.valid());
        assert_eq!(f.count, 2);
        assert_eq!(f.rects[1].w, f.rects[0].w * 0.5);
        p.words[2] = f32::NAN.to_bits();
        assert_eq!(rectangles(p).count, 0);
    }
    #[test]
    #[ignore = "requires reviewed MSVC x86 fixture; execute explicitly"]
    fn real_msvc_rfd_bridge_and_melee_float_abi() {
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryW(p: *const u16) -> *mut c_void;
            fn GetProcAddress(m: *mut c_void, n: *const u8) -> *mut c_void;
        }
        let path = std::env::var("SCBL_EQUIPMENT_ABI_FIXTURE").unwrap();
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let module = LoadLibraryW(path.as_ptr());
            assert!(!module.is_null());
            let callback: unsafe extern "system" fn() -> u32 =
                std::mem::transmute(GetProcAddress(module, b"_fixture_callback@0\0".as_ptr()));
            let resume: unsafe extern "system" fn() -> u32 =
                std::mem::transmute(GetProcAddress(module, b"_fixture_resume@0\0".as_ptr()));
            let run: unsafe extern "system" fn(*mut c_void, u32) -> u32 =
                std::mem::transmute(GetProcAddress(module, b"_fixture_run@8\0".as_ptr()));
            let address: unsafe extern "system" fn() -> usize =
                std::mem::transmute(GetProcAddress(module, b"_fixture_melee@0\0".as_ptr()));
            let invoke: unsafe extern "system" fn(*mut f32, u8) -> f32 =
                std::mem::transmute(GetProcAddress(module, b"_fixture_invoke@8\0".as_ptr()));
            let memory = VirtualAlloc(std::ptr::null_mut(), 128, 0x3000, 0x04);
            assert!(!memory.is_null());
            let code = bridge_bytes(memory as u32, callback(), resume());
            std::ptr::copy_nonoverlapping(code.as_ptr(), memory as _, code.len());
            let mut old = 0;
            assert_ne!(VirtualProtect(memory, 128, 0x20, &mut old), 0);
            assert_ne!(
                FlushInstructionCache(GetCurrentProcess(), memory, code.len()),
                0
            );
            assert_eq!(run(memory, 1), 0, "native visibility preserved");
            assert_eq!(
                run(memory, 0),
                0,
                "additional veto, GPR/x87/XMM/stack preserved"
            );
            static_detour! {static FixtureMelee:unsafe extern "thiscall" fn(*mut f32,*mut u8,u8)->f32;}
            fn replacement(this: *mut f32, target: *mut u8, context: u8) -> f32 {
                unsafe {
                    let native = FixtureMelee.call(this, target, context);
                    crate::equipment_rules::melee_range(native, *this, true)
                }
            }
            let mut multiplier = 1.5f32;
            assert_eq!(invoke(&mut multiplier, 0), 270.0);
            assert_eq!(invoke(&mut multiplier, 1), 180.0);
            FixtureMelee
                .initialize(std::mem::transmute(address()), replacement)
                .unwrap();
            FixtureMelee.enable().unwrap();
            for _ in 0..1000 {
                assert_eq!(invoke(&mut multiplier, 0), 270.0);
                assert_eq!(invoke(&mut multiplier, 1), 180.0);
            }
            FixtureMelee.disable().unwrap();
            assert_eq!(invoke(&mut multiplier, 0), 270.0);
            println!("NATIVE_EQUIPMENT_PROOF RFD visible/veto 1000 each preserved frame,GPR,x87,XMM,stack; MSVC thiscall+ret8 float/x87 original270/180 -> unchanged270/180 -> restored270; context and target preserved");
        }
    }
    #[test]
    fn gear_identity_reads_exact_asset_not_camo_or_unknown_slot() {
        let name: Vec<u16> = "MPArmorGlovesGhost".encode_utf16().chain(Some(0)).collect();
        let mut part = [0u32; 16];
        part[0x28 / 4] = name.as_ptr() as u32;
        part[0x2c / 4] = name.len() as u32;
        part[0x30 / 4] = name.len() as u32;
        let slots = [part.as_ptr() as u32];
        let mut armor_data = [0u32; 32];
        armor_data[0x50 / 4] = slots.as_ptr() as u32;
        armor_data[0x54 / 4] = 1;
        armor_data[0x58 / 4] = 1;
        unsafe {
            assert!(equipped(
                armor_data.as_ptr() as usize,
                b"MPArmorGlovesGhost"
            ));
            assert!(!equipped(
                armor_data.as_ptr() as usize,
                b"MPArmorPantsGhost"
            ));
            assert!(!equipped(
                armor_data.as_ptr() as usize,
                b"MPArmorGlovesGhostCamo"
            ));
            armor_data[0x54 / 4] = 13;
            assert!(!equipped(
                armor_data.as_ptr() as usize,
                b"MPArmorGlovesGhost"
            ));
        }
    }
}
