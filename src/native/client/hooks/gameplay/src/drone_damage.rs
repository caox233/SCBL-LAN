//! MP-drone-only damage windows. Retail actor damage spawns terminal effects
//! before the AD3920 endpoint, so changing AD3920 alone cannot suppress bullets.
//! Keep damage, invulnerability, source, score and non-lethal effects. On lethal
//! damage skip the terminal damage-level FX/audio and enter the existing fall.
use super::{apply, original, route, Fall, Route, DRONE};
use crate::equipment_native::{signature, word};
use retour::static_detour;
use sha2::{Digest, Sha256};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
type Damage = unsafe extern "thiscall" fn(usize, u32, usize, usize, usize, usize, usize) -> u32;
static READY: AtomicBool = AtomicBool::new(false);
static BANK: AtomicUsize = AtomicUsize::new(0);
static_detour! {
 static Hit:unsafe extern "thiscall" fn(usize,u32,usize,usize,usize,usize,usize)->u32;
 static Dead:unsafe extern "thiscall" fn(usize);
}
struct Spec {
    va: usize,
    n: usize,
    sha: &'static str,
    calls: &'static [(usize, usize)],
}
const MP: Spec = Spec {
    va: 0xb2a5f0,
    n: 455,
    sha: "c8f002f2d592b8c5df2923338ea8418e7894e540ee4a9cc64cfb6f5077c92114",
    calls: &[
        (0x32, 0x8c86f0),
        (0x54, 0xb07790),
        (0x60, 0x8dcba0),
        (0x77, 0x8dcba0),
        (0x8a, 0x8c86f0),
        (0x130, 0xf1b310),
        (0x18a, 0xd873f0),
        (0x1a8, 0xe8ca30),
        (0x1b7, 0xef7520),
    ],
};
const PARENT: Spec = Spec {
    va: 0xf1b310,
    n: 233,
    sha: "ec45c1a07d324223619b27abeaba038462cbffc59129ec43555843c8128793f6",
    calls: &[
        (0x3c, 0x8b5e30),
        (0x74, 0xe35300),
        (0x87, 0xe96d70),
        (0x90, 0xf135f0),
    ],
};
const ACTOR: Spec = Spec {
    va: 0xe35300,
    n: 1730,
    sha: "f55755f22453a29e4dee7b250130615c57cd6ce92475c28edfcd1beefc97163b",
    calls: &[
        (0x4d, 0x8c86f0),
        (0x77, 0xdf8a10),
        (0x81, 0xe23fa0),
        (0xd2, 0x8ba690),
        (0xef, 0x8b5e60),
        (0x229, 0x55a4d0),
        (0x27a, 0x4f1300),
        (0x3a9, 0x535200),
        (0x675, 0x8b5e60),
        (0x68f, 0xe22290),
    ],
};
const DEAD: Spec = Spec {
    va: 0xb0ed30,
    n: 333,
    sha: "1aab9f195daec93503505d1e34e9adcaacbfd8d0f6b37b66e8b39f4ad1c71740",
    calls: &[
        (0x3, 0xaf5f60),
        (0xe9, 0xe93df0),
        (0x10e, 0xa32bc0),
        (0x118, 0xe96eb0),
        (0x11d, 0xe8ca30),
        (0x129, 0xe8d5a0),
        (0x146, 0x8f1340),
    ],
};

const GADGET: Spec = Spec {
    va: 0xf135f0,
    n: 1486,
    sha: "39208442a5146f778ae77e0a092bde7f83fc0fe8ffbe23e898ee988906685d69",
    calls: &[
        (0x52, 0xbbe910),
        (0x130, 0xc5e090),
        (0x175, 0xd8b9d0),
        (0x180, 0xb2e5d0),
        (0x1a7, 0xc4e440),
        (0x1b9, 0xb53f30),
        (0x1f1, 0x8b5e60),
        (0x23a, 0xe93df0),
        (0x26a, 0xa32bc0),
        (0x278, 0xd148c0),
        (0x2b9, 0xb07c90),
        (0x2c8, 0xb2ae20),
        (0x2f3, 0x4b39c0),
        (0x2ff, 0x44ef80),
        (0x314, 0x44ef80),
        (0x350, 0xec28c0),
        (0x362, 0xf08e50),
        (0x36d, 0x450bf0),
        (0x37c, 0x450bf0),
        (0x419, 0xe69130),
        (0x467, 0x8b5e30),
        (0x4f9, 0xc94fc0),
        (0x507, 0xe66590),
        (0x51b, 0x9f0560),
        (0x578, 0x8b5e60),
        (0x5a0, 0x8b8370),
        (0x5a9, 0xe8ca30),
        (0x5b5, 0xef7520),
    ],
};

