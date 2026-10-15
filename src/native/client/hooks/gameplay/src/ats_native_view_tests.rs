use super::*;
#[derive(Default)]
struct Mock {
    next: usize,
    created: usize,
    deleted: Vec<usize>,
    started: usize,
    faded: usize,
    notified: usize,
    updates: Vec<(usize, [f32; 3])>,
    fail: bool,
}
impl Surface for Mock {
    type Handle = usize;
    fn create(&mut self, _: usize) -> Option<usize> {
        if self.fail {
            return None;
        }
        self.next += 1;
        self.created += 1;
        Some(self.next)
    }
    fn restart(&mut self, _: &mut usize) {
        self.started += 1;
    }
    fn update(&mut self, _: usize, h: &mut usize, _: &Frame, m: &Mark) {
        self.updates.push((*h, m.position));
    }
    fn fade(&mut self, _: &mut usize) {
        self.faded += 1;
    }
    fn destroy(&mut self, h: usize) {
        assert!(!self.deleted.contains(&h));
        self.deleted.push(h);
    }
    fn notify(&mut self) {
        self.notified += 1;
    }
}
fn frame(now: u64) -> Frame {
    Frame {
        world: 1,
        info: 2,
        local: 3,
        team: 20,
        now,
        position: [0.; 3],
        distance_delay_ms: 1000,
        visible: true,
    }
}
fn mark() -> Mark {
    Mark {
        source: 55,
        serial: 1,
        position: [90., 20., 30.],
        shot_time_ms: 100,
        expires_ms: 7600,
    }
}
#[test]
fn copied_mark_retains_native_duration_and_never_follows_actor() {
    let (mut v, mut s) = (View::default(), Mock::default());
    let m = mark();
    for now in [100, 200, 5101, 7599] {
        v.frame(&mut s, 44, Some(&frame(now)), &[m]);
    }
    assert_eq!((s.created, s.notified, s.started, s.faded), (1, 1, 1, 0));
    assert!(s.updates.iter().all(|(_, p)| *p == m.position));
    v.frame(&mut s, 44, Some(&frame(7600)), &[]);
    assert_eq!(s.faded, 1);
    assert!(s.deleted.is_empty());
    v.frame(&mut s, 44, Some(&frame(8099)), &[]);
    assert!(s.deleted.is_empty());
    v.frame(&mut s, 44, Some(&frame(8100)), &[]);
    assert_eq!(s.deleted, vec![1]);
    assert!(v.items.is_empty());
    println!("C28B_VIEW fixed_x=90 native_expiry_ms=7600 fade_end_ms=8100 native_UI_handles_only=true rendered_game=false");
}
#[test]
fn real_new_shot_refreshes_position_without_duplicate_widget_or_notification() {
    let (mut v, mut s) = (View::default(), Mock::default());
    let mut m = mark();
    v.frame(&mut s, 44, Some(&frame(100)), &[m]);
    m.serial = 2;
    m.position = [400., 0., 0.];
    m.shot_time_ms = 1000;
    m.expires_ms = 8500;
    v.frame(&mut s, 44, Some(&frame(1000)), &[m]);
    assert_eq!((s.created, s.notified, s.started), (1, 1, 1));
    assert_eq!(s.updates.last().unwrap().1, [400., 0., 0.]);
    v.frame(&mut s, 44, Some(&frame(8500)), &[]);
    assert_eq!(s.faded, 1);
    m.serial = 3;
    m.shot_time_ms = 8600;
    m.expires_ms = 16100;
    v.frame(&mut s, 44, Some(&frame(8600)), &[m]);
    assert_eq!((s.created, s.started), (1, 2));
    assert!(v.items[&55].fade_end.is_none());
}
#[test]
fn no_frame_death_scene_team_hud_change_and_clock_rollback_clear_owned_only() {
    for cause in 0..6 {
        let (mut v, mut s) = (View::default(), Mock::default());
        let m = mark();
        v.frame(&mut s, 44, Some(&frame(100)), &[m]);
        let mut f = frame(200);
        let mut hud = 44;
        match cause {
            0 => {}
            1 => f.world += 1,
            2 => f.team += 1,
            3 => f.local += 1,
            4 => f.now = 50,
            5 => hud = 45,
            _ => unreachable!(),
        }
        v.frame(&mut s, hud, if cause == 0 { None } else { Some(&f) }, &[]);
        assert_eq!(s.deleted, vec![1]);
        assert!(v.items.is_empty());
    }
}
#[test]
fn missing_asset_invalid_events_and_hidden_hud_fail_closed() {
    let (mut v, mut s) = (View::default(), Mock::default());
    s.fail = true;
    v.frame(&mut s, 44, Some(&frame(100)), &[mark()]);
    assert!(v.items.is_empty());
    assert_eq!(s.notified, 0);
    s.fail = false;
    let mut hidden = frame(100);
    hidden.visible = false;
    v.frame(&mut s, 44, Some(&hidden), &[mark()]);
    assert_eq!(s.created, 0);
    for bad in 0..4 {
        let mut m = mark();
        match bad {
            0 => m.source = 0,
            1 => m.position[0] = f32::NAN,
            2 => m.shot_time_ms = 101,
            3 => m.expires_ms = 100,
            _ => {}
        }
        v.frame(&mut s, 44, Some(&frame(100)), &[m]);
    }
    assert_eq!(s.created, 0);
}
#[test]
fn multiple_sources_are_independent_and_clear_is_idempotent() {
    let (mut v, mut s) = (View::default(), Mock::default());
    let a = mark();
    let mut b = a;
    b.source = 56;
    b.expires_ms = 9000;
    v.frame(&mut s, 44, Some(&frame(100)), &[a, b]);
    v.frame(&mut s, 44, Some(&frame(7600)), &[b]);
    assert_eq!((s.created, s.faded, s.notified), (2, 1, 2));
    v.frame(&mut s, 44, Some(&frame(8100)), &[b]);
    assert_eq!(s.deleted, vec![1]);
    v.clear(&mut s);
    v.clear(&mut s);
    assert_eq!(s.deleted, vec![1, 2]);
}
#[path = "ats_native_ui_tests.rs"]
mod backend;

