//! Executes the retail DX11 instruction windows with isolated leaf callbacks.
//! Not a substitute for multi-PC gameplay. The same inputs run on R55 and R56.
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
static SCORES: AtomicU32 = AtomicU32::new(0);
static CLEARS: AtomicU32 = AtomicU32::new(0);
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, kind: u32, protection: u32) -> *mut c_void;
    fn VirtualFree(p: *mut c_void, n: usize, kind: u32) -> i32;
    fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
}
unsafe extern "thiscall" fn score(_: usize, _: usize, event: u32) {
    assert_eq!(event, 0x21);
    SCORES.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "thiscall" fn clear(p: usize) {
    *((p + 0x1a08) as *mut u32) &= !0x800;
    CLEARS.fetch_add(1, Ordering::Relaxed);
}
unsafe fn put(p: usize, b: &[u8]) {
    std::ptr::copy_nonoverlapping(b.as_ptr(), p as *mut u8, b.len());
    assert_ne!(
        FlushInstructionCache(GetCurrentProcess(), p as _, b.len()),
        0
    );
}
unsafe fn jmp(p: usize, dst: usize) {
    let mut b = vec![0xe9];
    b.extend((dst as u32).wrapping_sub(p as u32 + 5).to_le_bytes());
    put(p, &b);
}
#[test]
#[ignore = "requires the exact retail DX11 windows; CI uses --include-ignored"]
fn r56_retail_teammate_mark_score_and_drone_recharge() {
    let dir = std::env::var("SCBL_R56_NATIVE_FIXTURE").expect("required native fixture");
    let dir = std::path::PathBuf::from(dir);
    unsafe {
        let base = VirtualAlloc(std::ptr::null_mut(), 0xc00000, 0x3000, 0x40) as usize;
        assert_ne!(base, 0);
        let relocation = base.wrapping_sub(0x400000);
        let addr = |v: usize| v.wrapping_add(relocation);
        let team = std::fs::read(dir.join("team.bin")).unwrap();
        assert_eq!(team.len(), 194);
        let mut manager = vec![0u32; 0x80 / 4];
        let mut vt = vec![0u32; 0x600 / 4];
        put(base + 0x400, &[0xb8, 1, 0, 0, 0, 0xc3]);
        vt[0x5cc / 4] = (base + 0x400) as u32;
        manager[0] = vt.as_mut_ptr() as u32;
        let mut getter = vec![0xb8];
        getter.extend((manager.as_mut_ptr() as u32).to_le_bytes());
        getter.push(0xc3);
        put(addr(0xe8ca30), &getter);
        put(addr(0xba2040), &[0x8b, 0x44, 0x24, 4, 0xc3]);
        put(addr(0xc94fc0), &[0xb8, 1, 0, 0, 0, 0xc3]);
        put(addr(0x55fbc0), &[0x31, 0xc0, 0xc3]);
        jmp(addr(0xef7400), score as *const () as usize);
        jmp(addr(0xb5ed10), clear as *const () as usize);
        let enter: unsafe extern "thiscall" fn(usize, usize) = std::mem::transmute(addr(0xbad1a0));
        for (name, active, merc, marked) in [
            ("intel_merc", true, true, true),
            ("expired_merc", false, true, true),
            ("spy", true, false, true),
            ("unmarked", true, true, false),
        ] {
            put(addr(0xbad1a0), &team);
            crate::balance_native_adapter::patch(relocation, active);
            let mut pawn = vec![0u32; 0x1b00 / 4];
            let mut marks = vec![10f32.to_bits(), 11, 12, 13, 20f32.to_bits(), 21, 22, 23];
            pawn[0] = if merc { 0x02ad2abc } else { 0x02ad3234 };
            pawn[0x1a90 / 4] = marks.as_mut_ptr() as u32;
            pawn[0x1a94 / 4] = if marked { 2 } else { 0 };
            pawn[0x1a98 / 4] = 2;
            pawn[0x1a08 / 4] = if marked { 0x800 } else { 0 };
            SCORES.store(0, Ordering::Relaxed);
            CLEARS.store(0, Ordering::Relaxed);
            enter(pawn.as_mut_ptr() as usize, 0x12345);
            let keep = crate::balance_native_adapter::ENABLED && active && merc && marked;
            let scores = SCORES.load(Ordering::Relaxed);
            let clears = CLEARS.load(Ordering::Relaxed);
            assert_eq!(pawn[0x1a94 / 4], if keep { 2 } else { 0 });
            assert_eq!(pawn[0x1a08 / 4] & 0x800 != 0, keep);
            assert_eq!(pawn[0x1a08 / 4] & 8, 8, "friendly protection tail must run");
            assert_eq!(scores, if marked && !keep { 1 } else { 0 });
            assert_eq!(clears, scores);
            println!("R56_TEAM case={name} marks={} detected={} score={} clear_calls={} friendly_protection=true",pawn[0x1a94/4],keep,scores,clears);
        }
        let mut code = std::fs::read(dir.join("recharge.bin")).unwrap();
        assert_eq!(code.len(), 191);
        for (va, offset) in [
            (0x2aeb2bcu32, 0x100usize),
            (0x2989888, 0x110),
            (0x2995478, 0x120),
        ] {
            let data = std::fs::read(dir.join(format!("0x{va:x}.bin"))).unwrap();
            put(base + offset, &data);
            let pattern = va.to_le_bytes();
            let mut found = 0;
            for i in 0..code.len() - 3 {
                if code[i..i + 4] == pattern {
                    code[i..i + 4].copy_from_slice(&((base + offset) as u32).to_le_bytes());
                    found += 1;
                }
            }
            assert!(found > 0);
        }
        put(addr(0xe6cfb0), &code);
        let recharge: unsafe extern "thiscall" fn(usize, f32) = std::mem::transmute(addr(0xe6cfb0));
        for (name, drone, merc) in [
            ("drone_merc", true, true),
            ("other_ability", false, true),
            ("drone_other_owner", true, false),
        ] {
            let mut pawn = vec![0u32; 0x1b00 / 4];
            pawn[0] = if merc { 0x02ad2abc } else { 0x02ad3234 };
            let mut ability = vec![0u32; 0xc0 / 4];
            ability[0] = if drone { 0x02a3f14c } else { 0x02a3f084 };
            ability[0x6c / 4] = pawn.as_mut_ptr() as u32;
            ability[0x2c / 4] = 60f32.to_bits();
            ability[0x44 / 4] = 99f32.to_bits();
            crate::balance_native_adapter::recharge(ability.as_mut_ptr() as usize, 15.0, recharge);
            let at15 = f32::from_bits(ability[0x7c / 4]);
            crate::balance_native_adapter::recharge(ability.as_mut_ptr() as usize, 15.0, recharge);
            let at30 = f32::from_bits(ability[0x7c / 4]);
            let changed = crate::balance_native_adapter::ENABLED && drone && merc;
            assert!((at15 - if changed { 1.0 } else { 0.25 }).abs() < 0.00001);
            assert!((at30 - if changed { 1.0 } else { 0.5 }).abs() < 0.00001);
            assert_eq!(ability[0x2c / 4], 60f32.to_bits());
            assert_eq!(ability[0x44 / 4], 99f32.to_bits());
            println!("R56_COOLDOWN case={name} charge15={at15:.3} charge30={at30:.3} native_fields_restored=true");
        }
        // Additional R58 boundary proof; the R56 15s/30s input pair above is
        // deliberately kept for baseline/modified/rollback comparison.
        for (seconds, expected) in [(0.0f32, 0.0f32), (14.0, 14.0 / 15.0), (15.0, 1.0), (16.0, 1.0), (20.0, 1.0), (25.0, 1.0)] {
            let mut pawn = vec![0u32; 0x1b00 / 4];
            pawn[0] = 0x02ad2abc;
            let mut ability = vec![0u32; 0xc0 / 4];
            ability[0] = 0x02a3f14c;
            ability[0x6c / 4] = pawn.as_mut_ptr() as u32;
            ability[0x2c / 4] = 60f32.to_bits();
            crate::drone_cooldown::fixture_recharge(
                ability.as_mut_ptr() as usize,
                seconds,
                recharge,
            );
            let result = f32::from_bits(ability[0x7c / 4]);
            assert!((result - expected).abs() < 0.00001);
            assert_eq!(ability[0x2c / 4], 60f32.to_bits());
            println!(
                "R58_COOLDOWN elapsed={seconds:.1} charge={result:.3} native_fields_restored=true"
            );
        }
        println!(
            "R56_EMP health_policy_enabled={}",
            crate::balance_native_adapter::health_emp()
        );
        assert_eq!(
            crate::balance_native_adapter::health_emp(),
            !crate::balance_native_adapter::ENABLED
        );
        assert_ne!(VirtualFree(base as _, 0, 0x8000), 0);
    }
}

#[test]
#[ignore = "requires the exact retail DX11 drain instruction window"]
fn r58_retail_drone_battery_half_preserves_native_movement_and_other_timers() {
    use sha2::{Digest, Sha256};
    let dir = std::path::PathBuf::from(
        std::env::var("SCBL_R58_BATTERY_FIXTURE").expect("required native drain fixture"),
    );
    let mut code = std::fs::read(dir.join("drain.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&code)),
        "377df237a91acb181b81fc6cecba41274e545543346061b303010dc8509a5813"
    );
    let one = std::fs::read(dir.join("one.bin")).unwrap();
    assert_eq!(f64::from_le_bytes(one.clone().try_into().unwrap()), 1.0);
    unsafe {
        let base = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 0x40) as usize;
        assert_ne!(base, 0);
        put(base + 0x200, &one);
        let mut replacements = 0;
        for i in 0..code.len() - 3 {
            if code[i..i + 4] == 0x2989888u32.to_le_bytes() {
                code[i..i + 4].copy_from_slice(&((base + 0x200) as u32).to_le_bytes());
                replacements += 1;
            }
        }
        assert_eq!(replacements, 1);
        put(base, &code);
        let native: unsafe extern "thiscall" fn(usize, f32) = std::mem::transmute(base);
        for (label, duration, multiplier, dt) in [
            ("idle10", 120.0f32, 1.0f32, 10.0f32),
            ("moving10", 120.0, 4.0, 10.0),
            ("idle60", 120.0, 1.0, 60.0),
            ("moving15", 120.0, 4.0, 15.0),
            ("configured240", 240.0, 1.0, 60.0),
            ("capture_idle600", 120.0, 1.0, 600.0),
            ("capture_moving150", 120.0, 4.0, 150.0),
        ] {
            let mut pawn = vec![0u32; 0x70 / 4];
            pawn[0] = 0x02ad2abc;
            pawn[0x4c / 4] = 1 << 8;
            let mut ability = vec![0u32; 0xc0 / 4];
            ability[0] = 0x02a3f14c;
            ability[0x28 / 4] = 3;
            ability[0x40 / 4] = 9;
            ability[0x6c / 4] = pawn.as_mut_ptr() as u32;
            ability[0x2c / 4] = 20f32.to_bits();
            ability[0x3c / 4] = duration.to_bits();
            ability[0x7c / 4] = 1f32.to_bits();
            ability[0x84 / 4] = multiplier.to_bits();
            ability[0x90 / 4] = 7f32.to_bits();
            ability[0x98 / 4] = 11f32.to_bits();
            let pristine = ability.clone();
            native(ability.as_mut_ptr() as usize, dt);
            let baseline = f32::from_bits(ability[0x7c / 4]);
            ability.copy_from_slice(&pristine);
            crate::drone_cooldown::fixture_drain(ability.as_mut_ptr() as usize, dt, native);
            let modified = f32::from_bits(ability[0x7c / 4]);
            let factor = if cfg!(feature = "drone-capture-long-battery") {
                5.0
            } else {
                0.5
            };
            assert!(
                (modified - (1.0 - dt * multiplier / (duration * factor)).max(0.0)).abs() < 0.00001
            );
            for i in 0..ability.len() {
                if i != 0x7c / 4 {
                    assert_eq!(ability[i], pristine[i], "unrelated word {i}");
                }
            }
            // The scoped duration edit has already rolled back; invoke the same
            // retail code/input again to prove no persistent drain multiplier.
            ability[0x7c / 4] = 1f32.to_bits();
            native(ability.as_mut_ptr() as usize, dt);
            let restored_native = f32::from_bits(ability[0x7c / 4]);
            assert_eq!(restored_native, baseline);
            println!("R58_BATTERY case={label} baseline={baseline:.6} modified={modified:.6} scope_restored={restored_native:.6} other_timers_unchanged=true");
        }
        // Unknown types, inactive/charging/disabled abilities, replicated owners,
        // non-merc owners and other equipment must keep native consumption.
        for case in 0..7 {
            let mut pawn = vec![0u32; 0x70 / 4];
            pawn[0] = if case == 5 { 0x02ad3234 } else { 0x02ad2abc };
            pawn[0x4c / 4] = if case == 4 { 2 << 8 } else { 1 << 8 };
            let mut ability = vec![0u32; 0xc0 / 4];
            ability[0] = if case == 6 { 0x02a3f084 } else { 0x02a3f14c };
            ability[0x28 / 4] = if case == 0 { 4 } else { 3 };
            ability[0x40 / 4] = match case {
                1 => 1,
                2 => 0x29,
                3 => 0xd,
                _ => 9,
            };
            ability[0x6c / 4] = pawn.as_mut_ptr() as u32;
            ability[0x2c / 4] = 20f32.to_bits();
            ability[0x3c / 4] = 120f32.to_bits();
            ability[0x7c / 4] = 1f32.to_bits();
            ability[0x84 / 4] = 1f32.to_bits();
            crate::drone_cooldown::fixture_drain(ability.as_mut_ptr() as usize, 10.0, native);
            assert!((f32::from_bits(ability[0x7c / 4]) - 11.0 / 12.0).abs() < 0.00001);
            assert_eq!(ability[0x3c / 4], 120f32.to_bits());
        }
        assert_ne!(VirtualFree(base as _, 0, 0x8000), 0);
    }
}

// C8: user-requested additional five-second redeploy reduction. Native E6CFB0,
// authority/threshold/clamp logic and the production temporary denominator run.
#[test]
#[ignore = "owned retail recharge instruction window"]
fn c8_cd15_native_boundary() {
    use sha2::{Digest, Sha256};
    let dir=std::path::PathBuf::from(std::env::var("SCBL_R56_NATIVE_FIXTURE").unwrap());
    unsafe {
        let base=VirtualAlloc(std::ptr::null_mut(),0xc00000,0x3000,0x40) as usize;
        assert_ne!(base,0,"C8_CD15_SETUP allocation");
        struct Free(usize);
        impl Drop for Free { fn drop(&mut self) { unsafe { VirtualFree(self.0 as _,0,0x8000); } } }
        let _free=Free(base);
        let relocation=base.wrapping_sub(0x400000);
        let addr=|v:usize|v.wrapping_add(relocation);
        let mut code = std::fs::read(dir.join("recharge.bin")).unwrap();
        assert_eq!(code.len(), 191);
        assert_eq!(format!("{:x}", Sha256::digest(&code)), "9e85d42c0a0975c683133637e0ecc2ffd97d201207be0a6c1e28eaff8cdb25da");
        for (va, offset) in [
            (0x2aeb2bcu32, 0x100usize),
            (0x2989888, 0x110),
            (0x2995478, 0x120),
        ] {
            let data = std::fs::read(dir.join(format!("0x{va:x}.bin"))).unwrap();
            put(base + offset, &data);
            let pattern = va.to_le_bytes();
            let mut found = 0;
            for i in 0..code.len() - 3 {
                if code[i..i + 4] == pattern {
                    code[i..i + 4].copy_from_slice(&((base + offset) as u32).to_le_bytes());
                    found += 1;
                }
            }
            assert!(found > 0);
        }
        put(addr(0xe6cfb0), &code);
        let recharge: unsafe extern "thiscall" fn(usize, f32) = std::mem::transmute(addr(0xe6cfb0));

        let mut all=true;
        for (name,seconds,drone,merc,expected) in [
            ("zero",0.0f32,true,true,0.0f32),
            ("before",14.0,true,true,14.0/15.0),
            ("at",15.0,true,true,1.0),
            ("after",16.0,true,true,1.0),
            ("other_ability",15.0,false,true,0.25),
            ("other_owner",15.0,true,false,0.25),
        ] {
            let mut pawn=vec![0u32;0x1b00/4];pawn[0]=if merc {0x02ad2abc}else{0x02ad3234};
            let mut ability=vec![0u32;0xc0/4];ability[0]=if drone {0x02a3f14c}else{0x02a3f084};
            ability[0x6c/4]=pawn.as_mut_ptr() as u32;
            ability[0x2c/4]=60f32.to_bits();ability[0x44/4]=99f32.to_bits();
            crate::drone_cooldown::fixture_recharge(ability.as_mut_ptr() as usize,seconds,recharge);
            let charge=f32::from_bits(ability[0x7c/4]);
            let hud=crate::drone_cooldown::with_hud_duration(ability.as_mut_ptr() as usize,
                ||f32::from_bits(ability[0x2c/4]));
            let restored=ability[0x2c/4]==60f32.to_bits()&&ability[0x44/4]==99f32.to_bits();
            let passed=(charge-expected).abs()<0.00001
                &&hud==(if drone&&merc {15.0}else{60.0})&&restored;
            all&=passed;
            println!("C8_CD15 {{\"case\":\"{}\",\"seconds\":{},\"charge\":{},\"hud\":{},\"restored\":{},\"passed\":{}}}",
                name,seconds,charge,hud,restored,passed);
        }
        assert!(all,"C8_CD15_ORACLE native 14/15-second boundary, HUD duration and exact restore");
    }
}