fn relative(code: &mut [u8], off: usize, at: usize, target: usize) {
    code[off + 1..off + 5].copy_from_slice(
        &(target as u32)
            .wrapping_sub((at + off + 5) as u32)
            .to_le_bytes(),
    );
}
fn copied(raw: &[u8], spec: &Spec, at: usize) -> Option<Vec<u8>> {
    if raw.len() != spec.n
        || format!("{:x}", Sha256::digest(raw)) != spec.sha
        || at == 0
        || at.checked_add(0x1000)? > u32::MAX as usize
    {
        return None;
    }
    let mut v = raw.to_vec();
    for &(o, d) in spec.calls {
        if v[o] != 0xe8 && v[o] != 0xe9 {
            return None;
        }
        let old = i32::from_le_bytes(v[o + 1..o + 5].try_into().ok()?);
        if (spec.va + o + 5).wrapping_add_signed(old as isize) != d {
            return None;
        }
        relative(&mut v, o, at, d);
    }
    Some(v)
}
fn mp_window(raw: &[u8], at: usize, parent: usize) -> Option<Vec<u8>> {
    let mut v = copied(raw, &MP, at)?;
    relative(&mut v, 0x130, at, parent);
    Some(v)
}
fn parent_window(raw: &[u8], at: usize, actor: usize, gadget: usize) -> Option<Vec<u8>> {
    let mut v = copied(raw, &PARENT, at)?;
    relative(&mut v, 0x74, at, actor);
    relative(&mut v, 0x90, at, gadget);
    // Preserve E96D70 notification and F135F0 scoring. Death audio at +8C0
    // belongs to the explosive endpoint, not the native falling motor sound.
    v[0x95..0xb7].fill(0x90);
    Some(v)
}
fn gadget_window(raw: &[u8], at: usize) -> Option<Vec<u8>> {
    let mut v = copied(raw, &GADGET, at)?;
    // F135F0 has another explosive FX/audio path and schedules delayed detonation.
    // Preserve its owner HUD-mode notification and kill credit, but not this
    // terminal-only explosion/popup sequence. Native Fall already owns shutdown.
    v[0x2cd..0x55c].fill(0x90);
    v[0x2cd] = 0xe9;
    relative(&mut v, 0x2cd, at, at + 0x55c);
    // Earlier native branches can enter +423 (remote owner): take same join.
    v[0x423] = 0xe9;
    relative(&mut v, 0x423, at, at + 0x55c);
    Some(v)
}
fn actor_window(raw: &[u8], at: usize) -> Option<Vec<u8>> {
    let mut v = copied(raw, &ACTOR, at)?;
    // HP and damage fraction already updated. Only dead bodies skip FX table.
    // Do NOT fake health, zero the damage, edit shared effect entries, or skip death.
    let mut gate = vec![0x83, 0xbe, 0xdc, 5, 0, 0, 0, 0x0f, 0x8e];
    gate.extend((0x60bi32 - 0x18e).to_le_bytes()); // E3590B
    gate.extend([0x83, 0xbe, 0xc8, 5, 0, 0, 0, 0x0f, 0x8e]);
    gate.extend((0x60bi32 - 0x19b).to_le_bytes());
    // Need a trampoline rather than overwrite damage-loop entry at +1D0.
    // Original 1BE..1CA is CMP count,EAX + JLE. EAX is zero at this site.
    v.resize(0x700, 0x90);
    let off = v.len();
    v.extend(gate);
    v.extend([0xc7, 0x45, 0xf8, 0, 0, 0, 0, 0xe9]);
    v.extend((0x1d0i32 - (off as i32 + 38)).to_le_bytes());
    // Recalculate both long JLEs from trampoline, not the original boundary.
    v[off + 9..off + 13].copy_from_slice(&(0x60bi32 - (off as i32 + 13)).to_le_bytes());
    v[off + 22..off + 26].copy_from_slice(&(0x60bi32 - (off as i32 + 26)).to_le_bytes());
    v[0x1be..0x1ca].fill(0x90);
    v[0x1be] = 0xe9;
    relative(&mut v, 0x1be, at, at + off);
    Some(v)
}
fn dead_window(raw: &[u8], at: usize, fall: usize) -> Option<Vec<u8>> {
    let mut v = copied(raw, &DEAD, at)?;
    // Native fall will cancel tags and spawn the nonexplosive falling FX once.
    // Retain native attachment cleanup and owner feedback before replacing removal.
    v[0..0x82].fill(0x90); // prologue + early clear/spawn block
    v[0..3].copy_from_slice(&[0x56, 0x8b, 0xf1]); // push esi; mov esi,ecx
    v[3] = 0xe9;
    relative(&mut v, 3, at, at + 0x82);
    v[0x116..].fill(0x90); // E96EB0 deactivation+duplicate cleanup now waits for landing
    v[0x116..0x11a].copy_from_slice(&[0x8b, 0xce, 0xe8, 0]);
    relative(&mut v, 0x118, at, fall);
    v[0x11d..0x11f].copy_from_slice(&[0x5e, 0xc3]);
    Some(v)
}
unsafe extern "thiscall" fn fall_bridge(body: usize) {
    apply(
        body,
        1,
        DRONE,
        original,
        std::mem::transmute::<usize, Fall>(0xb0f3b0),
        std::mem::transmute::<usize, Fall>(0xfe0be0),
    );
}
fn hit(body: usize, d: u32, t: usize, p: usize, v: usize, b: usize, source: usize) -> u32 {
    unsafe {
        if !READY.load(Ordering::Acquire) || route(body, DRONE) == Route::Native {
            return Hit.call(body, d, t, p, v, b, source);
        }
        if word(body, 0x910).map(|v| v & 255) == Some(3)
            && word(body, 0x5dc).is_some_and(|v| v as i32 <= 0)
        {
            return 0;
        }
        let f: Damage = std::mem::transmute(BANK.load(Ordering::Acquire));
        f(body, d, t, p, v, b, source)
    }
}
fn dead(body: usize) {
    unsafe {
        if READY.load(Ordering::Acquire) && route(body, DRONE) == Route::Fall {
            let f: Fall = std::mem::transmute(BANK.load(Ordering::Acquire) + 0xc00);
            f(body);
        } else {
            Dead.call(body);
        }
    }
}
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, protection: u32) -> *mut c_void;
    fn VirtualProtect(p: *mut c_void, n: usize, protection: u32, old: *mut u32) -> i32;
    fn VirtualFree(p: *mut c_void, n: usize, t: u32) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
}
pub(super) unsafe fn init() -> bool {
    if READY.load(Ordering::Acquire) {
        return true;
    }
    for spec in [&MP, &PARENT, &ACTOR, &DEAD, &GADGET] {
        if !signature(spec.va, spec.n, spec.sha) {
            return false;
        }
    }
    let at = VirtualAlloc(std::ptr::null_mut(), 0x2000, 0x3000, 4) as usize;
    if at == 0 {
        return false;
    }
    let raw = |s: &Spec| std::slice::from_raw_parts(s.va as *const u8, s.n);
    let windows = [
        (0, mp_window(raw(&MP), at, at + 0x200)),
        (
            0x200,
            parent_window(raw(&PARENT), at + 0x200, at + 0x400, at + 0x1000),
        ),
        (0x400, actor_window(raw(&ACTOR), at + 0x400)),
        (
            0xc00,
            dead_window(raw(&DEAD), at + 0xc00, fall_bridge as *const () as usize),
        ),
        (0x1000, gadget_window(raw(&GADGET), at + 0x1000)),
    ];
    for (off, v) in windows {
        let Some(v) = v else {
            VirtualFree(at as _, 0, 0x8000);
            return false;
        };
        std::ptr::copy_nonoverlapping(v.as_ptr(), (at + off) as *mut u8, v.len());
    }
    let mut old = 0;
    if VirtualProtect(at as _, 0x2000, 0x20, &mut old) == 0
        || FlushInstructionCache(GetCurrentProcess(), at as _, 0x2000) == 0
    {
        VirtualFree(at as _, 0, 0x8000);
        return false;
    }
    BANK.store(at, Ordering::Release);
    if Hit
        .initialize(std::mem::transmute::<usize, Damage>(MP.va), hit)
        .is_err()
        || Dead
            .initialize(std::mem::transmute::<usize, Fall>(DEAD.va), dead)
            .is_err()
        || Dead.enable().is_err()
        || Hit.enable().is_err()
    {
        let _ = Hit.disable();
        let _ = Dead.disable();
        return false;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_DRONE_DAMAGE enabled=true lethal=fall native_damage_score=true terminal_fx_audio=false");
    true
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Hit.disable();
    let _ = Dead.disable();
}
#[cfg(test)]
#[path = "drone_damage_tests.rs"]
mod tests;
