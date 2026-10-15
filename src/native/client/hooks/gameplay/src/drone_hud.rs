//! Keep native ArmorAbilityBar segments (native smoothed charge at +88) and
//! movie layouts. Only its existing timer is borrowed during voluntary reentry.
//! No overlay widget, no synthetic battery and no input/eligibility override.
use crate::equipment_native::{readable, signature, word, writable};
use retour::static_detour;
use sha2::{Digest, Sha256};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
static COPY: AtomicUsize = AtomicUsize::new(0);
const HUD: u32 = 0x02c40234;
static READY: AtomicBool = AtomicBool::new(false);
static_detour! { static Update:unsafe extern "thiscall" fn(usize,f32); }
type Frame = unsafe extern "thiscall" fn(usize, *const u8);
// 18BF320 forwards to 18BDA80: UTF-16 comparison and GFx StringW (type 7).
// Its FString overload converts native UTF-8 through 4AFF80 before forwarding.
type Text = unsafe extern "thiscall" fn(usize, *const u16) -> u8;
unsafe fn ability(hud: usize) -> Option<usize> {
    if word(hud, 0) != Some(HUD) {
        return None;
    }
    // Exact 1A52310 owner chain used by the native update.
    let pawn = word(hud, 0x25c).and_then(|v| word(v as usize, 0x310))? as usize;
    if word(pawn, 0) != Some(0x02ad2abc) {
        return None;
    }
    let armor = word(pawn, 0xcf4)? as usize;
    if word(armor, 0) != Some(0x02ad42ac) {
        return None;
    }
    let ability = word(armor, 0x5c)? as usize;
    (word(ability, 0) == Some(0x02a3f14c) && word(ability, 0x6c) == Some(pawn as u32))
        .then_some(ability)
}
unsafe fn present(hud: usize, seconds: u8, play: Frame, stop: Frame, text: Text) -> bool {
    if seconds > 3 || word(hud, 0) != Some(HUD) || !writable(hud + 0x33c, 8) {
        return false;
    }
    let offsets = [0x2c0, 0x2f4, 0x2d8, 0x308];
    let mut widgets = [0usize; 4];
    for (i, off) in offsets.into_iter().enumerate() {
        let Some(p) = word(hud, off)
            .map(|x| x as usize)
            .filter(|x| readable(*x, 0x1ac))
        else {
            return false;
        };
        widgets[i] = p;
    }
    let countdown = *((hud + 0x341) as *const u8) != 0;
    let icon = *((hud + 0x340) as *const u8) != 0;
    // Match native movie semantics: Countdown is a reveal animation, not a
    // static first frame. Play only on transition, never restart every update.
    if seconds > 0 && !countdown {
        play(widgets[0], b"Countdown\0".as_ptr());
        play(widgets[1], b"Countdown\0".as_ptr());
    } else if seconds == 0 && !icon {
        stop(widgets[0], b"Icon\0".as_ptr());
        stop(widgets[1], b"Icon\0".as_ptr());
    }
    if seconds > 0 && (!countdown || word(hud, 0x33c) != Some(seconds as u32)) {
        // A byte terminator is insufficient: the backend reads 16-bit units.
        let value = [u16::from(b'0') + u16::from(seconds), 0u16];
        text(widgets[2], value.as_ptr());
        text(widgets[3], value.as_ptr());
    }
    *((hud + 0x33c) as *mut u32) = seconds as u32;
    *((hud + 0x340) as *mut u8) = u8::from(seconds == 0);
    *((hud + 0x341) as *mut u8) = u8::from(seconds > 0);
    true
}
const UPDATE: usize = 0x1a6f0a0;
const UPDATE_SHA: &str = "212f76374a7c68c001ee1ae41caae575a2d398d44e28b71162afeda2380e4141";
const CALLS: &[(usize, usize)] = &[
    (0x34, 0x1a52310),
    (0x8f, 0x1a585f0),
    (0xb2, 0x1a587f0),
    (0xce, 0x1a3f940),
    (0xee, 0x1a492a0),
    (0x1a6, 0xaba850),
    (0x1e7, 0x44a6f0),
    (0x1f9, 0x18bf320),
    (0x20a, 0x44a6f0),
    (0x21c, 0x18bf320),
    (0x22f, 0x44a6f0),
    (0x241, 0x18bf320),
    (0x252, 0x44a6f0),
    (0x264, 0x18bf320),
    (0x271, 0xaba850),
    (0x2bd, 0x18bf320),
    (0x2d3, 0x18bf320),
    (0x2e9, 0x18bf320),
    (0x2ff, 0x18bf320),
    (0x315, 0x44a6f0),
    (0x327, 0x18bf320),
    (0x338, 0x44a6f0),
    (0x34a, 0x18bf320),
    (0x35b, 0x44a6f0),
    (0x36d, 0x18bf320),
    (0x37e, 0x44a6f0),
    (0x390, 0x18bf320),
    (0x3a6, 0x44a6f0),
    (0x3b8, 0x18bf320),
    (0x3c9, 0x44a6f0),
    (0x3db, 0x18bf320),
    (0x3ec, 0x44a6f0),
    (0x3fe, 0x18bf320),
    (0x40f, 0x44a6f0),
    (0x421, 0x18bf320),
    (0x590, 0x4b93f0),
    (0x5b5, 0x44bc50),
    (0x5c1, 0x42e2a0),
    (0x5dd, 0x18bf360),
    (0x5f2, 0x18bf360),
    (0x610, 0x42e370),
    (0x639, 0x18ca220),
    (0x64f, 0x18ca220),
    (0x75d, 0x18a2270),
    (0x782, 0x1a42100),
];

