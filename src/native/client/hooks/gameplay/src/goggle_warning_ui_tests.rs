use super::*;
use std::cell::RefCell;
use std::ffi::CStr;
use std::sync::Mutex;
static SERIAL: Mutex<()> = Mutex::new(());
#[derive(Default)]
struct Graph {
    color: u32,
    gets: usize,
    releases: usize,
    sets: usize,
    fail: bool,
}
thread_local! { static GRAPH:RefCell<Graph>=RefCell::new(Graph{color:0x6699aa,..Graph::default()}); }

unsafe extern "thiscall" fn get(
    interface: usize,
    object: usize,
    name: *const u8,
    out: *mut Value,
    display: u8,
) -> u8 {
    GRAPH.with(|g| {
        let mut g = g.borrow_mut();
        g.gets += 1;
        let name = CStr::from_ptr(name.cast()).to_bytes();
        if name == b"textField" {
            assert_eq!((interface, object, display), (0x111, 0x222, 1));
            out.write(Value {
                interface: 0x333,
                kind: 0x49,
                data: 0x444,
            });
        } else {
            assert_eq!(name, b"textColor");
            assert_eq!((interface, object, display), (0x333, 0x444, 0));
            out.write(Value {
                interface: 0,
                kind: 5,
                data: (g.color as f64).to_bits(),
            });
        }
        u8::from(!g.fail)
    })
}
unsafe extern "thiscall" fn release(interface: usize, value: *mut Value, object: usize) {
    assert_eq!((interface, object), (0x333, 0x444));
    assert_eq!((*value).kind, 0x49);
    GRAPH.with(|g| g.borrow_mut().releases += 1);
}
unsafe extern "thiscall" fn set_member(
    interface: usize,
    object: usize,
    name: *const u8,
    value: *const Value,
    display: u8,
) -> u8 {
    assert_eq!((interface, object, display), (0x333, 0x444, 0));
    assert_eq!(CStr::from_ptr(name.cast()).to_bytes(), b"textColor");
    assert_eq!((*value).kind, 5);
    GRAPH.with(|g| {
        let mut g = g.borrow_mut();
        g.sets += 1;
        g.color = (*value).color().unwrap();
    });
    1
}
unsafe extern "thiscall" fn unused_set(_: usize, _: *const u8, _: u32) {
    panic!("unexpected write");
}
struct Widget {
    words: Vec<u32>,
    _display: Vec<u32>,
    _root: Box<Value>,
}
fn widget() -> Widget {
    let root = Box::new(Value {
        interface: 0x111,
        kind: 0xa,
        data: 0x222,
    });
    let mut display = vec![0; 0x60 / 4];
    display[0x5c / 4] = (&*root as *const Value) as u32;
    let mut words = vec![0; 0x1b0 / 4];
    words[0x1a8 / 4] = display.as_ptr() as u32;
    Widget {
        words,
        _display: display,
        _root: root,
    }
}
fn api() -> ColorApi {
    ColorApi {
        get,
        release,
        set: unused_set,
    }
}
fn reset() {
    GRAPH.with(|g| {
        *g.borrow_mut() = Graph {
            color: 0x6699aa,
            ..Graph::default()
        }
    });
}

#[test]
fn color_is_exact_rgb_and_rejects_nonnumeric_or_fractional() {
    for n in [0, 0x6699aa, 0xffffffff] {
        assert_eq!(
            Value {
                kind: 5,
                data: (n as f64).to_bits(),
                ..Value::default()
            }
            .color(),
            Some(n)
        );
    }
    for n in [f64::NAN, f64::INFINITY, -1., 1.5, u32::MAX as f64 + 1.] {
        assert_eq!(
            Value {
                kind: 5,
                data: n.to_bits(),
                ..Value::default()
            }
            .color(),
            None
        );
    }
    assert_eq!(
        Value {
            kind: 3,
            data: 0xffffffff,
            ..Value::default()
        }
        .color(),
        None
    );
    assert_eq!(
        Value {
            kind: 6,
            data: 123,
            ..Value::default()
        }
        .color(),
        None
    );
}
#[test]
fn read_color_uses_native_child_and_releases_owned_value() {
    reset();
    let w = widget();
    assert_eq!(
        unsafe { api().read(w.words.as_ptr() as usize) },
        Some(0x6699aa)
    );
    GRAPH.with(|g| assert_eq!((g.borrow().gets, g.borrow().releases), (2, 1)));
}
#[test]
fn failed_get_also_releases_returned_managed_value() {
    reset();
    GRAPH.with(|g| g.borrow_mut().fail = true);
    let w = widget();
    assert_eq!(unsafe { api().read(w.words.as_ptr() as usize) }, None);
    GRAPH.with(|g| assert_eq!((g.borrow().gets, g.borrow().releases), (1, 1)));
}
#[test]
fn missing_display_or_nonobject_root_never_calls_native() {
    reset();
    assert_eq!(unsafe { api().read(0) }, None);
    let mut w = widget();
    w._root.kind = 5;
    assert_eq!(unsafe { api().read(w.words.as_ptr() as usize) }, None);
    GRAPH.with(|g| assert_eq!(g.borrow().gets, 0));
}

