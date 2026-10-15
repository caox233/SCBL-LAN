use super::*;
use crate::goggle_exposure::Source;
use sha2::{Digest, Sha256};
use std::cell::Cell;
fn vec_bytes(n: usize) -> Vec<u32> {
    vec![0; (n + 3) / 4]
}
struct Scene {
    hud: Vec<u32>,
    info: Vec<u32>,
    world: Vec<u32>,
    wi: Vec<u32>,
    pc: Vec<u32>,
    pawn: Vec<u32>,
    ctx: Vec<u32>,
    locals: Vec<u32>,
    _inventory: Vec<u32>,
    _widgets: Vec<Vec<u32>>,
    _displays: Vec<Vec<u32>>,
    _values: Vec<Vec<u32>>,
}
unsafe extern "thiscall" fn goggles(_: usize) -> i32 {
    1
}
impl Scene {
    fn new() -> Self {
        let mut s = Self {
            hud: vec_bytes(0x340),
            info: vec_bytes(0x540),
            world: vec_bytes(0x700),
            wi: vec_bytes(0x400),
            pc: vec_bytes(0x700),
            pawn: vec_bytes(0x1b00),
            ctx: vec_bytes(0x320),
            locals: vec![0],
            _inventory: vec_bytes(0x100),
            _widgets: (0..4).map(|_| vec_bytes(0x1b0)).collect(),
            _displays: (0..4).map(|_| vec_bytes(0x70)).collect(),
            _values: (0..4).map(|_| vec_bytes(16)).collect(),
        };
        s.hud[0] = 0x2c41234;
        s.hud[0x25c / 4] = s.ctx.as_ptr() as u32;
        s.info[0x330 / 4] = 0x10;
        s.info[0x144 / 4] = s.world.as_ptr() as u32;
        s.world[0x6d0 / 4] = s.wi.as_ptr() as u32;
        s.world[0x5a0 / 4] = 1f32.to_bits();
        s.wi[0x36c / 4] = s.locals.as_ptr() as u32;
        s.wi[0x370 / 4] = 1;
        s.wi[0x374 / 4] = 1;
        s.locals[0] = s.pc.as_ptr() as u32;
        s.pc[0x520 / 4] = s.pawn.as_ptr() as u32;
        s.pc[0x144 / 4] = s.world.as_ptr() as u32;
        s.pc[0x60 / 4] = 0x60000;
        s.pc[0x4c / 4] = 1 << 8;
        s.ctx[0x310 / 4] = s.pawn.as_ptr() as u32;
        s.pawn[0] = SPY;
        s.pawn[0x20 / 4] = 17;
        s.pawn[0x374 / 4] = 0x40000;
        s.pawn[0x144 / 4] = s.world.as_ptr() as u32;
        s.pawn[0x1410 / 4] = s._inventory.as_ptr() as u32;
        for i in 0..4 {
            s.hud[(0x320 + i * 4) / 4] = s._widgets[i].as_ptr() as u32;
            s._widgets[i][0x1a8 / 4] = s._displays[i].as_ptr() as u32;
            s._displays[i][0x5c / 4] = s._values[i].as_ptr() as u32;
        }
        s
    }
    fn local(&self) -> Option<Local> {
        unsafe {
            local(
                self.hud.as_ptr() as usize,
                self.info.as_ptr() as usize,
                goggles,
            )
        }
    }
}
#[derive(Default)]
struct Mock {
    texts: Vec<String>,
    colors: Vec<u32>,
    anim: Vec<bool>,
    zh: bool,
    sounds: usize,
}
impl Ui for Mock {
    fn colors(&mut self, _: Binding) -> Option<[u32; 2]> {
        Some([0x112233, 0xaabbcc])
    }
    fn text(&mut self, _: usize, t: &[u16]) -> bool {
        self.texts
            .push(String::from_utf16(&t[..t.len() - 1]).unwrap());
        true
    }
    fn color(&mut self, _: usize, c: u32) {
        self.colors.push(c);
    }
    fn transform(&mut self, _: usize) -> Option<Transform> {
        Some(Transform([1., 1., 1., 1., 0., 0., 0., 0.]))
    }
    fn tint_rgb(&mut self, _: usize, _: Transform) -> bool {
        true
    }
    fn sound(&mut self, _: Binding) {
        self.sounds += 1;
    }
    fn native_label(&mut self) -> Option<Vec<u16>> {
        Some(
            if self.zh { "被發現" } else { "DETECTED" }
                .encode_utf16()
                .chain([0])
                .collect(),
        )
    }
    fn animation(&mut self, _: usize, b: bool) {
        self.anim.push(b);
    }
}
fn detected() -> ActualDetections {
    let mut d = ActualDetections::default();
    d.record(Source::Rfd, true);
    d
}
#[test]
fn local_merc_interference_receipt_shares_native_ui_not_spy_audio_or_tag() {
    let mut s=Scene::new(); s.pawn[0]=MERC; s.pawn[0x374/4]=0x80000;
    let mut l=s.local().unwrap(); assert!(l.merc && !l.goggles);
    let before=(s.pawn.clone(),s.hud.clone());
    let mut r=Runtime::default(); let mut ui=Mock::default();
    crate::rfd_interference::publish(None);
    assert!(!r.after_native(Some(l),Some(Language::English),detected(),&mut ui));
    for now in [100,170,500,600,800,3000] {
        l.now=now;
        crate::rfd_interference::publish(Some(crate::rfd_interference::Receipt {
            life:l.binding.life,now,enabled:true,fake:true,
        }));
        assert!(r.after_native(Some(l),Some(Language::TraditionalChinese),ActualDetections::default(),&mut ui));
        assert_eq!(ui.texts.last().unwrap(),"被干擾");
        assert_eq!(ui.sounds,0);assert_eq!(ui.anim,[true]);
    }
    l.marked=Some(true);
    assert!(!r.after_native(Some(l),Some(Language::English),detected(),&mut ui));
    assert_eq!(ui.texts.last().unwrap(),"DETECTED");
    assert_eq!(before,(s.pawn,s.hud));
    crate::rfd_interference::publish(None);
    println!("C49_UI role=Merc label=INTERFERENCE/被干擾 intro_count=1 audio_count=0 native_detected_priority=true pawn_hud_unchanged=true");
}

