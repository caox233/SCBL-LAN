//! Exact retail instruction fixtures; engine rendering/transport leaves isolated.
//! These prove ABI/state routing, not live multiplayer replication or visuals.
use super::*;
use std::cell::Cell;
thread_local! { static ACTIVE_FLOW: Cell<usize> = const { Cell::new(0) }; }
static FEEDBACK: AtomicU32 = AtomicU32::new(0);
static DISABLE: AtomicU32 = AtomicU32::new(0);
static SCORES: AtomicU32 = AtomicU32::new(0);

fn fixture(name: &str, hash: &str) -> Vec<u8> {
    let path = std::path::PathBuf::from(std::env::var("SCBL_R58_C18_FIXTURE").unwrap());
    let code = std::fs::read(path.join(name)).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&code)), hash);
    code
}
unsafe extern "thiscall" fn guarded_cancel(a: usize) {
    let keep = ACTIVE_FLOW
        .with(|p| p.get() != 0 && (*(p.get() as *const ActionFlow)).keep_parked_on_owner_hit(a));
    if !keep {
        let native: unsafe extern "thiscall" fn(usize) =
            std::mem::transmute(*(*(a as *const usize) as *const usize).add(0xb8 / 4));
        native(a);
    }
}
unsafe extern "thiscall" fn disable_event(_: usize, _: usize, a: usize, b: usize) {
    assert_eq!((a, b), (0, 0));
    DISABLE.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn feedback(kind: u32, _: usize, _: f32, _: f32, owner: usize) {
    assert_eq!(kind, 0x2a);
    assert_ne!(owner, 0);
    FEEDBACK.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn score(_: usize, source: usize) {
    assert_eq!(source, 0x1357);
    SCORES.fetch_add(1, Ordering::SeqCst);
}
unsafe fn ticket(f: &mut Fixture, flow: &ActionFlow) {
    let request = f.request().unwrap();
    f.park();
    f.state[0x138 / 4] = 0;
    f.body[0x4c / 4] = 1 << 8;
    let parked = ParkedDrone::after_exit(&request).unwrap();
    flow.reserve(f.ability(), Phase::Parked(parked)).unwrap();
}
unsafe fn cancel_code(f: &Fixture) -> unsafe extern "thiscall" fn(usize) {
    let at = |v| f.base + v - 0x400000;
    let code = fixture(
        "cancel.bin",
        "12d269c89b565ddf32cbce1b0e2ac844140c84952387e4f7092e6bf1243786b9",
    );
    put(at(0xe6d7a0), &code);
    jump(at(0xd873f0), disable_event as *const () as usize);
    jump(at(0xdb0ee0), remove_body as *const () as usize);
    jump(at(0xe6cd60), deactivate_fixture as *const () as usize);
    *((f.layout.ability as usize + 0xb8) as *mut u32) = at(0xe6d7a0) as u32;
    std::mem::transmute(at(0xe6d7a0))
}
#[test]
#[ignore = "requires exact C18 retail fixtures"]
fn c18_owner_hit_native_vs_guard_and_rollback() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let original = fixture(
            "owner_hit_tail.bin",
            "2ed5b49c8a6a30fbe34afd3fad89ae1b608083afd4d94c36769287f79610d286",
        );
        for variant in ["BASELINE", "MODIFIED", "ROLLBACK"] {
            let mut f = Fixture::new();
            let flow = ActionFlow::default();
            ticket(&mut f, &flow);
            cancel_code(&f);
            ACTIVE_FLOW.with(|p| p.set(&flow as *const _ as usize));
            let mut code = original[25..38].to_vec();
            code.push(0xc3);
            if variant == "MODIFIED" {
                code[5..11].copy_from_slice(
                    &owner_hit_patch(&original, guarded_cancel as *const () as usize).unwrap(),
                );
            }
            let at = f.base + 0x300;
            put(at, &code);
            FlushInstructionCache(GetCurrentProcess(), at as _, code.len());
            let call: unsafe extern "thiscall" fn(usize) = std::mem::transmute(at);
            call(f.armor.as_ptr() as usize);
            let retained = f.ability[0xa0 / 4] == f.body() as u32;
            assert_eq!(retained, variant == "MODIFIED");
            println!(
                "C18_OWNER_HIT {} retained={} owner_hp_untouched=true",
                variant, retained
            );
            ACTIVE_FLOW.with(|p| p.set(0));
        }
    }
}
#[test]
#[ignore = "requires exact C18 retail fixtures"]
fn c18_owner_hit_death_control_emp_expiry_and_generic_cancel_stay_native() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for case in [
            "death",
            "controlled",
            "emp",
            "expired",
            "replaced",
            "no_ticket",
            "generic_cancel",
        ] {
            let mut f = Fixture::new();
            let flow = ActionFlow::default();
            ticket(&mut f, &flow);
            let cancel = cancel_code(&f);
            match case {
                "death" => f.pawn[0x60 / 4] |= 0x800,
                "controlled" => f.body[0x910 / 4] = 2,
                "emp" => f.body[0xb68 / 4] = 1,
                "expired" => f.ability[0x7c / 4] = 0,
                "replaced" => f.body[0x9fc / 4] = 0,
                "no_ticket" => flow.cancel(f.ability()),
                _ => {}
            }
            ACTIVE_FLOW.with(|p| p.set(&flow as *const _ as usize));
            if case == "generic_cancel" {
                cancel(f.ability());
            } else {
                guarded_cancel(f.ability());
            }
            assert_eq!(f.ability[0xa0 / 4], 0, "{case}");
            println!("C18_OWNER_HIT native_cancel={} cleared=true", case);
            ACTIVE_FLOW.with(|p| p.set(0));
        }
    }
}
#[test]
#[ignore = "requires exact C18 retail fixtures"]
fn c18_owner_hit_patch_fails_closed_on_bytes_or_pointer() {
    let original = fixture(
        "owner_hit_tail.bin",
        "2ed5b49c8a6a30fbe34afd3fad89ae1b608083afd4d94c36769287f79610d286",
    );
    assert!(owner_hit_patch(&original, 0).is_none());
    for i in 0..original.len() {
        let mut bad = original.clone();
        bad[i] ^= 1;
        assert!(owner_hit_patch(&bad, 0x12345678).is_none());
    }
}
type Emp = unsafe extern "thiscall" fn(usize, usize, f32, u32, u32, u32);
unsafe fn emp_code(f: &mut Fixture) -> Emp {
    let at = |v| f.base + v - 0x400000;
    let mut code = fixture(
        "emp_body.bin",
        "dd99cd6345de8ed1874e8fd609c2e5f52590dec1b347ac23ccf97b2a9bd21d8c",
    );
    // Relocate only absolute constant references; relative calls retain arena RVA.
    for i in 0..code.len() - 3 {
        if code[i..i + 4] == 0x2d75c00u32.to_le_bytes() {
            code[i..i + 4].copy_from_slice(&(at(0x2d75c00) as u32).to_le_bytes());
        }
    }
    *((at(0x2d75c00)) as *mut f32) = 1.0;
    put(at(0xaea010), &code);
    let manager = f.base + 0x800;
    let vtable = f.base + 0xa00;
    let fx = f.base + 0xc00;
    let fxvt = f.base + 0xe00;
    *(manager as *mut u32) = vtable as u32;
    *(fx as *mut u32) = fxvt as u32;
    f.body[0x148 / 4] = manager as u32;
    f.body[0x8c / 4] = f.pawn.as_ptr() as u32;
    let mut spawn = vec![0xb8];
    spawn.extend((fx as u32).to_le_bytes());
    spawn.extend([0xc2, 60, 0]);
    put(f.base + 0x600, &spawn);
    *((vtable + 0x9c) as *mut u32) = (f.base + 0x600) as u32;
    put(f.base + 0x620, &[0xc2, 20, 0]);
    put(f.base + 0x630, &[0xc2, 4, 0]);
    *((fxvt + 0x13c) as *mut u32) = (f.base + 0x620) as u32;
    *((fxvt + 0x130) as *mut u32) = (f.base + 0x630) as u32;
    // Owner feedback lookup and scoring manager are read-only native leaves.
    put(at(0xe93df0), &[0xb8, 1, 0, 0, 0, 0xc3]);
    put(at(0xe8ca30), &[0xb8, 1, 0, 0, 0, 0xc3]);
    jump(at(0xa32bc0), feedback as *const () as usize);
    jump(at(0xd873f0), disable_event as *const () as usize);
    jump(at(0xef7520), score as *const () as usize);
    FlushInstructionCache(GetCurrentProcess(), f.base as _, 0x3000000);
    std::mem::transmute(at(0xaea010))
}
#[test]
#[ignore = "requires exact C18 retail fixtures"]
fn c18_body_emp_preserves_feedback_score_and_completes_parked_fall() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for variant in ["BASELINE", "MODIFIED", "ROLLBACK"] {
            let mut f = Fixture::new();
            f.park();
            f.body[0x4c / 4] = 1 << 8;
            let native = emp_code(&mut f);
            FEEDBACK.store(0, Ordering::SeqCst);
            DISABLE.store(0, Ordering::SeqCst);
            SCORES.store(0, Ordering::SeqCst);
            let call = || native(f.body(), 0x1357, 4.5, 17, 19, 23);
            if variant == "MODIFIED" {
                assert!(
                    parked_emp_dispatch(
                        f.body(),
                        f.layout,
                        |_| false,
                        call,
                        |p| {
                            native_fall(p);
                            true
                        }
                    )
                    .1
                );
            } else {
                call();
            }
            assert_eq!(FEEDBACK.load(Ordering::SeqCst), 1);
            assert_eq!(DISABLE.load(Ordering::SeqCst), 1);
            assert_eq!(SCORES.load(Ordering::SeqCst), 1);
            assert_eq!(f.body[0x910 / 4], if variant == "MODIFIED" { 3 } else { 1 });
            println!(
                "C18_BODY_EMP {} state={} feedback=1 disable=1 score=1",
                variant,
                f.body[0x910 / 4]
            );
            // Native b68 duplicate handling survives the bridge.
            assert!(
                !parked_emp_dispatch(
                    f.body(),
                    f.layout,
                    |_| false,
                    || native(f.body(), 0x1357, 4.5, 17, 19, 23),
                    |_| panic!("duplicate fall")
                )
                .1
            );
            assert_eq!(SCORES.load(Ordering::SeqCst), 1);
        }
    }
}
#[test]
#[ignore = "requires exact native exit fixture"]
fn c18_body_emp_controlled_replaced_foreign_or_native_fell_never_forces_fall() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for case in [
            "controlled",
            "replaced",
            "native_fell",
            "foreign",
            "duplicate",
            "destroyed",
            "owner_replaced",
            "control_entered",
        ] {
            let mut f = Fixture::new();
            f.park();
            f.body[0x4c / 4] = 1 << 8;
            match case {
                "foreign" => f.body[0] = 0,
                "duplicate" => f.body[0xb68 / 4] = 1,
                "destroyed" => f.body[0x4b0 / 4] |= 0x80000000,
                _ => {}
            }
            let entered = Cell::new(case == "controlled");
            let calls = Cell::new(0);
            let result = parked_emp_dispatch(
                f.body(),
                f.layout,
                |_| entered.get(),
                || {
                    calls.set(calls.get() + 1);
                    match case {
                        "replaced" => f.ability[0xa0 / 4] = 0,
                        "native_fell" => f.body[0x910 / 4] = 3,
                        "owner_replaced" => f.body[0x9fc / 4] = 0,
                        "control_entered" => entered.set(true),
                        _ => {}
                    }
                    27
                },
                |_| panic!("unowned forced fall: {case}"),
            );
            assert_eq!(result, (27, false));
            assert_eq!(calls.get(), 1);
        }
    }
}