/// Replace only the native timer decision block, after native battery segments
/// were updated. This prevents the native draining-battery Icon branch from
/// hiding/restarting our countdown each frame. All other UI logic stays native.
fn timer_window(bytes: &[u8], at: usize, bridge: usize) -> Option<Vec<u8>> {
    if bytes.len() != 1944
        || format!("{:x}", Sha256::digest(bytes)) != UPDATE_SHA
        || at == 0
        || bridge == 0
        || at.checked_add(0x900)? > u32::MAX as usize
        || bridge > u32::MAX as usize
    {
        return None;
    }
    let mut out = bytes.to_vec();
    for &(off, target) in CALLS {
        if out[off] != 0xe8 {
            return None;
        }
        let rel = i32::from_le_bytes(out[off + 1..off + 5].try_into().ok()?);
        if (UPDATE + off + 5).wrapping_add_signed(rel as isize) != target {
            return None;
        }
        out[off + 1..off + 5].copy_from_slice(
            &(target as u32)
                .wrapping_sub((at + off + 5) as u32)
                .to_le_bytes(),
        );
    }
    let original = out[0x433..0x43b].to_vec();
    if original != [0x8b, 0x55, 0xec, 0xf3, 0x0f, 0x10, 0x42, 0x30] {
        return None;
    }
    out.resize(0x800, 0x90);
    out[0x433..0x43b].copy_from_slice(&[0xe9, 0xc8, 3, 0, 0, 0x90, 0x90, 0x90]);
    // Both joins reload XMM temporaries. Preserve all GPRs and flags; no
    // native x87 value is live at this boundary. ECX is the native HUD `this`.
    out.extend_from_slice(&[0x9c, 0x60, 0x8b, 0x8d, 0x30, 0xff, 0xff, 0xff, 0xe8]);
    out.extend_from_slice(
        &(bridge as u32)
            .wrapping_sub((at + 0x80d) as u32)
            .to_le_bytes(),
    );
    out.extend_from_slice(&[0x84, 0xc0, 0x75, 15, 0x61, 0x9d]);
    out.extend_from_slice(&original);
    out.push(0xe9);
    out.extend_from_slice(&(0x43bu32.wrapping_sub(0x820)).to_le_bytes());
    out.extend_from_slice(&[0x61, 0x9d, 0xe9]);
    out.extend_from_slice(&(0x66eu32.wrapping_sub(0x827)).to_le_bytes());
    Some(out)
}
unsafe extern "thiscall" fn timer_bridge(hud: usize) -> u8 {
    let Some(wait) = ability(hud).and_then(|a| crate::drone_control::hud_wait(a)) else {
        return 0;
    };
    u8::from(present(
        hud,
        wait,
        std::mem::transmute::<usize, Frame>(0x18ca1b0),
        std::mem::transmute::<usize, Frame>(0x18ca220),
        std::mem::transmute::<usize, Text>(0x18bf320),
    ))
}
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, kind: u32, protection: u32) -> *mut c_void;
    fn VirtualProtect(p: *mut c_void, n: usize, protection: u32, old: *mut u32) -> i32;
    fn VirtualFree(p: *mut c_void, n: usize, kind: u32) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
}
unsafe fn make_copy() -> Option<usize> {
    let p = VirtualAlloc(std::ptr::null_mut(), 0x1000, 0x3000, 4) as usize;
    if p == 0 {
        return None;
    }
    let result = (|| {
        let code = timer_window(
            std::slice::from_raw_parts(UPDATE as *const u8, 1944),
            p,
            timer_bridge as *const () as usize,
        )?;
        std::ptr::copy_nonoverlapping(code.as_ptr(), p as *mut u8, code.len());
        let mut old = 0;
        if VirtualProtect(p as _, 0x1000, 0x20, &mut old) == 0
            || FlushInstructionCache(GetCurrentProcess(), p as _, code.len()) == 0
        {
            return None;
        }
        Some(p)
    })();
    if result.is_none() {
        VirtualFree(p as _, 0, 0x8000);
    }
    result
}
fn update(hud: usize, dt: f32) {
    unsafe {
        let a = if READY.load(Ordering::Acquire) {
            ability(hud)
        } else {
            None
        };
        if let Some(a) = a {
            let native: unsafe extern "thiscall" fn(usize, f32) =
                std::mem::transmute(COPY.load(Ordering::Acquire));
            crate::drone_cooldown::with_hud_duration(a, || native(hud, dt));
        } else {
            Update.call(hud, dt);
        }
    }
}
pub(crate) fn init(world: usize) {
    if world != 0x589860 {
        return;
    }
    unsafe {
        for (va, n, sha) in [
            (
                0x1a6f0a0,
                1944,
                "212f76374a7c68c001ee1ae41caae575a2d398d44e28b71162afeda2380e4141",
            ),
            (
                0x1a3f940,
                386,
                "b8a18c6d7cfa8883098d1abb37791de702fed563e4d31e8260eed3061f1fd88f",
            ),
            (
                0x18bf320,
                60,
                "5125847850556767bc858280c4541dbf892a28ad36c6864e5fc29be2ed1495de",
            ),
            (
                0x18ca1b0,
                104,
                "d189cc0bb15c2cf79438e6bdc7d1f5e173a180da5d33245c768747e10a8b0cd9",
            ),
            (
                0x18ca220,
                70,
                "d26b8ca67cf261af76fa69aa65f6f06e3abc5ef6b630f8d5ab95db0bacc90237",
            ),
        ] {
            if !signature(va, n, sha) {
                tracing::error!("SCBL_MOD_DRONE_HUD enabled=false reason=native-signature");
                return;
            }
        }
        let Some(copy) = make_copy() else {
            return;
        };
        COPY.store(copy, Ordering::Release);
        if Update
            .initialize(
                std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(0x1a6f0a0),
                update,
            )
            .is_err()
            || Update.enable().is_err()
        {
            let _ = Update.disable();
            return;
        }
        READY.store(true, Ordering::Release);
        tracing::info!("SCBL_MOD_DRONE_HUD enabled=true native_segments=true native_timer=reentry_3s_or_deploy_{}s", crate::drone_cooldown::SECONDS);
    }
}
pub(crate) fn deinit() {
    READY.store(false, Ordering::Release);
    unsafe {
        let _ = Update.disable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "thiscall" fn frame(w: usize, s: *const u8) {
        *((w + 4) as *mut u32) = if *s == b'C' { 1 } else { 0 };
    }
    unsafe extern "thiscall" fn text(w: usize, s: *const u16) -> u8 {
        *((w + 8) as *mut u32) = *s as u32;
        assert_eq!(*s.add(1), 0);
        1
    }
    #[test]
    fn existing_two_layouts_countdown_and_native_segments_untouched() {
        unsafe {
            let mut hud = vec![0u32; 0x390 / 4];
            hud[0] = HUD;
            let mut w: Vec<Vec<u32>> = (0..4).map(|_| vec![0u32; 0x1ac / 4]).collect();
            for (i, o) in [0x2c0, 0x2f4, 0x2d8, 0x308].into_iter().enumerate() {
                hud[o / 4] = w[i].as_mut_ptr() as u32;
            }
            hud[0x314 / 4] = 0xabcdef;
            hud[0x334 / 4] = 0.73f32.to_bits();
            for seconds in [3, 3, 2, 1, 0] {
                assert!(present(
                    hud.as_mut_ptr() as usize,
                    seconds,
                    frame,
                    frame,
                    text
                ));
                assert_eq!(w[0][1], u32::from(seconds > 0));
                assert_eq!(w[1][1], u32::from(seconds > 0));
                if seconds > 0 {
                    assert_eq!(w[2][2], u32::from(b'0' + seconds));
                    assert_eq!(w[3][2], w[2][2]);
                }
                assert_eq!(hud[0x314 / 4], 0xabcdef);
                assert_eq!(hud[0x334 / 4], 0.73f32.to_bits());
            }
            let original = hud.clone();
            assert!(!present(hud.as_mut_ptr() as usize, 4, frame, frame, text));
            assert_eq!(hud, original);
            hud[0] = 0;
            assert!(!present(hud.as_mut_ptr() as usize, 2, frame, frame, text));
        }
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    static WAIT: AtomicU32 = AtomicU32::new(3);
    unsafe extern "thiscall" fn play(w: usize, _: *const u8) {
        *((w + 4) as *mut u32) += 1;
        *((w + 12) as *mut u32) = 1;
    }
    unsafe extern "thiscall" fn stop(w: usize, _: *const u8) {
        *((w + 8) as *mut u32) += 1;
        *((w + 12) as *mut u32) = 0;
    }
    unsafe extern "thiscall" fn text(w: usize, s: *const u16) -> u8 {
        assert_eq!(*s.add(1), 0);
        *((w + 16) as *mut u32) = *s as u32;
        1
    }
    unsafe extern "thiscall" fn bridge(hud: usize) -> u8 {
        let wait = WAIT.load(Ordering::SeqCst);
        if wait > 3 {
            0
        } else {
            u8::from(present(hud, wait as u8, play, stop, text))
        }
    }
    unsafe fn put(p: usize, b: &[u8]) {
        std::ptr::copy_nonoverlapping(b.as_ptr(), p as *mut u8, b.len());
    }
    #[test]
    #[ignore = "requires exact native HUD bytes"]
    fn c11_native_timer_branch_plays_once_and_is_not_hidden_by_draining_battery() {
        unsafe {
            let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C11_FIXTURE").unwrap());
            let raw = std::fs::read(root.join("hud_update.bin")).unwrap();
            for stage in ["BASELINE", "MODIFIED", "ROLLBACK", "FALLBACK"] {
                let base = VirtualAlloc(std::ptr::null_mut(), 0x3000000, 0x3000, 0x40) as usize;
                assert_ne!(base, 0);
                let at = base + UPDATE - 0x400000;
                let mut hud = vec![0u32; 0x390 / 4];
                hud[0] = HUD;
                let mut a = vec![0u32; 0xc0 / 4];
                a[0x30 / 4] = 100f32.to_bits();
                a[0x7c / 4] = 0.7f32.to_bits();
                hud[0x334 / 4] = 0.8f32.to_bits();
                hud[0x314 / 4] = 0xabcdef;
                let widgets: Vec<Vec<u32>> = (0..4).map(|_| vec![0u32; 0x240 / 4]).collect();
                for (i, off) in [0x2c0, 0x2f4, 0x2d8, 0x308].into_iter().enumerate() {
                    hud[off / 4] = widgets[i].as_ptr() as u32;
                }
                let modified = matches!(stage, "MODIFIED" | "FALLBACK");
                let mut code = if modified {
                    timer_window(&raw, at, bridge as *const () as usize).unwrap()
                } else {
                    raw.clone()
                };
                for (off, addr) in [
                    (24, 52988860usize),
                    (261, 43554952),
                    (656, 43554952),
                    (1090, 43603064),
                    (1136, 43594024),
                    (1228, 47668128),
                ] {
                    code[off..off + 4]
                        .copy_from_slice(&((base + addr - 0x400000) as u32).to_le_bytes());
                    let bytes = std::fs::read(root.join(format!("data-{addr:x}.bin"))).unwrap();
                    put(base + addr - 0x400000, &bytes);
                }
                // Native timer is the tested unit: return at its original join;
                // the production clone keeps the rest of the entire update.
                code[0x66e..0x672].copy_from_slice(&[0x8b, 0xe5, 0x5d, 0xc3]);
                for off in [0x639, 0x64f] {
                    code[off + 1..off + 5].copy_from_slice(
                        &(stop as *const () as u32)
                            .wrapping_sub((at + off + 5) as u32)
                            .to_le_bytes(),
                    );
                }
                put(at, &code);
                let entry = base + 0x100;
                let mut wrapper = vec![
                    0x55, 0x8b, 0xec, 0x81, 0xec, 0xe0, 0, 0, 0, 0x89, 0x8d, 0x30, 0xff, 0xff,
                    0xff, 0xc7, 0x45, 0xec,
                ];
                wrapper.extend_from_slice(&(a.as_ptr() as u32).to_le_bytes());
                wrapper.push(0xe9);
                wrapper.extend_from_slice(
                    &((at + 0x433) as u32)
                        .wrapping_sub((entry + wrapper.len() + 4) as u32)
                        .to_le_bytes(),
                );
                put(entry, &wrapper);
                let run: unsafe extern "thiscall" fn(usize) = std::mem::transmute(entry);
                for seconds in [3, 3, 2, 1, 0] {
                    WAIT.store(
                        if stage == "FALLBACK" { 255 } else { seconds },
                        Ordering::SeqCst,
                    );
                    run(hud.as_mut_ptr() as usize);
                    assert_eq!(hud[0x314 / 4], 0xabcdef);
                    assert_eq!(a[0x7c / 4], 0.7f32.to_bits());
                    if stage == "MODIFIED" && seconds > 0 {
                        assert_eq!(widgets[0][3], 1);
                        assert_eq!(widgets[1][3], 1);
                        assert_eq!(widgets[2][4], u32::from(b'0') + seconds);
                    }
                }
                assert_eq!(widgets[0][1], u32::from(stage == "MODIFIED"));
                assert_eq!(widgets[1][1], widgets[0][1]);
                assert_eq!(widgets[0][2], 1);
                assert_eq!(widgets[0][3], 0);
                println!(
                    "R58_C11_HUD {stage} countdown_play={} icon_stop={} segments_unchanged=true",
                    widgets[0][1], widgets[0][2]
                );
                assert_ne!(VirtualFree(base as _, 0, 0x8000), 0);
            }
        }
    }
    #[test]
    #[ignore = "requires exact native HUD bytes"]
    fn c11_timer_clone_rejects_unknown_code_and_keeps_non_timer_bytes() {
        let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C11_FIXTURE").unwrap());
        let raw = std::fs::read(root.join("hud_update.bin")).unwrap();
        let code = timer_window(&raw, UPDATE, 0x1000).unwrap();
        assert_eq!(&code[..0x433], &raw[..0x433]);
        assert_eq!(&code[0x43b..1944], &raw[0x43b..]);
        for i in [0, 0x433, 0x590, 1943] {
            let mut bad = raw.clone();
            bad[i] ^= 1;
            assert!(timer_window(&bad, UPDATE, 0x1000).is_none());
        }
    }
}

#[cfg(test)]
mod wide_abi_tests {
    use super::*;
    unsafe extern "thiscall" fn backend(
        out: usize,
        property: usize,
        value: *const u16,
        _: u32,
    ) -> u8 {
        assert_eq!(property, 0x2c12a84);
        let dst = out as *mut u16;
        let mut i = 0;
        while i < 7 && *value.add(i) != 0 {
            *dst.add(i) = *value.add(i);
            i += 1;
        }
        *dst.add(i) = 0;
        1
    }
    unsafe extern "thiscall" fn frame(_: usize, _: *const u8) {}
    #[test]
    #[ignore = "requires native text ABI fixture"]
    fn c12_native_text_wrapper_reads_wide_terminator_and_both_hud_layouts() {
        unsafe {
            let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C12_FIXTURE").unwrap());
            let mut raw = std::fs::read(root.join("text.bin")).unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(&raw)),
                "5125847850556767bc858280c4541dbf892a28ad36c6864e5fc29be2ed1495de"
            );
            let mem = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 0x40) as usize;
            assert_ne!(mem, 0);
            for off in [0x1e, 0x31] {
                assert_eq!(raw[off], 0xe8);
                raw[off + 1..off + 5].copy_from_slice(
                    &(backend as *const () as u32)
                        .wrapping_sub((mem + off + 5) as u32)
                        .to_le_bytes(),
                );
            }
            std::ptr::copy_nonoverlapping(raw.as_ptr(), mem as *mut u8, raw.len());
            let native: Text = std::mem::transmute(mem);
            let mut hud = vec![0u32; 0x390 / 4];
            hud[0] = HUD;
            let mut widgets: Vec<Vec<u32>> = (0..4).map(|_| vec![0u32; 0x1ac / 4]).collect();
            let mut outputs = [[0u16; 8]; 2];
            for (i, off) in [0x2c0, 0x2f4, 0x2d8, 0x308].into_iter().enumerate() {
                hud[off / 4] = widgets[i].as_mut_ptr() as u32;
                if i >= 2 {
                    widgets[i][0x1a8 / 4] = outputs[i - 2].as_mut_ptr() as u32;
                }
            }
            // Deterministic adjacent-stack sentinel: C11's two bytes terminate
            // ASCII but not UTF-16; the real game's extra character is undefined.
            let old = [u16::from(b'3'), u16::from(b'L'), 0u16];
            native(widgets[2].as_mut_ptr() as usize, old.as_ptr());
            assert_eq!(&outputs[0][..3], &[51, 76, 0]);
            println!("R58_C12_TEXT BASELINE old_byte_buffer_with_adjacent_L=3L");
            for seconds in [3, 2, 1] {
                assert!(present(
                    hud.as_mut_ptr() as usize,
                    seconds,
                    frame,
                    frame,
                    native
                ));
                assert_eq!(&outputs[0][..2], &[48 + u16::from(seconds), 0]);
                assert_eq!(&outputs[1][..2], &[48 + u16::from(seconds), 0]);
                println!(
                    "R58_C12_TEXT MODIFIED seconds={seconds} both_layouts_one_utf16_digit=true"
                );
            }
            native(widgets[2].as_mut_ptr() as usize, old.as_ptr());
            assert_eq!(&outputs[0][..3], &[51, 76, 0]);
            println!("R58_C12_TEXT ROLLBACK old_byte_buffer_with_adjacent_L=3L");
            assert_ne!(VirtualFree(mem as _, 0, 0x8000), 0);
        }
    }
}