#[test]
fn ats_uses_native_activation_not_wrapper_visibility_and_rejects_foreign_hud() {
    unsafe {
        let mut hud = vec![0u32; 0x300 / 4];
        let mut ctx = vec![0u32; 0x314 / 4];
        let hp = hud.as_mut_ptr() as usize;
        let f = frame(100);
        hud[0] = 0x2c3e85c;
        hud[0x25c / 4] = ctx.as_mut_ptr() as u32;
        ctx[0x310 / 4] = f.local as u32;
        for wrapper in [0u8, 1] {
            *((hp + 0x231) as *mut u8) = wrapper;
            for active in [0u8, 1, 2, 255] {
                *((hp + 0x2f8) as *mut u8) = active;
                assert_eq!(hud_visible(hp, &f), active == 1);
            }
        }
        *((hp + 0x2f8) as *mut u8) = 1;
        ctx[0x310 / 4] += 1;
        assert!(!hud_visible(hp, &f));
        ctx[0x310 / 4] = f.local as u32;
        hud[0] = 0;
        assert!(!hud_visible(hp, &f));
        assert!(!hud_visible(0, &f));
    }
}

// Execute the unchanged retail ATS activation setter, not a mocked visibility bit.
#[test]
fn retail_ats_activation_opens_fixed_position_view() {
    use sha2::{Digest, Sha256};
    use std::ffi::c_void;
    extern "system" {
        fn VirtualAlloc(p: *mut c_void, n: usize, kind: u32, protect: u32) -> *mut c_void;
        fn VirtualProtect(p: *mut c_void, n: usize, protect: u32, old: *mut u32) -> i32;
        fn FlushInstructionCache(p: *mut c_void, code: *const c_void, n: usize) -> i32;
        fn VirtualFree(p: *mut c_void, n: usize, kind: u32) -> i32;
    }
    unsafe {
        let path = std::path::PathBuf::from(std::env::var("SCBL_R58_C56_FIXTURE").unwrap());
        let raw = std::fs::read(path.join("native-ats-activation.bin")).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&raw)),
            "2ddc1333f419413a97fe772931d4c2a65c6ac0d8619fa2fbfa6b02515f7b00cb"
        );
        let code = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4);
        assert!(!code.is_null());
        std::ptr::copy_nonoverlapping(raw.as_ptr(), code.cast(), raw.len());
        let mut old = 0;
        assert_ne!(VirtualProtect(code, 4096, 0x20, &mut old), 0);
        assert_ne!(FlushInstructionCache(-1isize as _, code, raw.len()), 0);
        let set: unsafe extern "thiscall" fn(usize, u8) = std::mem::transmute(code);
        let mut hud = vec![0u32; 0x300 / 4];
        let mut ctx = vec![0u32; 0x314 / 4];
        let hp = hud.as_mut_ptr() as usize;
        let f = frame(100);
        hud[0] = 0x2c3e85c;
        hud[0x25c / 4] = ctx.as_mut_ptr() as u32;
        ctx[0x310 / 4] = f.local as u32;
        for active in [0, 1, 0, 1] {
            set(hp, active);
            assert_eq!(*((hp + 0x2f8) as *const u8), active);
            assert_eq!(*((hp + 0x231) as *const u8), 0);
            let visible = hud_visible(hp, &f);
            let mut f = f;
            f.visible = visible;
            let (mut view, mut surface) = (View::default(), Mock::default());
            view.frame(&mut surface, hp, Some(&f), &[mark()]);
            println!("C56_HUD_REPLAY native_active={} wrapper231=0 visible={} created={} updates={} retail_setter=true game_pixels=false", active, visible, surface.created, surface.updates.len());
            assert_eq!(visible, active == 1);
            assert_eq!(surface.created, usize::from(active == 1));
        }
        assert_ne!(VirtualFree(code, 0, 0x8000), 0);
    }
}