// C19: body EMP must not deliver the remote drone's screen feedback to a
// mercenary who has already exited control. Owner's own EMP path is untouched.
thread_local! {
    static C19_CONTEXT: Cell<Option<(Layout, bool)>> = const { Cell::new(None) };
}
unsafe extern "C" fn c19_feedback_lookup(body: usize) -> usize {
    let (layout, controlled) = C19_CONTEXT.with(|p| p.get().unwrap());
    emp_feedback_owner_with(body, layout, |_| controlled, |_| 1)
}
unsafe fn c19_emp_code(f: &mut Fixture) -> Emp {
    emp_code(f); // Identical isolated engine leaves to C18, same exact byte input.
    let original = fixture(
        "emp_body.bin",
        "dd99cd6345de8ed1874e8fd609c2e5f52590dec1b347ac23ccf97b2a9bd21d8c",
    );
    let entry = f.base + 0x1600;
    let mut code =
        emp_feedback_window(&original, entry, c19_feedback_lookup as *const () as usize).unwrap();
    for (off, target) in [
        (0xa, 0xe69000),
        (0x12c, 0xa32bc0),
        (0x147, 0xd873f0),
        (0x165, 0xe8ca30),
        (0x174, 0xef7520),
    ] {
        let actual = (entry + off + 5).wrapping_add_signed(i32::from_le_bytes(
            code[off + 1..off + 5].try_into().unwrap(),
        ) as isize);
        assert_eq!(actual, target);
        code[off + 1..off + 5].copy_from_slice(
            &((f.base + target - 0x400000) as u32)
                .wrapping_sub((entry + off + 5) as u32)
                .to_le_bytes(),
        );
    }
    for i in 0..code.len() - 3 {
        if code[i..i + 4] == 0x2d75c00u32.to_le_bytes() {
            code[i..i + 4].copy_from_slice(&((f.base + 0x2d75c00 - 0x400000) as u32).to_le_bytes());
        }
    }
    put(entry, &code);
    FlushInstructionCache(GetCurrentProcess(), entry as _, code.len());
    std::mem::transmute(entry)
}
#[test]
#[ignore = "requires exact C18 retail fixtures"]
fn c19_native_parked_emp_feedback_baseline_modified_rollback() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for variant in ["BASELINE", "MODIFIED", "ROLLBACK"] {
            let mut f = Fixture::new();
            f.park();
            f.body[0x4c / 4] = 1 << 8;
            let native = if variant == "MODIFIED" {
                c19_emp_code(&mut f)
            } else {
                emp_code(&mut f)
            };
            C19_CONTEXT.with(|v| v.set(Some((f.layout, false))));
            FEEDBACK.store(0, Ordering::SeqCst);
            DISABLE.store(0, Ordering::SeqCst);
            SCORES.store(0, Ordering::SeqCst);
            assert!(
                parked_emp_dispatch(
                    f.body(),
                    f.layout,
                    |_| false,
                    || native(f.body(), 0x1357, 4.5, 17, 19, 23),
                    |p| {
                        native_fall(p);
                        true
                    }
                )
                .1
            );
            let count = FEEDBACK.load(Ordering::SeqCst);
            assert_eq!(count, u32::from(variant != "MODIFIED"));
            assert_eq!(DISABLE.load(Ordering::SeqCst), 1);
            assert_eq!(SCORES.load(Ordering::SeqCst), 1);
            assert_eq!(f.body[0x910 / 4], 3);
            println!(
                "C19_BODY_EMP {} owner_feedback={} disable=1 score=1 state=3",
                variant, count
            );
            // Subsequent EMP still native-deduplicated; no suppressed global effect.
            native(f.body(), 0x1357, 4.5, 17, 19, 23);
            assert_eq!(FEEDBACK.load(Ordering::SeqCst), count);
            assert_eq!(SCORES.load(Ordering::SeqCst), 1);
            feedback(0x2a, 0, 1.0, 0.0, 1);
            assert_eq!(FEEDBACK.load(Ordering::SeqCst), count + 1);
            C19_CONTEXT.with(|v| v.set(None));
        }
    }
}
#[test]
#[ignore = "requires exact C18 retail fixtures"]
fn c19_controlled_native_emp_keeps_feedback_and_other_events() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for (state, controlled) in [(2, true), (1, true)] {
            let mut f = Fixture::new();
            f.body[0x910 / 4] = state;
            f.body[0x4c / 4] = 1 << 8;
            let native = c19_emp_code(&mut f);
            C19_CONTEXT.with(|v| v.set(Some((f.layout, controlled))));
            FEEDBACK.store(0, Ordering::SeqCst);
            DISABLE.store(0, Ordering::SeqCst);
            SCORES.store(0, Ordering::SeqCst);
            native(f.body(), 0x1357, 4.5, 17, 19, 23);
            assert_eq!(FEEDBACK.load(Ordering::SeqCst), 1);
            assert_eq!(DISABLE.load(Ordering::SeqCst), 1);
            assert_eq!(SCORES.load(Ordering::SeqCst), 1);
            println!(
                "C19_CONTROLLED state={} owner_feedback=1 disable=1 score=1",
                state
            );
        }
        C19_CONTEXT.with(|v| v.set(None));
    }
}
#[test]
#[ignore = "requires exact native exit fixture"]
fn c19_feedback_unknown_replaced_dead_or_in_control_uses_native_lookup() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for case in [
            "foreign",
            "owner_replaced",
            "body_replaced",
            "dead",
            "falling",
            "remote",
            "controlled",
            "replacement_during_query",
        ] {
            let mut f = Fixture::new();
            f.park();
            f.body[0x4c / 4] = 1 << 8;
            match case {
                "foreign" => f.body[0] = 0,
                "owner_replaced" => f.ability[0x6c / 4] = 0,
                "body_replaced" => f.ability[0xa0 / 4] = 0,
                "dead" => f.pawn[0x60 / 4] |= 0x800,
                "falling" => f.body[0x910 / 4] = 3,
                "remote" => f.body[0x4c / 4] = 2 << 8,
                _ => {}
            }
            let calls = Cell::new(0);
            assert_eq!(
                emp_feedback_owner_with(
                    f.body(),
                    f.layout,
                    |_| {
                        if case == "replacement_during_query" {
                            f.ability[0xa0 / 4] = 0;
                        }
                        case == "controlled"
                    },
                    |p| {
                        assert_eq!(p, f.pawn.as_ptr() as usize);
                        calls.set(calls.get() + 1);
                        57
                    }
                ),
                57,
                "{case}"
            );
            assert_eq!(calls.get(), 1);
        }
    }
}
#[test]
#[ignore = "requires exact C18 retail fixture"]
fn c19_emp_window_only_changes_local_lookup_and_rejects_mismatch() {
    let original = fixture(
        "emp_body.bin",
        "dd99cd6345de8ed1874e8fd609c2e5f52590dec1b347ac23ccf97b2a9bd21d8c",
    );
    let patched = emp_feedback_window(&original, 0xaea010, 0x12345678).unwrap();
    assert_eq!(patched[0xfe], 0x56);
    for i in 0..385 {
        if i != 0xfe && !(0x100..0x104).contains(&i) {
            assert_eq!(original[i], patched[i], "{i}");
        }
        let mut wrong = original.clone();
        wrong[i] ^= 1;
        assert!(emp_feedback_window(&wrong, 0xaea010, 0x12345678).is_none());
    }
    assert!(emp_feedback_window(&original, 0, 0x12345678).is_none());
    assert!(emp_feedback_window(&original, 0xaea010, 0).is_none());
}

#[path = "drone_control_c23_tests.rs"]
mod c23;
