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
    icon: Vec<u32>,
    wave: Vec<u32>,
    icon_data: Vec<u32>,
    wave_data: Vec<u32>,
    frame: Frame,
    people: Vec<(u32, u64, Role)>,
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self {
            hud: vec![0; 0x300 / 4],
            context: vec![0; 0x320 / 4],
            entries: vec![0; 0x50 / 4],
            pawn: vec![0; 0x150 / 4],
            icon: vec![0; 0x1b0 / 4],
            wave: vec![0; 0x1b0 / 4],
            icon_data: vec![0; 0x50 / 4],
            wave_data: vec![0; 0x50 / 4],
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
        put(&mut f.context, 0x310, f.frame.local as u32);
        put(&mut f.hud, 0x2ec, 1);
        put(&mut f.hud, 0x2ac, f.entries.as_ptr() as u32);
        put(&mut f.hud, 0x2b0, 1);
        put(&mut f.hud, 0x2b4, 2);
        put(&mut f.entries, 0, f.icon.as_ptr() as u32);
        put(&mut f.entries, 4, f.wave.as_ptr() as u32);
        put(&mut f.entries, 0x1c, f.pawn.as_ptr() as u32);
        put(&mut f.entries, 0x20, f.pawn.as_ptr() as u32);
        put(&mut f.pawn, 0, 0x2ad3234);
        put(&mut f.pawn, 0x144, f.frame.world as u32);
        put(&mut f.pawn, 0x20, 42);
        put(&mut f.icon, 0x1a8, f.icon_data.as_ptr() as u32);
        put(&mut f.wave, 0x1a8, f.wave_data.as_ptr() as u32);
        f
    }
    fn capture(&self) -> Option<Snapshot> {
        unsafe { capture(self.hud.as_ptr() as usize, &self.frame, &self.people) }
    }
}
#[test]
fn icon_and_wave_are_independent_native_results_not_distance_guesses() {
    let mut f = Fixture::new();
    for (icon, wave, count) in [(0, 0, 0), (1, 0, 1), (0, 1, 1), (1, 1, 1)] {
        put(&mut f.icon_data, 0x40, icon);
        put(&mut f.wave_data, 0x40, wave);
        assert_eq!(f.capture().unwrap().targets.len(), count);
    }
    put(&mut f.icon_data, 0x40, 0);
    put(&mut f.wave_data, 0x40, 0);
    f.frame.position = [0.0001, 0., 0.];
    assert!(f.capture().unwrap().targets.is_empty());
}
#[test]
fn fake_skill_warns_merc_only_and_genuine_goggles_still_expose_spy() {
    let mut f=Fixture::new();
    put(&mut f.wave_data,0x40,1);
    let before=(f.hud.clone(),f.pawn.clone(),f.entries.clone(),f.wave_data.clone());
    for ordinary in [false,true] {
        let s=unsafe {capture_with(f.hud.as_ptr() as usize,&f.frame,&f.people,|_| {
            crate::rfd_rules::display(true,true,ordinary)==crate::rfd_rules::Display::Interference
        })}.unwrap();
        assert!(s.enabled);
        assert_eq!(s.interference,!ordinary);
        assert_eq!(s.targets.len(),usize::from(ordinary));
    }
    assert_eq!(before,(f.hud.clone(),f.pawn.clone(),f.entries.clone(),f.wave_data.clone()));
    put(&mut f.hud,0x2ec,0);
    let s=unsafe {capture_with(f.hud.as_ptr() as usize,&f.frame,&f.people,|_|true)}.unwrap();
    assert!(!s.enabled && !s.interference && s.targets.is_empty());
    println!("C49_RFD fake_only_spy_warning=false merc_interference=true precise_goggles_spy_warning=true native_state_unchanged=true");
}
#[test]
fn pants_alone_without_native_acceptance_never_warn_and_mixed_results_keep_real_target() {
    let mut f=Fixture::new();
    let capture_fake=|f:&Fixture|unsafe {capture_with(f.hud.as_ptr() as usize,&f.frame,&f.people,|_|true)}.unwrap();
    assert!(!capture_fake(&f).interference);
    put(&mut f.wave_data,0x40,1);
    // A second native accepted Spy has a real unprotected RF source.
    let mut other=f.pawn.clone(); other[0x20/4]=44;
    let first=f.entries[..0x28/4].to_vec(); f.entries[0x28/4..].copy_from_slice(&first);
    put(&mut f.entries,0x28+0x20,other.as_ptr() as u32);
    put(&mut f.hud,0x2b0,2);
    f.people.push((44,777,Role::Spy));
    let s=unsafe {capture_with(f.hud.as_ptr() as usize,&f.frame,&f.people,|p|p==f.pawn.as_ptr() as usize)}.unwrap();
    assert!(s.interference);assert_eq!(s.targets,[777]);
}
#[test]
fn native_goggles_off_invalidates_old_visible_widgets() {
    let mut f = Fixture::new();
    put(&mut f.wave_data, 0x40, 1);
    put(&mut f.hud, 0x2ec, 0);
    assert!(f.capture().unwrap().targets.is_empty());
    put(&mut f.hud, 0x2ec, 2);
    assert!(f.capture().is_none());
}
#[test]
fn random_pants_scale_alpha_direction_are_not_a_detection_source() {
    let mut f = Fixture::new();
    for scale in [90f32, 100., 150.] {
        put(&mut f.wave_data, 0, scale.to_bits());
        put(&mut f.wave_data, 0x38, 70f32.to_bits());
        put(&mut f.wave_data, 0x10, 0.5f32.to_bits());
        assert!(f.capture().unwrap().targets.is_empty());
    }
    put(&mut f.wave_data, 0x40, 1);
    put(&mut f.wave_data, 0x38, 0f32.to_bits());
    assert_eq!(f.capture().unwrap().targets.len(), 1);
}
#[test]
fn capture_before_pants_hide_keeps_copied_native_result_but_not_next_frame() {
    let _guard = STATE_TEST.lock().unwrap();
    let mut f = Fixture::new();
    put(&mut f.icon_data, 0x40, 1);
    put(&mut f.wave_data, 0x40, 1);
    publish(f.capture());
    put(&mut f.icon_data, 0x40, 0);
    put(&mut f.wave_data, 0x40, 0);
    assert_eq!(snapshot(&f.frame).unwrap().targets, [0x1122334455667788]);
    f.frame.now += 1;
    assert!(snapshot(&f.frame).is_none());
    publish(f.capture());
    assert!(snapshot(&f.frame).unwrap().targets.is_empty());
    clear();
}
#[test]
fn wrong_hud_owner_world_or_nonplayer_entries_cannot_expose() {
    let mut f = Fixture::new();
    put(&mut f.wave_data, 0x40, 1);
    put(&mut f.hud, 0, HUD + 4);
    assert!(f.capture().is_none());
    put(&mut f.hud, 0, HUD);
    put(&mut f.context, 0x310, 0);
    assert!(f.capture().is_none());
    put(&mut f.context, 0x310, f.frame.local as u32);
    put(&mut f.pawn, 0x144, 0);
    assert!(f.capture().unwrap().targets.is_empty());
    put(&mut f.pawn, 0x144, f.frame.world as u32);
    put(&mut f.pawn, 0, 0x2ad2abc);
    assert!(f.capture().unwrap().targets.is_empty());
    put(&mut f.pawn, 0, 0x2ad3234);
    put(&mut f.entries, 0x20, 0);
    assert!(f.capture().unwrap().targets.is_empty());
}
#[test]
fn missing_ambiguous_or_wrong_role_roster_identity_fails_closed() {
    let mut f = Fixture::new();
    put(&mut f.wave_data, 0x40, 1);
    put(&mut f.pawn, 0x20, 999);
    assert!(f.capture().unwrap().targets.is_empty());
    put(&mut f.pawn, 0x20, 43);
    assert!(f.capture().unwrap().targets.is_empty());
    put(&mut f.pawn, 0x20, 42);
    f.people.push((42, 333, Role::Spy));
    assert!(f.capture().is_none());
}
#[test]
fn invalid_arrays_and_widget_values_are_not_positive() {
    let mut f = Fixture::new();
    for (n, cap) in [(129, 129), (2, 1), (1, 4097)] {
        put(&mut f.hud, 0x2b0, n);
        put(&mut f.hud, 0x2b4, cap);
        assert!(f.capture().is_none());
    }
    put(&mut f.hud, 0x2b0, 1);
    put(&mut f.hud, 0x2b4, 2);
    put(&mut f.wave_data, 0x40, 2);
    assert!(f.capture().unwrap().targets.is_empty());
    put(&mut f.entries, 0, 0);
    put(&mut f.entries, 4, 0);
    assert!(f.capture().unwrap().targets.is_empty());
}
#[test]
fn duplicates_coalesce_without_native_writes() {
    let mut f = Fixture::new();
    put(&mut f.wave_data, 0x40, 1);
    let first = f.entries[..10].to_vec();
    f.entries[10..20].copy_from_slice(&first);
    put(&mut f.hud, 0x2b0, 2);
    let before = vec![
        f.hud.clone(),
        f.context.clone(),
        f.entries.clone(),
        f.pawn.clone(),
        f.icon.clone(),
        f.wave.clone(),
        f.icon_data.clone(),
        f.wave_data.clone(),
    ];
    assert_eq!(f.capture().unwrap().targets.len(), 1);
    assert_eq!(
        before,
        vec![
            f.hud,
            f.context,
            f.entries,
            f.pawn,
            f.icon,
            f.wave,
            f.icon_data,
            f.wave_data
        ]
    );
}
#[test]
fn cached_source_never_crosses_scene_owner_time_or_teardown() {
    let _guard = STATE_TEST.lock().unwrap();
    let f = Fixture::new();
    publish(f.capture());
    let mut other = f.frame;
    other.world += 4;
    assert!(snapshot(&other).is_none());
    other = f.frame;
    other.local += 4;
    assert!(snapshot(&other).is_none());
    other = f.frame;
    other.now += 1;
    assert!(snapshot(&other).is_none());
    clear();
    assert!(snapshot(&f.frame).is_none());
}
#[test]
fn native_accepted_and_rejected_wave_blocks_drive_the_collector() {
    use sha2::{Digest, Sha256};
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, protect: u32) -> *mut c_void;
        fn VirtualProtect(p: *mut c_void, n: usize, protect: u32, old: *mut u32) -> i32;
        fn VirtualFree(p: *mut c_void, n: usize, t: u32) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
    }
    let root = std::path::PathBuf::from(
        std::env::var("SCBL_R58_C29C_FIXTURE").expect("C29C native fixture required"),
    );
    let whole = std::fs::read(root.join("contract-01a73860.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&whole)),
        "3c7af93c59a49fc895f0efde1135bbd58fca3e9e5ab68d972058221af366d045"
    );
    let mut setter = std::fs::read(root.join("contract-018a1c80.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&setter)),
        "0af8694bc1f619cc24e1aa03c398e6d78cc3d9923e4a21f078c02803791ea40f"
    );
    let registry = Box::new(0u32);
    assert_eq!(&setter[0x22..0x26], &0x344d674u32.to_le_bytes());
    setter[0x22..0x26].copy_from_slice(&((&*registry as *const u32) as u32).to_le_bytes());
    unsafe {
        let mut f = Fixture::new();
        let mut vt = vec![0u32; 0x110 / 4];
        for (start, end, expected) in [
            (0x1a74655usize, 0x1a74689usize, 1),
            (0x1a7468b, 0x1a746d9, 0),
            (0x1a74655, 0x1a74689, 1),
        ] {
            let p = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4) as usize;
            assert_ne!(p, 0);
            vt[0x108 / 4] = (p + 128) as u32;
            put(&mut f.wave, 0, vt.as_ptr() as u32);
            let mut code = vec![0x55, 0x89, 0xe5, 0x81, 0xec, 0x08, 0x02, 0, 0, 0xc7, 0x85];
            code.extend((-0x1b0i32).to_le_bytes());
            code.extend((f.hud.as_ptr() as u32).to_le_bytes());
            code.extend([0xc7, 0x45, 0xdc, 0, 0, 0, 0]);
            code.extend(&whole[start - 0x1a73860..end - 0x1a73860]);
            code.extend([0xc9, 0xc3]);
            assert!(code.len() < 128);
            std::ptr::copy_nonoverlapping(code.as_ptr(), p as _, code.len());
            std::ptr::copy_nonoverlapping(setter.as_ptr(), (p + 128) as _, setter.len());
            let mut old = 0;
            assert_ne!(VirtualProtect(p as _, 4096, 0x20, &mut old), 0);
            assert_ne!(FlushInstructionCache(GetCurrentProcess(), p as _, 4096), 0);
            std::mem::transmute::<usize, unsafe extern "C" fn()>(p)();
            assert_eq!(f.capture().unwrap().targets.len(), expected);
            println!("C29C_NATIVE_RFD native_wave={expected} captured={expected} fake_pants_writes=0 mark_writes=0");
            assert_ne!(VirtualFree(p as _, 0, 0x8000), 0);
        }
    }
}