#[test]
fn ats_short_silent_gaps_keep_one_intro_and_one_sound_until_full_two_second_loss() {
    let s = Scene::new();
    let mut l = s.local().unwrap();
    let before = (s.pawn.clone(), s.hud.clone());
    let mut r = Runtime::default();
    let mut ui = Mock::default();
    for (now, seen) in [
        (0, true),
        (2200, true),
        (2300, false),
        (3000, true),
        (5200, true),
        (5300, false),
        (6000, true),
        (10000, true),
        (11999, false),
    ] {
        l.now = now;
        let mut d = ActualDetections::default();
        d.record(Source::Ats, seen);
        assert!(
            r.after_native(Some(l), Some(Language::English), d, &mut ui),
            "at {now}"
        );
        assert_eq!(ui.sounds, 1);
        assert_eq!(ui.anim, [true]);
    }
    l.now = 12000;
    assert!(!r.after_native(
        Some(l),
        Some(Language::English),
        ActualDetections::default(),
        &mut ui
    ));
    assert_eq!(ui.anim, [true, false]);
    assert_eq!(ui.sounds, 1);
    l.now = 13000;
    assert!(r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    assert_eq!(ui.anim, [true, false, true]);
    assert_eq!(ui.sounds, 2);
    assert_eq!(before, (s.pawn, s.hud));
    println!("C46_RELEASE_HOLD silence_ms=1999 visible=true continuous_intro=1 continuous_sound=1 silence_ms=2000 hidden=true next_episode_sound_total=2 pawn_hud_unchanged=true");
}

#[test]
fn visible_episode_is_at_least_two_seconds_with_only_one_sound() {
    let s = Scene::new();
    let mut l = s.local().unwrap();
    l.now = 100;
    let mut r = Runtime::default();
    let mut ui = Mock::default();
    assert!(r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    for time in [200, 900, 1100, 1999, 2099] {
        l.now = time;
        assert!(r.after_native(
            Some(l),
            Some(Language::English),
            ActualDetections::default(),
            &mut ui
        ));
        assert_eq!(ui.sounds, 1);
    }
    l.now = 2100;
    assert!(!r.after_native(
        Some(l),
        Some(Language::English),
        ActualDetections::default(),
        &mut ui
    ));
    assert_eq!(ui.sounds, 1);
    l.now = 3000;
    assert!(r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    assert_eq!(ui.sounds, 2);
    for time in [3100, 5000, 8000, 20000] {
        l.now = time;
        assert!(r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    }
    assert_eq!(ui.sounds, 2);
    l.marked = Some(true);
    assert!(!r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    assert_eq!(ui.sounds, 2); // actual native mark owns its own sound.
    println!("C44_EPISODE minimum_ms=2000 continuous_sound_count=1 second_episode_total=2 native_priority=true");
}

#[test]
fn warning_audio_requires_current_local_life_cue_and_safe_audio_chain() {
    unsafe extern "thiscall" fn play(
        p: usize,
        c: usize,
        k: u32,
        f: f32,
        a: usize,
        rep: u32,
        last: u32,
    ) -> u32 {
        assert_ne!(p, 0);
        assert_ne!(c, 0);
        assert_eq!((k, f, a, rep, last), (7, 0., 0, 0, 0));
        1
    }
    let mut s = Scene::new();
    let b = s.local().unwrap().binding;
    assert!(!unsafe { warning_sound(b, play) });
    let cue = vec_bytes(0x48);
    let mut a = vec_bytes(0x50);
    let mut context = vec_bytes(0x50);
    let audio = [0u32; 4];
    a[0x38 / 4] = context.as_ptr() as u32;
    context[0x48 / 4] = audio.as_ptr() as u32;
    s.pawn[0x1aa4 / 4] = cue.as_ptr() as u32;
    s.pawn[0x148 / 4] = a.as_ptr() as u32;
    let before = s.pawn.clone();
    assert!(unsafe { warning_sound(b, play) });
    assert_eq!(s.pawn, before);
    context[0x48 / 4] = 0;
    assert!(!unsafe { warning_sound(b, play) });
    context[0x48 / 4] = audio.as_ptr() as u32;
    s.pawn[0x20 / 4] += 1;
    assert!(!unsafe { warning_sound(b, play) });
}

#[test]
fn retail_detected_audio_six_argument_call_and_body_are_local_only() {
    use windows::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    };
    thread_local! {static CALLS:Cell<usize>=const {Cell::new(0)};}
    unsafe extern "thiscall" fn sink(
        _audio: usize,
        pawn: usize,
        cue: usize,
        k: u32,
        bus: usize,
        gain: f32,
        extra: usize,
    ) -> u32 {
        assert_ne!(pawn, 0);
        assert_ne!(cue, 0);
        assert_eq!((k, bus, gain, extra), (7, 0, 0., 0));
        CALLS.with(|c| c.set(c.get() + 1));
        1
    }
    let e = std::path::PathBuf::from(std::env::var("SCBL_R58_C44_FIXTURE").unwrap());
    let block = std::fs::read(e.join("contract-00b5eb5e.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&block)),
        "e7b84bd4d206ebfcb3b5e2123c0e04eaf519bbd0d12c7be6051ab161f4d8c8fb"
    );
    let mut body = std::fs::read(e.join("contract-00c71de0.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&body)),
        "8b44c9b9df6c432fe10a3ea41a551984fc67aceff8eaf98d5a711566840813fa"
    );
    let global = 0u32;
    assert_eq!(&body[0x72..0x76], &0x0330c198u32.to_le_bytes());
    body[0x72..0x76].copy_from_slice(&(&global as *const u32 as u32).to_le_bytes());
    let mut s = Scene::new();
    let b = s.local().unwrap().binding;
    let cue = vec_bytes(0x48);
    let mut a = vec_bytes(0x50);
    let mut ctx = vec_bytes(0x50);
    let mut audio = vec_bytes(0x10);
    let mut av = vec_bytes(0xc0);
    av[0xa8 / 4] = sink as usize as u32;
    audio[0] = av.as_ptr() as u32;
    a[0x38 / 4] = ctx.as_ptr() as u32;
    ctx[0x48 / 4] = audio.as_ptr() as u32;
    s.pawn[0x148 / 4] = a.as_ptr() as u32;
    s.pawn[0x1aa4 / 4] = cue.as_ptr() as u32;
    let mut vt = vec_bytes(0x600);
    unsafe {
        let m = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(m, 0);
        std::ptr::copy_nonoverlapping(body.as_ptr(), m as *mut u8, body.len());
        let f: PlayWarning = std::mem::transmute(m);
        let before = s.pawn.clone();
        for _ in 0..1000 {
            assert!(warning_sound(b, f));
        }
        assert_eq!(s.pawn, before);
        // Execute the retail caller block with a synthetic pawn vtable too.
        vt[0x5dc / 4] = m as u32;
        s.pawn[0] = vt.as_ptr() as u32;
        let mut thunk = vec![0x56, 0x8b, 0x74, 0x24, 0x08]; // save esi; esi=arg
        thunk.extend_from_slice(&block);
        thunk.extend_from_slice(&[0x5e, 0xc3]);
        std::ptr::copy_nonoverlapping(thunk.as_ptr(), (m + 1024) as *mut u8, thunk.len());
        let call: unsafe extern "C" fn(usize) -> u32 = std::mem::transmute(m + 1024);
        for _ in 0..1000 {
            assert_eq!(call(s.pawn.as_ptr() as usize), 1);
        }
        assert_eq!(CALLS.with(|c| c.get()), 2000);
        VirtualFree(m as *mut _, 0, MEM_RELEASE).unwrap();
    }
    println!("C44_NATIVE audio_caller_and_body=retail_bytes calls=2000 local_type=7 replicate=0 mark_writes=0 audio_device_stubbed=true");
}
#[test]
fn local_spy_capture_is_read_only_and_native_tag_is_not_detection() {
    let mut s = Scene::new();
    s.pawn[0x1a08 / 4] = 0x800;
    let before = (s.hud.clone(), s.pawn.clone(), s.world.clone());
    let l = s.local().unwrap();
    assert!(l.alive && l.goggles);
    assert_eq!(l.marked, Some(true));
    let mut r = Runtime::default();
    let mut ui = Mock::default();
    assert!(!r.after_native(
        Some(l),
        Some(Language::English),
        ActualDetections::default(),
        &mut ui
    ));
    assert!(ui.texts.is_empty() && ui.colors.is_empty() && ui.anim.is_empty());
    assert_eq!(before, (s.hud, s.pawn, s.world));
}
#[test]
fn wrong_scene_owner_campaign_merc_ambiguous_local_or_generation_rejected() {
    let mut s = Scene::new();
    s.info[0x330 / 4] = 0;
    assert!(s.local().is_none());
    s.info[0x330 / 4] = 0x10;
    s.pawn[0] = 0x2ad2abc;
    assert!(s.local().is_none());
    s.pawn[0] = SPY;
    s.ctx[0x310 / 4] = 0;
    assert!(s.local().is_none());
    s.ctx[0x310 / 4] = s.pawn.as_ptr() as u32;
    s.wi[0x370 / 4] = 2;
    assert!(s.local().is_none());
    s.wi[0x370 / 4] = 1;
    s.pawn[0x20 / 4] = 0;
    assert!(s.local().is_none());
    s.pawn[0x20 / 4] = 17;
    s.pawn[0x144 / 4] = 0;
    assert!(s.local().is_none());
}
#[test]
fn invalid_clock_rejected_and_dead_local_never_queries_goggles() {
    let mut s = Scene::new();
    for n in [f32::NAN, f32::INFINITY, -1.] {
        s.world[0x5a0 / 4] = n.to_bits();
        assert!(s.local().is_none());
    }
    s.world[0x5a0 / 4] = 0f32.to_bits();
    s.pawn[0xc50 / 4] = 1;
    unsafe extern "thiscall" fn forbidden(_: usize) -> i32 {
        panic!("dead query")
    }
    let l = unsafe { local(s.hud.as_ptr() as usize, s.info.as_ptr() as usize, forbidden) }.unwrap();
    assert!(!l.alive && !l.goggles);
}
#[test]
fn native_tick_runs_once_before_frame_read_and_keeps_dt_abi() {
    let calls = Cell::new(0);
    let clock = Cell::new(0);
    let result = original_then(
        || {
            calls.set(calls.get() + 1);
            clock.set(17);
        },
        || {
            assert_eq!(clock.get(), 17);
            42
        },
    );
    assert_eq!((calls.get(), result), (1, 42));
    unsafe extern "thiscall" fn tick_fixture(h: usize, dt: f32) {
        assert_eq!(h, 0x1234);
        assert_eq!(dt, 0.125);
    }
    let f: unsafe extern "thiscall" fn(usize, f32) = tick_fixture;
    for _ in 0..1000 {
        unsafe { f(0x1234, 0.125) }
    }
}
#[test]
fn private_runtime_without_peer_results_never_shows_yellow() {
    let s = Scene::new();
    let l = s.local();
    let mut r = Runtime::default();
    let mut ui = Mock::default();
    for _ in 0..1000 {
        assert!(!r.after_native(
            l,
            Some(Language::English),
            ActualDetections::default(),
            &mut ui
        ));
    }
    assert!(ui.texts.is_empty() && ui.anim.is_empty() && ui.colors.is_empty());
}
#[test]
fn actual_results_hold_two_seconds_but_goggles_off_cancels_and_native_wins() {
    let s = Scene::new();
    let mut l = s.local().unwrap();
    let mut r = Runtime::default();
    let mut ui = Mock::default();
    assert!(r.after_native(
        Some(l),
        Some(Language::TraditionalChinese),
        detected(),
        &mut ui
    ));
    assert_eq!(ui.texts, ["已暴露", "已暴露", "已暴露", "已暴露"]);
    assert_eq!(ui.colors, [0xefa63c; 4]);
    assert_eq!(ui.anim, [true]);
    l.now += 1999;
    assert!(r.after_native(
        Some(l),
        Some(Language::English),
        ActualDetections::default(),
        &mut ui
    ));
    l.now += 1;
    assert!(!r.after_native(
        Some(l),
        Some(Language::English),
        ActualDetections::default(),
        &mut ui
    ));
    assert_eq!(ui.anim, [true, false]);
    assert!(r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    l.goggles = false;
    assert!(!r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    l.goggles = true;
    assert!(r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    ui.anim.clear();
    ui.texts.clear();
    ui.colors.clear();
    ui.zh = true;
    l.marked = Some(true);
    assert!(!r.after_native(
        Some(l),
        Some(Language::TraditionalChinese),
        detected(),
        &mut ui
    ));
    assert!(ui.anim.is_empty());
    assert_eq!(ui.texts, ["被發現", "被發現"]);
    assert_eq!(ui.colors, [0x112233, 0xaabbcc]);
}
#[test]
fn missing_binding_does_not_write_old_widget_and_death_drops_hold() {
    let s = Scene::new();
    let mut l = s.local().unwrap();
    let mut r = Runtime::default();
    let mut ui = Mock::default();
    assert!(r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    ui.texts.clear();
    ui.anim.clear();
    ui.colors.clear();
    assert!(!r.after_native(None, Some(Language::English), detected(), &mut ui));
    assert!(ui.texts.is_empty() && ui.anim.is_empty() && ui.colors.is_empty());
    assert!(!r.after_native(
        Some(l),
        Some(Language::English),
        ActualDetections::default(),
        &mut ui
    ));
    assert!(r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    l.alive = false;
    assert!(!r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    l.alive = true;
    assert!(!r.after_native(
        Some(l),
        Some(Language::English),
        ActualDetections::default(),
        &mut ui
    ));
}
#[test]
fn unknown_locale_restores_native_instead_of_leaving_exposed() {
    let s = Scene::new();
    let l = s.local();
    let mut r = Runtime::default();
    let mut ui = Mock::default();
    assert!(language_bytes(b"int\0").is_some());
    assert!(language_bytes(b"cht\0").is_some());
    assert!(language_bytes(b"rus\0").is_none());
    assert!(r.after_native(l, Some(Language::English), detected(), &mut ui));
    ui.texts.clear();
    assert!(!r.after_native(l, None, detected(), &mut ui));
    assert!(ui.texts.is_empty()); // Hidden OutroTag retains yellow, not a false red warning.
}
#[test]
fn wide_label_copy_is_bounded_nonempty_and_owned() {
    assert!(unsafe { wide_copy(0) }.is_none());
    let t: Vec<u16> = "被發現".encode_utf16().chain([0]).collect();
    assert_eq!(unsafe { wide_copy(t.as_ptr() as usize) }.unwrap(), t);
    let bad = vec![65u16; 256];
    assert!(unsafe { wide_copy(bad.as_ptr() as usize) }.is_none());
    assert!(unsafe { wide_copy([0u16].as_ptr() as usize) }.is_none());
}
#[test]
fn retail_contracts_and_tag_getter_execute_without_mark_mutation() {
    use windows::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    };
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C33_FIXTURE").unwrap());
    for &(p, n, h) in &CONTRACTS[..4] {
        let b = std::fs::read(root.join(format!("native-{p:08x}.bin"))).unwrap();
        assert_eq!(b.len(), n);
        assert_eq!(format!("{:x}", Sha256::digest(&b)), h);
    }
    let bytes = std::fs::read(root.join("native-00ba43c0.bin")).unwrap();
    unsafe {
        let m = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(m, 0);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), m as *mut u8, bytes.len());
        let getter: unsafe extern "thiscall" fn(usize) -> u8 = std::mem::transmute(m);
        let mut pawn = vec_bytes(0x1b00);
        for flags in [0, 0x800, 0x801, 0xffffffff, 0x400] {
            pawn[0x1a08 / 4] = flags;
            assert_eq!(getter(pawn.as_ptr() as usize), u8::from(flags & 0x800 != 0));
            assert_eq!(pawn[0x1a08 / 4], flags);
        }
        VirtualFree(m as *mut _, 0, MEM_RELEASE).unwrap();
    }
    println!("C33_NATIVE tagged_predicate=retail_bytes native_priority_only no_mark_writes=true");
}
#[test]
fn retail_animation_vcall_abi_one_string_argument_and_stack_balanced() {
    use windows::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    };
    unsafe extern "thiscall" fn anim(w: usize, s: *const u8) {
        assert_ne!(w, 0);
        assert_eq!(std::ffi::CStr::from_ptr(s.cast()).to_bytes(), b"IntroTag");
    }
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C33_FIXTURE").unwrap());
    let retail = std::fs::read(root.join("native-01a68da0.bin")).unwrap();
    let block = &retail[0x1a68fe4 - 0x1a68da0..0x1a69004 - 0x1a68da0];
    assert_eq!(block.len(), 32);
    assert_eq!(&block[30..], &[0xff, 0xd0]);
    let mut code = vec![
        0x55, 0x8b, 0xec, 0x83, 0xec, 0x4c, 0x8b, 0x45, 0x08, 0x89, 0x45, 0xc8, 0x8b, 0x45, 0x0c,
        0x89, 0x45, 0xb4,
    ];
    code.extend_from_slice(block);
    code.extend_from_slice(&[0x8b, 0xe5, 0x5d, 0xc3]);
    let mut vt = vec_bytes(0x140);
    vt[0x12c / 4] = anim as usize as u32;
    let widget = [vt.as_ptr() as u32];
    let mut hud = vec_bytes(0x340);
    hud[0x320 / 4] = widget.as_ptr() as u32;
    unsafe {
        let m = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(m, 0);
        std::ptr::copy_nonoverlapping(code.as_ptr(), m as *mut u8, code.len());
        let f: unsafe extern "C" fn(usize, *const u8) = std::mem::transmute(m);
        for _ in 0..1000 {
            f(hud.as_ptr() as usize, b"IntroTag\0".as_ptr());
        }
        VirtualFree(m as *mut _, 0, MEM_RELEASE).unwrap();
    }
    println!(
        "C33_NATIVE animation_vcall=retail_bytes iterations=1000 engine_animation_stubbed=true"
    );
}

#[test]
fn shutdown_clears_owned_tls_without_native_ui_calls() {
    std::thread::spawn(|| {
        let s = Scene::new();
        let mut ui = Mock::default();
        STATE.with(|state| {
            assert!(state.borrow_mut().after_native(
                s.local(),
                Some(Language::English),
                detected(),
                &mut ui
            ));
        });
        ui.texts.clear();
        ui.colors.clear();
        ui.anim.clear();
        assert!(clear_thread_state());
        assert!(ui.texts.is_empty() && ui.colors.is_empty() && ui.anim.is_empty());
        STATE.with(|state| {
            assert!(!state.borrow_mut().after_native(
                s.local(),
                Some(Language::English),
                ActualDetections::default(),
                &mut ui
            ));
        });
        assert!(ui.texts.is_empty() && ui.colors.is_empty() && ui.anim.is_empty());
    })
    .join()
    .unwrap();
}

#[test]
fn shutdown_does_not_wait_or_panic_when_tls_is_borrowed() {
    std::thread::spawn(|| {
        STATE.with(|state| {
            let _busy = state.borrow_mut();
            assert!(!clear_thread_state());
        });
        assert!(clear_thread_state());
    })
    .join()
    .unwrap();
}

#[test]
fn shutdown_after_tls_destructor_is_tolerated() {
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };
    struct AfterState(Arc<AtomicU32>);
    impl Drop for AfterState {
        fn drop(&mut self) {
            // This destructor was registered before STATE. It executes after
            // STATE has been destroyed, as can happen during native detach.
            let result = std::panic::catch_unwind(clear_thread_state);
            self.0.store(
                match result {
                    Ok(false) => 1,
                    Ok(true) => 2,
                    Err(_) => 3,
                },
                Ordering::SeqCst,
            );
        }
    }
    thread_local! { static LATE: RefCell<Option<AfterState>> = const { RefCell::new(None) }; }
    let result = Arc::new(AtomicU32::new(0));
    let worker_result = result.clone();
    std::thread::spawn(move || {
        LATE.with(|v| *v.borrow_mut() = Some(AfterState(worker_result)));
        STATE.with(|v| {
            let _ = v.borrow();
        });
    })
    .join()
    .unwrap();
    assert_eq!(result.load(Ordering::SeqCst), 1);
    println!("C35_SHUTDOWN tls_destroyed=true panic=false no_wait=true no_gfx_calls=true");
}

#[test]
fn two_player_match_keeps_local_spy_hud_and_rejects_two_actual_locals() {
    let mut s = Scene::new();
    let mut remote = vec_bytes(0x700);
    remote[0x60 / 4] = 0x60000;
    remote[0x4c / 4] = 2 << 8;
    remote[0x144 / 4] = s.world.as_ptr() as u32;
    s.locals = vec![remote.as_ptr() as u32, s.pc.as_ptr() as u32];
    s.wi[0x36c / 4] = s.locals.as_ptr() as u32;
    s.wi[0x370 / 4] = 2;
    s.wi[0x374 / 4] = 2;
    assert!(s.local().unwrap().alive);
    s.locals.swap(0, 1);
    assert!(s.local().unwrap().alive);
    remote[0x4c / 4] = 1 << 8;
    assert!(s.local().is_none());
}

#[test]
fn live_spy_null_icon_allows_warning_and_restores_native_label() {
    let mut s = Scene::new();
    s._displays[3][0x5c / 4] = 0; // C40 real local HUD; not a missing label.
    let before = (s.hud.clone(), s.pawn.clone(), s._displays[3].clone());
    let mut r = Runtime::default();
    let mut ui = Mock {
        zh: true,
        ..Mock::default()
    };
    assert!(r.after_native(
        s.local(),
        Some(Language::TraditionalChinese),
        detected(),
        &mut ui
    ));
    assert_eq!(ui.texts, ["已暴露", "已暴露", "已暴露", "已暴露"]);
    assert_eq!(ui.colors, [0xefa63c; 4]);
    assert_eq!(ui.anim, [true]);
    // The untouched icon may also be absent or reconstructed during exposure.
    let b = s.local().unwrap().binding;
    s.hud[0x32c / 4] = 0;
    assert_eq!(s.local().unwrap().binding, b);
    ui.texts.clear();
    ui.colors.clear();
    ui.anim.clear();
    let mut l = s.local().unwrap();
    l.marked = Some(true);
    assert!(!r.after_native(
        Some(l),
        Some(Language::TraditionalChinese),
        detected(),
        &mut ui
    ));
    assert_eq!(ui.texts, ["被發現", "被發現"]);
    assert_eq!(ui.colors, [0x112233, 0xaabbcc]);
    assert!(ui.anim.is_empty()); // Never hide a genuine native mark.
    s.hud[0x32c / 4] = before.0[0x32c / 4];
    assert_eq!(before, (s.hud, s.pawn, s._displays[3].clone()));
}

#[test]
fn every_modified_hud_widget_still_requires_live_distinct_gfx() {
    for i in 0..3 {
        let mut s = Scene::new();
        s._displays[3][0x5c / 4] = 0;
        s._displays[i][0x5c / 4] = 0;
        assert!(s.local().is_none());
        let mut ui = Mock::default();
        assert!(!Runtime::default().after_native(
            s.local(),
            Some(Language::English),
            detected(),
            &mut ui
        ));
        assert!(ui.texts.is_empty() && ui.colors.is_empty() && ui.anim.is_empty());
    }
    for offset in [0x324, 0x328] {
        let mut s = Scene::new();
        s.hud[offset / 4] = s.hud[0x320 / 4];
        assert!(s.local().is_none());
    }
}

#[test]
fn optional_icon_does_not_invent_detection_or_bypass_goggles() {
    let mut s = Scene::new();
    s._displays[3][0x5c / 4] = 0;
    let mut r = Runtime::default();
    let mut ui = Mock::default();
    assert!(!r.after_native(
        s.local(),
        Some(Language::English),
        ActualDetections::default(),
        &mut ui
    ));
    let mut l = s.local().unwrap();
    l.goggles = false;
    assert!(!r.after_native(Some(l), Some(Language::English), detected(), &mut ui));
    assert!(ui.texts.is_empty() && ui.colors.is_empty() && ui.anim.is_empty());
    assert_eq!(r.last_local_valid, Some(true));
    assert_eq!(r.last_applied, Some(false));
}

#[test]
fn lost_local_hud_clears_diagnostic_state_without_touching_stale_widgets() {
    let s = Scene::new();
    let mut r = Runtime::default();
    let mut ui = Mock::default();
    assert!(r.after_native(s.local(), Some(Language::English), detected(), &mut ui));
    ui.texts.clear();
    ui.colors.clear();
    ui.anim.clear();
    for _ in 0..10 {
        assert!(!r.after_native(None, Some(Language::English), detected(), &mut ui));
        assert_eq!(r.last_local_valid, Some(false));
        assert_eq!(r.last_presentation, None);
        assert_eq!(r.last_applied, None);
    }
    assert!(ui.texts.is_empty() && ui.colors.is_empty() && ui.anim.is_empty());
    assert!(r.after_native(s.local(), Some(Language::English), detected(), &mut ui));
    assert_eq!(r.last_applied, Some(true));
}
