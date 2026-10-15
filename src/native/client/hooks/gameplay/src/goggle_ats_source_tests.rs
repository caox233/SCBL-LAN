use super::*;
static STATE_TEST: Mutex<()> = Mutex::new(());
fn put(v: &mut [u32], o: usize, x: u32) {
    v[o / 4] = x;
}
struct Fixture {
    hud: Vec<u32>,
    context: Vec<u32>,
    entries: Vec<u32>,
    pawn: Vec<u32>,
    popup: Vec<u32>,
    display: Vec<u32>,
    stack: Vec<u32>,
    wave: Vec<u32>,
    frame: Frame,
    people: Vec<(u32, u64, Role)>,
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self {
            hud: vec![0; 0x310 / 4],
            context: vec![0; 0x320 / 4],
            entries: vec![0; 0x58 / 4],
            pawn: vec![0; 0x150 / 4],
            popup: vec![0; 0x1b0 / 4],
            display: vec![0; 0x50 / 4],
            stack: vec![0; 0x210 / 4],
            wave: vec![0; 0x18 / 4],
            frame: Frame {
                world: 0x11110000,
                info: 0x11120000,
                local: 0x11130000,
                team: 1,
                now: 100,
                position: [0.; 3],
                distance_delay_ms: 0,
                visible: true,
            },
            people: vec![(42, 0x1122334455667788, Role::Spy)],
        };
        put(&mut f.hud, 0, HUD);
        put(&mut f.hud, 0x25c, f.context.as_ptr() as u32);
        put(&mut f.context, 0x310, f.frame.local as u32);
        put(&mut f.hud, 0x2c8, f.entries.as_ptr() as u32);
        put(&mut f.hud, 0x2cc, 1);
        put(&mut f.hud, 0x2d0, 2);
        put(&mut f.hud, 0x2f8, 1);
        put(&mut f.entries, 0, f.popup.as_ptr() as u32);
        put(&mut f.entries, 0x10, f.pawn.as_ptr() as u32);
        put(&mut f.entries, 0x14, f.pawn.as_ptr() as u32);
        put(&mut f.entries, 0x18, 4);
        put(&mut f.pawn, 0, 0x2ad3234);
        put(&mut f.pawn, 0x20, 42);
        put(&mut f.pawn, 0x144, f.frame.world as u32);
        put(&mut f.popup, 0x1a8, f.display.as_ptr() as u32);
        put(&mut f.stack, 0x200 - 0x1bc, f.hud.as_ptr() as u32);
        put(&mut f.stack, 0x200 - 0x58, 0);
        put(&mut f.wave, 4, 0.5f32.to_bits());
        put(&mut f.wave, 0x14, 1);
        f
    }
    fn pending(&self) -> Pending {
        Pending {
            hud: self.hud.as_ptr() as usize,
            frame: self.frame,
            people: self.people.clone(),
            waves: Vec::new(),
        }
    }
    fn capture(&self, waves: &[(u32, u64)]) -> Option<Snapshot> {
        unsafe { capture(self.hud.as_ptr() as usize, &self.frame, &self.people, waves) }
    }
    unsafe fn wave(&self, p: &mut Pending) {
        record_wave(
            self.stack.as_ptr() as usize + 0x200,
            self.wave.as_ptr() as usize,
            p,
        );
    }
}
#[test]
fn native_popup_and_actual_wave_are_independent_positive_sources() {
    let mut f = Fixture::new();
    assert!(f.capture(&[]).unwrap().targets.is_empty());
    put(&mut f.display, 0x40, 1);
    assert_eq!(f.capture(&[]).unwrap().targets.len(), 1);
    put(&mut f.display, 0x40, 0);
    put(&mut f.entries, 0, 0);
    let mut p = f.pending();
    unsafe { f.wave(&mut p) };
    assert_eq!(f.capture(&p.waves).unwrap().targets, [0x1122334455667788]);
}
#[test]
fn entry_or_short_distance_alone_does_not_create_a_detection() {
    let mut f = Fixture::new();
    f.frame.position = [0.; 3];
    // No range check/cached resolver output substitutes for an accepted write.
    assert!(f.capture(&[]).unwrap().targets.is_empty());
    put(&mut f.entries, 0x18, 2);
    assert!(f.capture(&[]).unwrap().targets.is_empty());
}
#[test]
fn zero_negative_nan_and_uncommitted_wave_writes_are_ignored() {
    let mut f = Fixture::new();
    for v in [0., -1., f32::NAN, f32::INFINITY] {
        put(&mut f.wave, 4, v.to_bits());
        let mut p = f.pending();
        unsafe { f.wave(&mut p) };
        assert!(p.waves.is_empty());
    }
    put(&mut f.wave, 4, 0.5f32.to_bits());
    put(&mut f.wave, 0x14, 0);
    let mut p = f.pending();
    unsafe { f.wave(&mut p) };
    assert!(p.waves.is_empty());
}
#[test]
fn stale_frame_index_or_different_hud_cannot_record() {
    let mut f = Fixture::new();
    let mut p = f.pending();
    put(&mut f.stack, 0x200 - 0x58, 1);
    unsafe { f.wave(&mut p) };
    assert!(p.waves.is_empty());
    put(&mut f.stack, 0x200 - 0x58, 0);
    put(&mut f.stack, 0x200 - 0x1bc, 99);
    unsafe { f.wave(&mut p) };
    assert!(p.waves.is_empty());
}
#[test]
fn live_roster_spy_identity_required_not_equipment_actor() {
    let mut f = Fixture::new();
    put(&mut f.display, 0x40, 1);
    put(&mut f.entries, 0x10, 0);
    assert!(f.capture(&[]).unwrap().targets.is_empty());
    put(&mut f.entries, 0x10, f.pawn.as_ptr() as u32);
    f.people[0].2 = Role::Merc;
    assert!(f.capture(&[]).unwrap().targets.is_empty());
    f.people[0].2 = Role::Spy;
    f.people.push((42, 111, Role::Spy));
    assert!(f.capture(&[]).unwrap().targets.is_empty());
    f.people.pop();
    put(&mut f.pawn, 0x144, 0);
    assert!(f.capture(&[]).unwrap().targets.is_empty());
}
#[test]
fn native_goggles_off_foreign_owner_and_invalid_array_fail_closed() {
    let mut f = Fixture::new();
    put(&mut f.display, 0x40, 1);
    put(&mut f.hud, 0x2f8, 0);
    assert!(f.capture(&[]).unwrap().targets.is_empty());
    put(&mut f.hud, 0x2f8, 2);
    assert!(f.capture(&[]).is_none());
    put(&mut f.hud, 0x2f8, 1);
    put(&mut f.context, 0x310, 0);
    assert!(f.capture(&[]).is_none());
    put(&mut f.context, 0x310, f.frame.local as u32);
    for (n, cap) in [(129, 129), (2, 1), (1, 4097)] {
        put(&mut f.hud, 0x2cc, n);
        put(&mut f.hud, 0x2d0, cap);
        assert!(f.capture(&[]).is_none());
    }
}
#[test]
fn native_removed_or_recycled_source_does_not_reuse_wave_observation() {
    let mut f = Fixture::new();
    let mut p = f.pending();
    unsafe { f.wave(&mut p) };
    assert_eq!(f.capture(&p.waves).unwrap().targets.len(), 1);
    put(&mut f.hud, 0x2cc, 0);
    assert!(f.capture(&p.waves).unwrap().targets.is_empty());
    put(&mut f.hud, 0x2cc, 1);
    f.people[0].1 = 333;
    assert!(f.capture(&p.waves).unwrap().targets.is_empty());
    put(&mut f.pawn, 0x20, 43);
    assert!(f.capture(&p.waves).unwrap().targets.is_empty());
}
#[test]
fn repeated_wave_bins_and_popups_coalesce_without_native_writes() {
    let mut f = Fixture::new();
    put(&mut f.display, 0x40, 1);
    let before = vec![
        f.hud.clone(),
        f.entries.clone(),
        f.pawn.clone(),
        f.popup.clone(),
        f.display.clone(),
        f.stack.clone(),
        f.wave.clone(),
    ];
    let mut p = f.pending();
    unsafe {
        for _ in 0..20 {
            f.wave(&mut p)
        }
    };
    assert_eq!(p.waves.len(), 1);
    assert_eq!(f.capture(&p.waves).unwrap().targets.len(), 1);
    assert_eq!(
        before,
        vec![f.hud, f.entries, f.pawn, f.popup, f.display, f.stack, f.wave]
    );
}
#[test]
fn cache_is_current_native_frame_not_a_persistent_mark_lease() {
    let _guard = STATE_TEST.lock().unwrap();
    let f = Fixture::new();
    publish(f.capture(&[(42, 0x1122334455667788)]));
    assert_eq!(snapshot(&f.frame).unwrap().targets.len(), 1);
    let mut next = f.frame;
    next.now += 1;
    assert!(snapshot(&next).is_none());
    next = f.frame;
    next.local += 4;
    assert!(snapshot(&next).is_none());
    clear();
    assert!(snapshot(&f.frame).is_none());
}
#[test]
fn unsupported_observer_still_executes_original_once() {
    let _guard = STATE_TEST.lock().unwrap();
    READY.store(false, Ordering::Release);
    let n = std::cell::Cell::new(0);
    unsafe {
        update(0, || n.set(n.get() + 1));
    }
    assert_eq!(n.get(), 1);
}
#[test]
fn execute_native_wave_bridge_preserves_flags_registers_xmm_and_stack() {
    let _guard = STATE_TEST.lock().unwrap();
    use sha2::{Digest, Sha256};
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualFree(p: *mut c_void, n: usize, t: u32) -> i32;
    }
    let root = std::path::PathBuf::from(
        std::env::var("SCBL_R58_C29D_FIXTURE").expect("C29D native fixture required"),
    );
    let whole = std::fs::read(root.join("contract-01a6f990.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&whole)),
        "a25c45764a9645e8f7ed6dd2cbac0e4853b005318734f904268838fe5e7dd805"
    );
    for site in SITES {
        assert_eq!(&whole[site - 0x1a6f990..site - 0x1a6f990 + 7], &ORIGINAL);
    }
    unsafe {
        let mut f = Fixture::new();
        put(&mut f.entries, 0, 0);
        READY.store(true, Ordering::Release);
        PENDING.with(|v| v.replace(Some(f.pending())));
        for (label, value, expected) in [
            ("movement", 0.5f32, 1usize),
            ("zero", 0., 0),
            ("negative", -1., 0),
        ] {
            put(&mut f.wave, 4, value.to_bits());
            put(&mut f.wave, 0x14, 0);
            PENDING.with(|v| v.borrow_mut().as_mut().unwrap().waves.clear());
            let p = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4) as usize;
            assert_ne!(p, 0);
            let mut wrapper = vec![0x55, 0x89, 0xe5, 0x81, 0xec, 0x00, 0x02, 0, 0, 0xc7, 0x85];
            wrapper.extend((-0x1bci32).to_le_bytes());
            wrapper.extend((f.hud.as_ptr() as u32).to_le_bytes());
            wrapper.extend([0xc7, 0x45, 0xa8, 0, 0, 0, 0, 0xb9]);
            wrapper.extend((f.wave.as_ptr() as u32).to_le_bytes());
            wrapper.extend([0xba]);
            wrapper.extend(0x11223344u32.to_le_bytes());
            wrapper.push(0xb8);
            wrapper.extend(0.25f32.to_bits().to_le_bytes());
            wrapper.extend([0x66, 0x0f, 0x6e, 0xc0, 0xf9, 0xe9]);
            wrapper.extend(
                ((p + 128) as u32)
                    .wrapping_sub(p as u32 + wrapper.len() as u32 + 4)
                    .to_le_bytes(),
            );
            let b = bridge(
                (p + 128) as u32,
                on_wave as *const () as u32,
                (p + 512) as u32,
            );
            // Check carry, caller registers, native write, original XMM, then
            // return through the original stack frame. Success returns 123.
            let mut resume = vec![0x73, 0];
            let mut branches = vec![1];
            for bytes in [
                vec![0x81, 0xfa, 0x44, 0x33, 0x22, 0x11],
                {
                    let mut x = vec![0x81, 0xf9];
                    x.extend((f.wave.as_ptr() as u32).to_le_bytes());
                    x
                },
                vec![0x83, 0x79, 0x14, 1],
                {
                    let mut x = vec![0x66, 0x0f, 0x7e, 0xc2, 0x81, 0xfa];
                    x.extend(0.25f32.to_bits().to_le_bytes());
                    x
                },
            ] {
                resume.extend(bytes);
                resume.extend([0x75, 0]);
                branches.push(resume.len() - 1);
            }
            resume.extend([0xb8, 123, 0, 0, 0, 0xc9, 0xc3]);
            let fail = resume.len();
            resume.extend([0x31, 0xc0, 0xc9, 0xc3]);
            for i in branches {
                resume[i] = (fail - i - 1) as u8;
            }
            for (offset, code) in [
                (0, wrapper.as_slice()),
                (128, b.as_slice()),
                (512, resume.as_slice()),
            ] {
                std::ptr::copy_nonoverlapping(code.as_ptr(), (p + offset) as _, code.len());
            }
            let mut old = 0;
            assert_ne!(VirtualProtect(p as _, 4096, 0x20, &mut old), 0);
            assert_ne!(FlushInstructionCache(GetCurrentProcess(), p as _, 4096), 0);
            let run = std::mem::transmute::<usize, unsafe extern "C" fn() -> u32>(p);
            for _ in 0..1000 {
                assert_eq!(run(), 123);
            }
            PENDING.with(|v| {
                let v = v.borrow();
                let v = v.as_ref().unwrap();
                assert_eq!(v.waves.len(), expected);
                assert_eq!(f.capture(&v.waves).unwrap().targets.len(), expected);
            });
            println!("C29D_NATIVE_ATS {label} captured={expected} calls=1000 native_updated=1 CF_GPR_XMM_stack_preserved=true");
            assert_ne!(VirtualFree(p as _, 0, 0x8000), 0);
        }
        PENDING.with(|v| v.replace(None));
        READY.store(false, Ordering::Release);
        clear();
    }
}