fn binding() -> Binding {
    Binding {
        life: (1, 2, 3),
        hud: 3,
        container: 4,
        texts: [5, 6],
        display: [8, 9, 10],
        gfx: [12, 13, 14],
    }
}
#[derive(Default)]
struct Mock {
    events: Vec<String>,
    no_colors: bool,
    no_label: bool,
    zh: bool,
    no_transform: bool,
}

#[test]
fn style_is_ready_before_intro_reasserted_after_and_sound_is_once() {
    let mut s = WarningUi::default();
    let mut ui = Mock::default();
    for _ in 0..100 {
        assert!(s.apply(
            Some(binding()),
            Presentation::Exposed,
            Language::English,
            &mut ui
        ));
    }
    let intro = ui
        .events
        .iter()
        .position(|e| e == "animation:4:true")
        .unwrap();
    assert!(ui.events[..intro].contains(&"text:5:EXPOSED".into()));
    assert!(ui.events[..intro].contains(&"text:6:EXPOSED".into()));
    assert!(ui.events[..intro].contains(&"tint:amber".into()));
    assert_eq!(ui.events[intro + 1], "text:5:EXPOSED");
    assert_eq!(ui.events.iter().filter(|e| *e == "sound").count(), 1);
    assert_eq!(
        ui.events
            .iter()
            .filter(|e| *e == "animation:4:true")
            .count(),
        1
    );
    println!("C44_PRESENTATION frames=100 intro=1 sound=1 style_before_and_after_intro=true");
}

#[test]
fn yellow_outro_does_not_flash_red_and_next_native_mark_restores_snapshot() {
    let mut s = WarningUi::default();
    let mut ui = Mock::default();
    s.apply(
        Some(binding()),
        Presentation::Exposed,
        Language::English,
        &mut ui,
    );
    ui.events.clear();
    for _ in 0..100 {
        s.apply(
            Some(binding()),
            Presentation::Hidden,
            Language::English,
            &mut ui,
        );
    }
    assert_eq!(ui.events, ["animation:4:false"]);
    ui.events.clear();
    s.apply(
        Some(binding()),
        Presentation::Exposed,
        Language::English,
        &mut ui,
    );
    assert_eq!(ui.events.iter().filter(|e| *e == "sound").count(), 1);
    ui.events.clear();
    s.apply(
        Some(binding()),
        Presentation::Native,
        Language::English,
        &mut ui,
    );
    assert!(ui.events.contains(&"color:5:112233".into()));
    assert!(ui.events.contains(&"tint:restore".into()));
    assert!(!ui.events.contains(&"sound".into()));
}

#[test]
fn missing_transform_snapshot_must_not_start_or_play_sound() {
    let mut s = WarningUi::default();
    let mut ui = Mock {
        no_transform: true,
        ..Mock::default()
    };
    assert!(!s.apply(
        Some(binding()),
        Presentation::Exposed,
        Language::English,
        &mut ui
    ));
    assert!(ui.events.is_empty());
}

#[test]
fn whole_container_amber_preserves_current_animation_alpha_and_exact_rgb_restore() {
    let old = Transform([0.9, 0.8, 0.7, 0.4, 0.1, 0.2, 0.3, 0.05]);
    let yellow = old.with_rgb(Transform::amber());
    assert_eq!(yellow.0[3], old.0[3]);
    assert_eq!(yellow.0[7], old.0[7]);
    assert_eq!(&yellow.0[..3], &[0., 0., 0.]);
    assert_eq!(&yellow.0[4..7], &[239. / 255., 166. / 255., 60. / 255.]);
    let mut advanced = yellow;
    advanced.0[3] = 0.125;
    advanced.0[7] = 0.;
    let restored = advanced.with_rgb(old);
    for i in [0, 1, 2, 4, 5, 6] {
        assert_eq!(restored.0[i], old.0[i]);
    }
    assert_eq!(restored.0[3], 0.125);
    assert_eq!(restored.0[7], 0.);
    assert!(!Transform([f32::NAN; 8]).finite());
}

