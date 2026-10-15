use super::*;

fn put(v: &mut [u32], offset: usize, value: u32) {
    v[offset / 4] = value;
}
struct Fixture {
    hud: Vec<u32>,
    context: Vec<u32>,
    entries: Vec<u32>,
    widget: Vec<u32>,
    display: Vec<u32>,
    frame: Frame,
    people: Vec<(u32, u64, Role)>,
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self {
            hud: vec![0; 0x340 / 4],
            context: vec![0; 0x320 / 4],
            entries: vec![0; 0x58 / 4],
            widget: vec![0; 0x1b0 / 4],
            display: vec![0; 0x50 / 4],
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
            people: vec![(42, 0x1122334455667788, Role::Spy), (43, 222, Role::Merc)],
        };
        put(&mut f.hud, 0, HUD);
        put(&mut f.hud, 0x25c, f.context.as_ptr() as u32);
        put(&mut f.hud, 0x328, 1);
        put(&mut f.hud, 0x2b4, f.entries.as_ptr() as u32);
        put(&mut f.hud, 0x2b8, 1);
        put(&mut f.hud, 0x2bc, 2);
        put(&mut f.context, 0x310, f.frame.local as u32);
        put(&mut f.entries, 0, f.widget.as_ptr() as u32);
        put(&mut f.entries, 4, 42);
        put(&mut f.entries, 8, 1);
        put(&mut f.widget, 0x1a8, f.display.as_ptr() as u32);
        put(&mut f.display, 0x40, 1);
        f
    }
    fn capture(&self) -> Option<Snapshot> {
        unsafe { capture(self.hud.as_ptr() as usize, &self.frame, &self.people) }
    }
}
#[test]
fn native_visible_result_resolves_object_key_not_is_player_as_pointer() {
    let f = Fixture::new();
    assert_eq!(f.entries[2], 1);
    assert_eq!(f.capture().unwrap().targets, [0x1122334455667788]);
}
#[test]
fn native_rejection_does_not_turn_into_a_distance_guess() {
    let mut f = Fixture::new();
    put(&mut f.display, 0x40, 0);
    assert!(f.capture().unwrap().targets.is_empty());
    // There is intentionally no position/range fallback in the collector.
    f.frame.position = [100000., 0., 0.];
    assert!(f.capture().unwrap().targets.is_empty());
}
#[test]
fn pulse_alpha_zero_is_not_a_new_native_rejection() {
    let mut f = Fixture::new();
    put(&mut f.display, 0x38, 0f32.to_bits());
    assert_eq!(f.capture().unwrap().targets.len(), 1);
    put(&mut f.display, 0x40, 0);
    assert_eq!(f.capture().unwrap().targets.len(), 0);
}
#[test]
fn native_goggle_disable_clears_even_when_old_widget_is_visible() {
    let mut f = Fixture::new();
    put(&mut f.hud, 0x328, 0);
    assert!(f.capture().unwrap().targets.is_empty());
    put(&mut f.hud, 0x328, 2);
    assert!(f.capture().is_none());
}
#[test]
fn foreign_hud_and_owner_are_rejected() {
    let mut f = Fixture::new();
    put(&mut f.hud, 0, HUD + 4);
    assert!(f.capture().is_none());
    put(&mut f.hud, 0, HUD);
    put(&mut f.context, 0x310, 99);
    assert!(f.capture().is_none());
}
#[test]
fn missing_or_non_spy_roster_identity_does_not_warn() {
    let mut f = Fixture::new();
    put(&mut f.entries, 4, 999);
    assert!(f.capture().unwrap().targets.is_empty());
    put(&mut f.entries, 4, 43);
    assert!(f.capture().unwrap().targets.is_empty());
    put(&mut f.entries, 4, 42);
    f.people[0].1 = 0;
    assert!(f.capture().unwrap().targets.is_empty());
}
#[test]
fn equipment_entries_and_null_widgets_are_not_players() {
    let mut f = Fixture::new();
    put(&mut f.entries, 8, 0);
    assert!(f.capture().unwrap().targets.is_empty());
    put(&mut f.entries, 8, 1);
    put(&mut f.entries, 0, 0);
    assert!(f.capture().unwrap().targets.is_empty());
}
#[test]
fn arrays_are_bounded_and_unknown_identity_is_fail_closed() {
    let mut f = Fixture::new();
    put(&mut f.hud, 0x2b8, 129);
    assert!(f.capture().is_none());
    put(&mut f.hud, 0x2b8, 2);
    put(&mut f.hud, 0x2bc, 1);
    assert!(f.capture().is_none());
    put(&mut f.hud, 0x2b8, 1);
    put(&mut f.hud, 0x2bc, 4097);
    assert!(f.capture().is_none());
    put(&mut f.hud, 0x2bc, 2);
    f.people.push((42, 9999, Role::Spy));
    assert!(f.capture().is_none());
}
#[test]
fn duplicates_are_coalesced_and_reads_do_not_mutate_native_storage() {
    let mut f = Fixture::new();
    let first = f.entries[..11].to_vec();
    f.entries[11..22].copy_from_slice(&first);
    put(&mut f.hud, 0x2b8, 2);
    let before = (
        f.hud.clone(),
        f.context.clone(),
        f.entries.clone(),
        f.widget.clone(),
        f.display.clone(),
    );
    assert_eq!(f.capture().unwrap().targets.len(), 1);
    assert_eq!(before, (f.hud, f.context, f.entries, f.widget, f.display));
}
#[test]
fn published_result_cannot_be_reused_as_a_fresh_frame_or_other_scene() {
    let f = Fixture::new();
    *CURRENT.lock().unwrap() = f.capture();
    assert_eq!(snapshot(&f.frame).unwrap().targets.len(), 1);
    let mut next = f.frame;
    next.now += 1;
    assert!(snapshot(&next).is_none());
    next = f.frame;
    next.world += 4;
    assert!(snapshot(&next).is_none());
    next = f.frame;
    next.local += 4;
    assert!(snapshot(&next).is_none());
    *CURRENT.lock().unwrap() = None;
    assert!(snapshot(&f.frame).is_none());
}

