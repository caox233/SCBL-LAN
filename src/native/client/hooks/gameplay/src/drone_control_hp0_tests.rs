// C7 requirement supersedes immediate Finish: keep these twelve historical case
// names as stable inventory, but drive the shared native DroneEnd/timer fixture.
use super::*;
unsafe fn hp0_case(name:&str,_hp:u32,_body_mode:u32,_explicit_finish:bool) {
    super::c7_disabled::c7_case(if name=="remote" {"remote_hp0"} else {name});
}
#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_flying_exits_at_first_update() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("hp0_flying",0,2,false); } }
#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_already_falling_exits_at_first_update() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("hp0_falling",0,3,false); } }
#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_healthy_native_update_unchanged() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("healthy",600,2,false); } }
#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_native_immediate_finish_control() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("native_finish",0,2,true); } }

#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_reentrant_cancel_finishes_once() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("reentrant",0,2,false); } }

#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_repeat_update_finishes_once() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("repeat",0,2,false); } }

#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_remote_keeps_native_update() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("remote",0,2,false); } }

#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_foreign_body_owner_keeps_native() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("foreign_body_owner",0,2,false); } }

#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_foreign_owner_class_keeps_native() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("foreign_owner_vtable",0,2,false); } }

#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_closed_hud_keeps_native() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("closed_hud",0,2,false); } }

#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_cleared_ability_still_releases_current_control() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("ability_cleared",0,2,false); } }

#[test]
#[ignore = "owned retail machine-code fixture"]
fn c6_hp0_falling_effects_not_restarted() { let _l=LOCK.lock().unwrap_or_else(|e|e.into_inner()); unsafe { hp0_case("falling_no_restart",0,3,false); } }
