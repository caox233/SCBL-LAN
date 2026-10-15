// Calls the production backend through the same x86 ABIs with owned GFx stubs.
// No native actor, game array, saved profile, or real renderer is modified.
use super::super::{ui, Frame, Mark, Surface};
use std::{cell::RefCell, ffi::CStr};
type Str = [u32; 3];
#[derive(Default)]
struct State {
    nodes: Vec<Box<[u32]>>,
    vt: Box<[u32]>,
    counter: Box<u32>,
    root: Box<u32>,
    missing: String,
    trace: Vec<String>,
    projected: Vec<[f32; 3]>,
    positioned: Vec<[f32; 3]>,
    deleted: Vec<usize>,
    inits: usize,
    frees: usize,
    in_view: bool,
    bad_projection: bool,
    hud: usize,
    flags: Vec<u32>,
}
thread_local! {static S:RefCell<State>=RefCell::new(State::default());}
unsafe fn string(p: *const u8) -> String {
    CStr::from_ptr(p as _).to_str().unwrap().into()
}
fn node(kind: usize) -> usize {
    S.with(|s| {
        let mut s = s.borrow_mut();
        let mut n = vec![0; 4].into_boxed_slice();
        n[0] = s.vt.as_ptr() as u32;
        n[1] = kind as u32;
        let p = n.as_mut_ptr() as usize;
        s.nodes.push(n);
        p
    })
}
unsafe fn kind(p: usize) -> u32 {
    *((p + 4) as *const u32)
}
unsafe extern "C" fn factory(
    root: usize,
    asset: *const u8,
    name: *const u8,
    hud: usize,
    id: u32,
) -> usize {
    S.with(|s| {
        let mut s = s.borrow_mut();
        s.trace.push(format!(
            "factory:{root}:{}:{}:{hud}:{id}",
            string(asset),
            string(name)
        ));
        s.hud = hud;
    });
    node(1)
}
unsafe extern "thiscall" fn init(p: *mut Str, text: *const u8) -> usize {
    *p = [text as u32, string(text).len() as u32, 0];
    S.with(|s| s.borrow_mut().inits += 1);
    p as usize
}
unsafe extern "thiscall" fn free(_p: *mut Str) {
    S.with(|s| s.borrow_mut().frees += 1);
}
unsafe extern "thiscall" fn child(_p: usize, s: *const Str) -> usize {
    let name = string((*s)[0] as *const u8);
    let missing = S.with(|s| {
        let mut s = s.borrow_mut();
        s.trace.push(format!("child:{name}"));
        s.missing == name
    });
    if missing {
        return 0;
    }
    node(match name.as_str() {
        "IconAnim" => 2,
        "BATS_Distance" => 3,
        "BATS_Weapon" => 4,
        _ => 99,
    })
}
unsafe extern "thiscall" fn vis(p: usize, v: u8) {
    S.with(|s| {
        s.borrow_mut()
            .trace
            .push(format!("visible:{}:{v}", kind(p)))
    });
}
unsafe extern "thiscall" fn anim(p: usize, text: *const u8) {
    S.with(|s| {
        s.borrow_mut()
            .trace
            .push(format!("anim:{}:{}", kind(p), string(text)))
    });
}
unsafe extern "thiscall" fn delete(p: usize, flag: u32) -> usize {
    S.with(|s| {
        let mut s = s.borrow_mut();
        s.deleted.push(p);
        s.flags.push(flag);
        s.trace.push(format!("delete:{}", kind(p)));
    });
    p
}
unsafe extern "thiscall" fn project(
    _hud: usize,
    out: *mut [f32; 3],
    p: *const [f32; 3],
    aux: usize,
) -> usize {
    S.with(|s| {
        let mut s = s.borrow_mut();
        s.projected.push(*p);
        s.trace.push(format!("project-aux:{aux}"));
        *out = if s.bad_projection {
            [f32::NAN; 3]
        } else {
            [0.25, 0.75, 1.]
        };
    });
    out as usize
}
unsafe extern "thiscall" fn position(_p: usize, at: *const [f32; 3]) {
    S.with(|s| s.borrow_mut().positioned.push(*at));
}
unsafe extern "C" fn in_view(_p: *const [f32; 3]) -> u8 {
    S.with(|s| s.borrow().in_view as u8)
}
unsafe extern "thiscall" fn text(p: usize, str: *const Str) -> u8 {
    S.with(|s| {
        s.borrow_mut().trace.push(format!(
            "text:{}:{}",
            kind(p),
            string((*str)[0] as *const u8)
        ))
    });
    1
}
unsafe extern "C" fn sound(str: *const Str) {
    S.with(|s| {
        s.borrow_mut()
            .trace
            .push(format!("sound:{}", string((*str)[0] as *const u8)))
    });
}
fn setup() -> ui::Native {
    S.with(|s| {
        let mut s = s.borrow_mut();
        *s = State::default();
        s.vt = vec![0; 0x140 / 4].into_boxed_slice();
        s.vt[0] = delete as usize as u32;
        s.vt[0x108 / 4] = vis as usize as u32;
        s.vt[0x12c / 4] = anim as usize as u32;
        s.counter = Box::new(41);
        s.root = Box::new(b"GFxRoot\0".as_ptr() as u32);
        s.in_view = true;
        ui::Native {
            bindings: ui::Bindings {
                factory,
                init_str: init,
                free_str: free,
                child,
                text_child: child,
                project,
                position,
                in_view,
                text,
                sound,
            },
            root_global: &*s.root as *const u32 as usize,
            counter: &*s.counter as *const u32 as usize,
        }
    })
}
fn frame(now: u64) -> Frame {
    Frame {
        world: 1,
        info: 2,
        local: 3,
        team: 20,
        now,
        position: [0., 0., 0.],
        distance_delay_ms: 1000,
        visible: true,
    }
}
fn mark() -> Mark {
    Mark {
        source: 55,
        serial: 1,
        position: [300., 400., 0.],
        shot_time_ms: 100,
        expires_ms: 7600,
    }
}
#[test]
fn exact_native_assets_project_fixed_world_point_and_release_only_owned_handles() {
    let mut backend = setup();
    let mut h = backend.create(888).unwrap();
    backend.restart(&mut h);
    backend.notify();
    backend.update(888, &mut h, &frame(1200), &mark());
    assert!(h.positioned);
    backend.fade(&mut h);
    backend.destroy(h);
    S.with(|s| {
        let s = s.borrow();
        assert_eq!(*s.counter, 42);
        assert_eq!(s.hud, 888);
        assert_eq!(s.projected, vec![[300., 400., 25.]]);
        assert_eq!(s.positioned, vec![[0.25, 0.75, 1.]]);
        assert_eq!(s.inits, s.frees);
        assert_eq!(s.deleted.len(), 2);
        assert_eq!(s.flags, vec![1, 1]);
        for expected in [
            "child:IconAnim",
            "child:BATS_Distance",
            "child:BATS_Weapon",
            "anim:1:high",
            "anim:2:IconStart",
            "text:3:5m",
            "anim:1:fade_low",
            "anim:2:IconEnd",
            "delete:2",
            "delete:1",
        ] {
            assert!(
                s.trace.iter().any(|v| v == expected),
                "{expected}: {:?}",
                s.trace
            );
        }
        assert!(s.trace[0].ends_with(":BATS_Icon:BATS_Icon_obj_42:888:42"));
        assert!(s
            .trace
            .iter()
            .any(|v| v == "sound:DareStudio.Play_SFX_ADV_VMode_Spy_BATS_Highlight"));
    });
    println!("C28B_NATIVE_BACKEND asset=BATS_Icon fixed_projection=300,400,25 metres=5 fade=fade_low+IconEnd owned_delete=2 actual_GFx_rendered=false");
}
#[test]
fn absent_children_fail_closed_and_do_not_double_delete() {
    for missing in ["IconAnim", "BATS_Distance", "BATS_Weapon"] {
        let mut n = setup();
        S.with(|s| s.borrow_mut().missing = missing.into());
        assert!(n.create(888).is_none());
        S.with(|s| {
            let s = s.borrow();
            assert_eq!(s.inits, s.frees);
            let mut d = s.deleted.clone();
            d.sort();
            d.dedup();
            assert_eq!(d.len(), s.deleted.len());
            assert_eq!(d.len(), if missing == "IconAnim" { 1 } else { 2 });
        });
    }
}
#[test]
fn hidden_offscreen_and_invalid_projection_do_not_draw_or_mutate_mark() {
    let mut n = setup();
    let mut h = n.create(888).unwrap();
    let m = mark();
    let mut f = frame(1200);
    f.visible = false;
    n.update(888, &mut h, &f, &m);
    assert!(!h.positioned);
    f.visible = true;
    S.with(|s| s.borrow_mut().in_view = false);
    n.update(888, &mut h, &f, &m);
    assert!(!h.positioned);
    S.with(|s| {
        let mut s = s.borrow_mut();
        s.in_view = true;
        s.bad_projection = true;
    });
    n.update(888, &mut h, &f, &m);
    assert!(!h.positioned);
    S.with(|s| {
        let s = s.borrow();
        assert!(s.positioned.is_empty());
        assert!(!s.trace.iter().any(|x| x == "visible:1:1"));
    });
    assert_eq!(m, mark());
    n.destroy(h);
}
#[test]
fn distance_delay_and_native_ids_remain_independent_per_instance() {
    let mut n = setup();
    let mut a = n.create(888).unwrap();
    let b = n.create(888).unwrap();
    n.update(888, &mut a, &frame(1100), &mark());
    S.with(|s| {
        let s = s.borrow();
        assert_eq!(*s.counter, 43);
        assert!(!s.trace.iter().any(|x| x.starts_with("text:")));
    });
    n.update(888, &mut a, &frame(1101), &mark());
    n.destroy(a);
    n.destroy(b);
    S.with(|s| assert_eq!(s.borrow().deleted.len(), 4));
}

#[test]
fn high_volume_widget_churn_balances_native_string_and_handle_ownership() {
    let mut n = setup();
    for _ in 0..1024 {
        let mut h = n.create(888).unwrap();
        n.restart(&mut h);
        n.update(888, &mut h, &frame(1200), &mark());
        n.fade(&mut h);
        n.destroy(h);
    }
    S.with(|s| {
        let s = s.borrow();
        assert_eq!(s.deleted.len(), 2048);
        assert_eq!(s.inits, s.frees);
        let mut seen = s.deleted.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 2048);
        assert_eq!(*s.counter, 1065);
    });
    println!("C28B_UI_CHURN created=1024 deleted=2048 native_string_balance=true native_array_untouched=true");
}