#[test]
fn utf16_text_member_reasserts_even_when_native_string_cache_is_unchanged() {
    unsafe extern "thiscall" fn set_text(
        i: usize,
        o: usize,
        n: *const u8,
        v: *const Value,
        d: u8,
    ) -> u8 {
        assert_eq!((i, o, d), (0x333, 0x444, 0));
        assert_eq!(CStr::from_ptr(n.cast()).to_bytes(), b"text");
        assert_eq!((*v).kind, 7);
        assert_eq!((*v).interface, 0);
        let p = (*v).data as *const u16;
        let expected: Vec<u16> = "EXPOSED".encode_utf16().chain([0]).collect();
        assert_eq!(std::slice::from_raw_parts(p, expected.len()), expected);
        GRAPH.with(|g| g.borrow_mut().sets += 1);
        1
    }
    reset();
    let w = widget();
    let t: Vec<u16> = "EXPOSED".encode_utf16().chain([0]).collect();
    for _ in 0..1000 {
        assert!(unsafe { api().force_text(w.words.as_ptr() as usize, &t, set_text) });
    }
    GRAPH.with(|g| assert_eq!((g.borrow().sets, g.borrow().releases), (1000, 1000)));
    assert!(!unsafe { api().force_text(w.words.as_ptr() as usize, &[65], set_text) });
}