// Execute the exact retail visibility setter at a fixture-owned ABI boundary.
// Its one absolute UI-registry load is relocated to a null fixture registry;
// no real GFx renderer or game process is used by this test.
#[test]
fn exact_native_visibility_setter_drives_the_production_collector() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, protect: u32) -> *mut c_void;
        fn VirtualProtect(p: *mut c_void, n: usize, protect: u32, old: *mut u32) -> i32;
        fn VirtualFree(p: *mut c_void, n: usize, kind: u32) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
    }
    use sha2::{Digest, Sha256};
    let dir = std::env::var("SCBL_R58_C29B_FIXTURE").expect("C29B retail native fixture required");
    let mut code = std::fs::read(std::path::Path::new(&dir).join("contract-018a1c80.bin")).unwrap();
    assert_eq!(code.len(), 0x5c);
    assert_eq!(
        format!("{:x}", Sha256::digest(&code)),
        "0af8694bc1f619cc24e1aa03c398e6d78cc3d9923e4a21f078c02803791ea40f"
    );
    assert_eq!(&code[0x22..0x26], &0x344d674u32.to_le_bytes());
    let registry = Box::new(0u32);
    code[0x22..0x26].copy_from_slice(&((&*registry as *const u32) as u32).to_le_bytes());
    unsafe {
        let page = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4);
        assert!(!page.is_null());
        std::ptr::copy_nonoverlapping(code.as_ptr(), page.cast(), code.len());
        let mut old = 0;
        assert_ne!(VirtualProtect(page, 4096, 0x20, &mut old), 0);
        assert_ne!(
            FlushInstructionCache(GetCurrentProcess(), page, code.len()),
            0
        );
        let native: unsafe extern "thiscall" fn(usize, u8) = std::mem::transmute(page);
        let f = Fixture::new();
        for expected in [0u8, 1, 0, 1] {
            native(f.widget.as_ptr() as usize, expected);
            assert_eq!(f.capture().unwrap().targets.len(), usize::from(expected));
            println!(
                "C29B_NATIVE_BMS visible={expected} captured={} native_mark_writes=0",
                f.capture().unwrap().targets.len()
            );
        }
        assert_ne!(VirtualFree(page, 0, 0x8000), 0);
    }
}