#[test]
fn retail_cxform_get_set_abi_copy_32_bytes_and_keep_alpha() {
    use sha2::{Digest, Sha256};
    use windows::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    };
    #[derive(Clone, Copy)]
    struct State {
        object: usize,
        value: Transform,
        gets: usize,
        sets: usize,
        invalid: usize,
    }
    thread_local! {static ST:RefCell<State>=const {RefCell::new(State{object:0,value:Transform([1.;8]),gets:0,sets:0,invalid:0})};}
    unsafe extern "thiscall" fn resolve(data: usize, root: usize) -> usize {
        assert_eq!((data, root), (0x222, 0x555));
        ST.with(|s| s.borrow().object)
    }
    unsafe extern "thiscall" fn kind(_: usize) -> u32 {
        4
    }
    unsafe extern "thiscall" fn read(_: usize) -> *const Transform {
        ST.with(|s| {
            let mut s = s.borrow_mut();
            s.gets += 1;
            &s.value as *const Transform
        })
    }
    unsafe extern "thiscall" fn write(_: usize, p: *const Transform) {
        ST.with(|s| {
            let mut s = s.borrow_mut();
            s.value = p.read();
            s.sets += 1;
        });
    }
    unsafe extern "thiscall" fn invalidate(_: usize, n: usize) {
        assert_eq!(n, 0);
        ST.with(|s| s.borrow_mut().invalid += 1);
    }
    fn rel(code: &mut [u8], old: usize, site: usize, base: usize, to: usize) {
        let o = site - old;
        assert_eq!(code[o], 0xe8);
        code[o + 1..o + 5]
            .copy_from_slice(&((to as isize - (base + o + 5) as isize) as i32).to_le_bytes());
    }
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C44_FIXTURE").unwrap());
    let mut get = std::fs::read(root.join("native-02417420.bin")).unwrap();
    let mut set = std::fs::read(root.join("native-02417480.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&get)),
        "5a024468ede23fd206fb3a05a3a897a5d839f730f4599dfba89f7e9f2c713749"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(&set)),
        "8a3d5575d49c42bf24cc555b84cff84220e1873cd02f39a9ca95912ce9ef5230"
    );
    let mut vt = vec![0usize; 0x140 / 4];
    vt[0x12c / 4] = kind as usize;
    vt[0xc4 / 4] = invalidate as usize;
    let mut object = vec![0usize; 0x80 / 4];
    object[0] = vt.as_ptr() as usize;
    let interface = [0usize, 0x555];
    let mut w = widget();
    w._root.interface = interface.as_ptr() as usize;
    ST.with(|s| {
        *s.borrow_mut() = State {
            object: object.as_ptr() as usize,
            value: Transform([1., 1., 1., 0.35, 0., 0., 0., 0.1]),
            gets: 0,
            sets: 0,
            invalid: 0,
        }
    });
    unsafe {
        let m = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(m, 0);
        rel(&mut get, 0x2417420, 0x241742b, m, resolve as usize);
        rel(&mut get, 0x2417420, 0x241745d, m, read as usize);
        rel(&mut set, 0x2417480, 0x241748b, m + 256, resolve as usize);
        rel(&mut set, 0x2417480, 0x24174c0, m + 256, write as usize);
        std::ptr::copy_nonoverlapping(get.as_ptr(), m as *mut u8, get.len());
        std::ptr::copy_nonoverlapping(set.as_ptr(), (m + 256) as *mut u8, set.len());
        let api = TransformApi {
            get: std::mem::transmute(m),
            set: std::mem::transmute(m + 256),
        };
        let before = api.read(w.words.as_ptr() as usize).unwrap();
        for _ in 0..1000 {
            assert!(api.rgb(w.words.as_ptr() as usize, Transform::amber()));
        }
        let after = api.read(w.words.as_ptr() as usize).unwrap();
        assert_eq!(after.0[3], 0.35);
        assert_eq!(after.0[7], 0.1);
        assert!(api.rgb(w.words.as_ptr() as usize, before));
        assert_eq!(api.read(w.words.as_ptr() as usize), Some(before));
        ST.with(|s| assert_eq!((s.borrow().sets, s.borrow().invalid), (1001, 1001)));
        ST.with(|s| s.borrow_mut().object = 0);
        assert!(api.read(w.words.as_ptr() as usize).is_none());
        assert!(!api.rgb(w.words.as_ptr() as usize, Transform::amber()));
        VirtualFree(m as *mut _, 0, MEM_RELEASE).unwrap();
    }
    println!("C44_NATIVE cxform=retail_bytes calls=1000 bytes=32 alpha_unchanged=true rgb_rollback_exact=true engine_stubbed=true");
}
impl Ui for Mock {
    fn colors(&mut self, _: Binding) -> Option<[u32; 2]> {
        (!self.no_colors).then_some([0x112233, 0xaabbcc])
    }
    fn text(&mut self, w: usize, t: &[u16]) -> bool {
        self.events.push(format!(
            "text:{w}:{}",
            String::from_utf16(&t[..t.len() - 1]).unwrap()
        ));
        true
    }
    fn color(&mut self, w: usize, c: u32) {
        self.events.push(format!("color:{w}:{c:x}"));
    }
    fn transform(&mut self, _: usize) -> Option<Transform> {
        (!self.no_transform).then_some(Transform([0.9, 0.8, 0.7, 0.4, 0.1, 0.2, 0.3, 0.05]))
    }
    fn tint_rgb(&mut self, _: usize, rgb: Transform) -> bool {
        self.events.push(
            if rgb == Transform::amber() {
                "tint:amber"
            } else {
                "tint:restore"
            }
            .into(),
        );
        true
    }
    fn sound(&mut self, _: Binding) {
        self.events.push("sound".into());
    }
    fn native_label(&mut self) -> Option<Vec<u16>> {
        if self.no_label {
            None
        } else {
            Some(
                (if self.zh { "被發現" } else { "DETECTED" })
                    .encode_utf16()
                    .chain([0])
                    .collect(),
            )
        }
    }
    fn animation(&mut self, w: usize, intro: bool) {
        self.events.push(format!("animation:{w}:{intro}"));
    }
}
#[test]
fn warning_restores_each_original_color_not_an_assumed_default() {
    let mut s = WarningUi::default();
    let mut ui = Mock::default();
    assert!(s.apply(
        Some(binding()),
        Presentation::Exposed,
        Language::English,
        &mut ui
    ));
    assert!(ui.events.contains(&"text:5:EXPOSED".into()));
    assert!(ui.events.contains(&"color:6:efa63c".into()));
    ui.events.clear();
    s.apply(
        Some(binding()),
        Presentation::Native,
        Language::English,
        &mut ui,
    );
    assert_eq!(
        ui.events,
        [
            "text:5:DETECTED",
            "color:5:112233",
            "text:6:DETECTED",
            "color:6:aabbcc",
            "tint:restore"
        ]
    );
    ui.events.clear();
    s.apply(
        Some(binding()),
        Presentation::Hidden,
        Language::English,
        &mut ui,
    );
    assert!(ui.events.is_empty());
}
#[test]
fn genuine_mark_gets_native_priority_without_outro() {
    let mut s = WarningUi::default();
    let mut ui = Mock::default();
    s.apply(
        Some(binding()),
        Presentation::Exposed,
        Language::English,
        &mut ui,
    );
    ui.events.clear();
    s.apply(
        Some(binding()),
        Presentation::Native,
        Language::English,
        &mut ui,
    );
    assert!(ui.events.iter().all(|s| !s.starts_with("animation:")));
    assert_eq!(ui.events[0], "text:5:DETECTED");
}
#[test]
fn no_repeated_intro_during_one_exposure() {
    let mut s = WarningUi::default();
    let mut ui = Mock::default();
    for _ in 0..10 {
        s.apply(
            Some(binding()),
            Presentation::Exposed,
            Language::English,
            &mut ui,
        );
    }
    assert_eq!(
        ui.events
            .iter()
            .filter(|s| s.starts_with("animation:"))
            .count(),
        1
    );
}
#[test]
fn changed_widget_or_life_never_restores_old_handles() {
    let mut s = WarningUi::default();
    let mut ui = Mock::default();
    s.apply(
        Some(binding()),
        Presentation::Exposed,
        Language::English,
        &mut ui,
    );
    ui.events.clear();
    let mut b = binding();
    b.gfx[1] += 1;
    s.apply(Some(b), Presentation::Hidden, Language::English, &mut ui);
    assert!(ui.events.is_empty());
    s.apply(
        Some(binding()),
        Presentation::Exposed,
        Language::English,
        &mut ui,
    );
    ui.events.clear();
    s.apply(None, Presentation::Hidden, Language::English, &mut ui);
    assert!(ui.events.is_empty());
}
#[test]
fn unavailable_color_or_localized_restore_cannot_start_warning() {
    for (no_colors, no_label) in [(true, false), (false, true)] {
        let mut s = WarningUi::default();
        let mut ui = Mock {
            no_colors,
            no_label,
            ..Mock::default()
        };
        assert!(!s.apply(
            Some(binding()),
            Presentation::Exposed,
            Language::English,
            &mut ui
        ));
        assert!(ui.events.is_empty());
    }
}
#[test]
fn restore_uses_current_native_language() {
    let mut s = WarningUi::default();
    let mut ui = Mock::default();
    s.apply(
        Some(binding()),
        Presentation::Exposed,
        Language::TraditionalChinese,
        &mut ui,
    );
    assert!(ui.events.contains(&"text:5:已暴露".into()));
    ui.events.clear();
    ui.zh = true;
    s.apply(
        Some(binding()),
        Presentation::Native,
        Language::TraditionalChinese,
        &mut ui,
    );
    assert_eq!(ui.events[0], "text:5:被發現");
}
#[test]
fn binding_requires_local_owner_world_and_complete_distinct_widgets() {
    assert!(unsafe { Binding::resolve(0, 1, 2, 1) }.is_none());
    let mut hud = vec![0u32; 0x340 / 4];
    let mut pawn = vec![0u32; 0x150 / 4];
    let mut context = vec![0u32; 0x320 / 4];
    let ws = [widget(), widget(), widget(), widget()];
    hud[0] = HUD;
    hud[0x25c / 4] = context.as_ptr() as u32;
    context[0x310 / 4] = pawn.as_ptr() as u32;
    pawn[0x144 / 4] = 123;
    for (i, offset) in [0x320, 0x324, 0x328, 0x32c].into_iter().enumerate() {
        hud[offset / 4] = ws[i].words.as_ptr() as u32;
    }
    assert!(
        unsafe { Binding::resolve(hud.as_ptr() as usize, 123, pawn.as_ptr() as usize, 1) }
            .is_some()
    );
    context[0x310 / 4] = 1;
    assert!(
        unsafe { Binding::resolve(hud.as_ptr() as usize, 123, pawn.as_ptr() as usize, 1) }
            .is_none()
    );
    context[0x310 / 4] = pawn.as_ptr() as u32;
    hud[0x328 / 4] = hud[0x324 / 4];
    assert!(
        unsafe { Binding::resolve(hud.as_ptr() as usize, 123, pawn.as_ptr() as usize, 1) }
            .is_none()
    );
}
#[test]
fn real_native_color_wrapper_abi_and_unsigned_values_round_trip() {
    use sha2::{Digest, Sha256};
    use windows::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    };
    let _lock = SERIAL.lock().unwrap();
    reset();
    let root = std::path::PathBuf::from(
        std::env::var("SCBL_R58_C29E_FIXTURE").expect("native UI fixture"),
    );
    let mut wrapper = std::fs::read(root.join("contract-color-widget.bin")).unwrap();
    let mut body = std::fs::read(root.join("contract-color-body.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&wrapper)),
        "36ebbe709c41aa1580513a812b94bd445b5efecd72d1473e2a18d87a9223108c"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(&body)),
        "c6793f2699942bf77fcdde326a78a622fd9fcc2dd59060ad746acab7ee5576d0"
    );
    unsafe {
        let memory =
            VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(memory, 0);
        fn rel(code: &mut [u8], original: usize, site: usize, base: usize, to: usize) {
            let o = site - original;
            assert!(matches!(code[o], 0xe8 | 0xe9));
            code[o + 1..o + 5]
                .copy_from_slice(&((to as isize - (base + o + 5) as isize) as i32).to_le_bytes());
        }
        rel(&mut wrapper, 0x1862fd0, 0x1862fde, memory, memory + 0x100);
        for (site, to) in [
            (0x1861201, get as usize),
            (0x186122d, release as usize),
            (0x1861275, set_member as usize),
            (0x1861292, release as usize),
            (0x18612b9, release as usize),
        ] {
            rel(&mut body, 0x18611a0, site, memory + 0x100, to);
        }
        let cookie = 0x12345678u32;
        let correction = 4294967296f64;
        assert_eq!(&body[0x16..0x1a], &0x3288bbcu32.to_le_bytes());
        body[0x16..0x1a].copy_from_slice(&(&cookie as *const _ as u32).to_le_bytes());
        let off = 0x1861248 - 0x18611a0;
        assert_eq!(&body[off..off + 4], &0x2d75bd8u32.to_le_bytes());
        body[off..off + 4].copy_from_slice(&(&correction as *const _ as u32).to_le_bytes());
        let color_property = b"textColor\0";
        let offset = 0x1861270 - 0x18611a0;
        assert_eq!(&body[offset..offset + 4], &0x2c12c14u32.to_le_bytes());
        body[offset..offset + 4].copy_from_slice(&(color_property.as_ptr() as u32).to_le_bytes());
        std::ptr::copy_nonoverlapping(wrapper.as_ptr(), memory as *mut u8, wrapper.len());
        std::ptr::copy_nonoverlapping(body.as_ptr(), (memory + 0x100) as *mut u8, body.len());
        let native = ColorApi {
            get,
            release,
            set: std::mem::transmute(memory),
        };
        let w = widget();
        for _ in 0..1000 {
            for rgb in [0xffd34d, 0x6699aa, 0xffffffff] {
                native.write(w.words.as_ptr() as usize, rgb);
                assert_eq!(native.read(w.words.as_ptr() as usize), Some(rgb));
            }
        }
        GRAPH.with(|g| {
            let g = g.borrow();
            assert_eq!(g.sets, 3000);
            assert_eq!(g.releases, 6000);
        });
        VirtualFree(memory as *mut _, 0, MEM_RELEASE).unwrap();
        println!("C29E_NATIVE_COLOR wrapper1862FD0 body18611A0 iterations=3000 unsigned_color_exact=true managed_release_exact=true; GFx engine callbacks are fixture stubs");
    }
}

#[test]
fn temporary_localization_failure_restores_original_before_native_mark() {
    let mut s = WarningUi::default();
    let mut ui = Mock::default();
    s.apply(
        Some(binding()),
        Presentation::Exposed,
        Language::English,
        &mut ui,
    );
    ui.events.clear();
    ui.no_label = true;
    s.apply(
        Some(binding()),
        Presentation::Native,
        Language::English,
        &mut ui,
    );
    assert_eq!(
        ui.events,
        [
            "text:5:DETECTED",
            "color:5:112233",
            "text:6:DETECTED",
            "color:6:aabbcc",
            "tint:restore"
        ]
    );
}
#[test]
fn new_spawn_generation_cannot_restore_previous_player_style() {
    let mut s = WarningUi::default();
    let mut ui = Mock::default();
    s.apply(
        Some(binding()),
        Presentation::Exposed,
        Language::English,
        &mut ui,
    );
    ui.events.clear();
    let mut fresh = binding();
    fresh.life.2 += 1;
    s.apply(
        Some(fresh),
        Presentation::Native,
        Language::English,
        &mut ui,
    );
    assert!(ui.events.is_empty());
}

// Appended to the exact UI module solely to construct its private typed Binding.
mod boundary_fixture {
    use super::*;
    const ORIGINAL: [u32; 2] = [0x112233, 0xaabbcc];
    fn rgb() -> Transform { Transform([0.9,0.8,0.7,0.4,0.1,0.2,0.3,0.05]) }
    fn b() -> Binding {
        Binding { life:(1,2,3),hud:10,container:11,texts:[12,13],display:[14,15,16],gfx:[17,18,19] }
    }
    fn wide(s:&str)->Vec<u16>{s.encode_utf16().chain([0]).collect()}
    pub struct Surface {
        colors:[u32;2], labels:[Vec<u16>;2], tint:Transform,
        calls:usize,intros:usize,outros:usize,sounds:usize,
        fail_text:Option<usize>, fail_tint:bool, zh:bool,
    }
    impl Default for Surface {
        fn default()->Self {Self {colors:ORIGINAL,labels:[wide("DETECTED"),wide("DETECTED")],tint:rgb(),calls:0,intros:0,outros:0,sounds:0,fail_text:None,fail_tint:false,zh:false}}
    }
    impl Surface {
        fn original(&self)->bool { self.colors==ORIGINAL && self.tint==rgb() && self.labels==[wide(if self.zh {"被發現"} else {"DETECTED"}),wide(if self.zh {"被發現"} else {"DETECTED"})] }
    }
    impl Ui for Surface {
        fn colors(&mut self,_:Binding)->Option<[u32;2]>{self.calls+=1;Some(self.colors)}
        fn text(&mut self,w:usize,t:&[u16])->bool {
            self.calls+=1;
            if self.fail_text==Some(w) {self.fail_text=None;return false;}
            self.labels[w-12]=t.to_vec();true
        }
        fn transform(&mut self,_:usize)->Option<Transform>{self.calls+=1;Some(self.tint)}
        fn tint_rgb(&mut self,_:usize,t:Transform)->bool {
            self.calls+=1;
            if self.fail_tint {self.fail_tint=false;return false;}
            self.tint=self.tint.with_rgb(t);true
        }
        fn sound(&mut self,_:Binding){self.calls+=1;self.sounds+=1;}
        fn color(&mut self,w:usize,c:u32){self.calls+=1;self.colors[w-12]=c;}
        fn native_label(&mut self)->Option<Vec<u16>>{self.calls+=1;Some(wide(if self.zh {"被發現"} else {"DETECTED"}))}
        fn animation(&mut self,_:usize,intro:bool){self.calls+=1;if intro{self.intros+=1}else{self.outros+=1}}
    }
    fn apply(s:&mut WarningUi,u:&mut Surface,p:Presentation){s.apply(Some(b()),p,Language::English,u);}
    pub fn observations()->Vec<(&'static str,bool)> {
        let mut out=Vec::new();
        let(mut s,mut u)=(WarningUi::default(),Surface::default());
        for _ in 0..20 {apply(&mut s,&mut u,Presentation::Exposed);}
        out.push(("continuous_episode_one_intro_sound",u.intros==1&&u.sounds==1));
        let calls=u.calls;
        s.apply(None,Presentation::Native,Language::English,&mut u);
        out.push(("missing_binding_never_calls_ui",u.calls==calls));
        apply(&mut s,&mut u,Presentation::Native);
        out.push(("same_binding_recovery_restores_native_style",u.original()));
        out.push(("native_restore_no_outro_or_extra_sound",u.outros==0&&u.sounds==1));

        let(mut s,mut u)=(WarningUi::default(),Surface::default());
        apply(&mut s,&mut u,Presentation::Exposed);
        s.apply(None,Presentation::Hidden,Language::English,&mut u);
        apply(&mut s,&mut u,Presentation::Exposed);
        out.push(("same_binding_gap_does_not_replay_sound",u.sounds==1&&u.intros==1));
        apply(&mut s,&mut u,Presentation::Native);
        out.push(("gap_never_recaptures_yellow_as_original",u.original()));

        let(mut s,mut u)=(WarningUi::default(),Surface::default());
        apply(&mut s,&mut u,Presentation::Exposed);
        s.apply(None,Presentation::Hidden,Language::English,&mut u);
        apply(&mut s,&mut u,Presentation::Hidden);
        apply(&mut s,&mut u,Presentation::Hidden);
        out.push(("revalidated_hidden_closes_old_episode_once",u.outros==1));
        apply(&mut s,&mut u,Presentation::Native);
        out.push(("hidden_recovery_keeps_pristine_style",u.original()));

        for (name,text,tint) in [("native_text_failure_retries",Some(13),false),("native_tint_failure_retries",None,true)] {
            let(mut s,mut u)=(WarningUi::default(),Surface::default());
            apply(&mut s,&mut u,Presentation::Exposed);u.fail_text=text;u.fail_tint=tint;
            apply(&mut s,&mut u,Presentation::Native);
            apply(&mut s,&mut u,Presentation::Native);
            out.push((name,u.original()&&u.sounds==1&&u.outros==0));
        }
        let(mut s,mut u)=(WarningUi::default(),Surface::default());
        apply(&mut s,&mut u,Presentation::Exposed);u.fail_tint=true;
        apply(&mut s,&mut u,Presentation::Native);
        apply(&mut s,&mut u,Presentation::Exposed);
        apply(&mut s,&mut u,Presentation::Native);
        out.push(("new_episode_after_failed_restore_keeps_pristine_style",u.original()&&u.sounds==2));

        let(mut s,mut u)=(WarningUi::default(),Surface::default());
        apply(&mut s,&mut u,Presentation::Exposed);
        s.apply(None,Presentation::Native,Language::English,&mut u);
        u.zh=true;apply(&mut s,&mut u,Presentation::Native);
        out.push(("recovery_uses_current_native_locale",u.original()));

        let(mut s,mut u)=(WarningUi::default(),Surface::default());
        apply(&mut s,&mut u,Presentation::Exposed);let calls=u.calls;
        s.apply(None,Presentation::Native,Language::English,&mut u);
        let mut other=b();other.life.2+=1;
        s.apply(Some(other),Presentation::Native,Language::English,&mut u);
        out.push(("different_life_never_restores_old_handles",u.calls==calls));

        let(mut s,mut u)=(WarningUi::default(),Surface::default());
        s.apply_kind(Some(b()),Presentation::Exposed,Language::English,WarningKind::Interference,&mut u);
        s.apply(None,Presentation::Native,Language::English,&mut u);
        apply(&mut s,&mut u,Presentation::Native);
        out.push(("merc_interference_restores_without_spy_sound",u.original()&&u.sounds==0));
        out
    }
}
#[allow(dead_code)]
pub(crate) fn run_boundary_fixture()->i32 {
    let rows=boundary_fixture::observations();let failed=rows.iter().filter(|(_,ok)|!*ok).count();
    for (n,ok) in &rows {println!("{}={}",n,ok);}
    println!("observations={} failed={}",rows.len(),failed);
    if failed==0{0}else{1}
}

#[test]
fn transient_binding_and_restore_failures_preserve_ui_ownership() {
    for (name, passed) in boundary_fixture::observations() {
        assert!(passed, "{name}");
    }
}

#[test]
fn exposed_and_rfd_interference_share_amber_without_extra_sound() {
    for kind in [WarningKind::Exposed, WarningKind::Interference] {
        let mut state=WarningUi::default();let mut ui=Mock::default();
        assert!(state.apply_kind(Some(binding()),Presentation::Exposed,Language::English,kind,&mut ui));
        assert!(ui.events.contains(&"color:5:efa63c".into()));
        assert!(ui.events.contains(&"color:6:efa63c".into()));
        assert!(ui.events.contains(&"tint:amber".into()));
        assert_eq!(ui.events.iter().filter(|e| *e=="sound").count(),usize::from(kind==WarningKind::Exposed));
        ui.events.clear();state.apply_kind(Some(binding()),Presentation::Native,Language::English,kind,&mut ui);
        assert!(ui.events.contains(&"color:5:112233".into()));
        assert!(ui.events.contains(&"color:6:aabbcc".into()));
    }
}
