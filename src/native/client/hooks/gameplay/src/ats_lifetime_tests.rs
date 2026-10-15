//! Copied-state lifetime, without native memory or a new shot at invalidation.
use super::*;

fn seed() {
    let facts=Facts {
        world:1,info:2,local:3,team:20,
        shot:Shot {scene:1,source:7,serial:0,game_time_ms:1000,team:10,role:Role::Spy,weapon:Weapon::SuppressedPrimary,position:[10.,0.,0.]},
        listeners:vec![Listener {scene:1,team:20,role:Role::Merc,ats_operational:true,position:[0.;3],native_footstep_range:20.,native_duration_ms:5000}],
    };
    let mut s=State {world:1,info:2,local:3,team:20,last:1000,serial:0,marks:Marks::new(1,20)};
    assert!(s.feed(facts));
    *STATE.lock().unwrap()=Some(s);
}
fn read(now:u64)->Vec<Mark>{snapshot(1,2,3,20,Role::Merc,now)}
fn frame(now:u64)->Frame{Frame {world:1,info:2,local:3,team:20,now,position:[0.;3],distance_delay_ms:0,visible:true}}
fn view_snapshot(f:Option<Frame>)->Vec<Mark>{
    #[cfg(test)] { snapshot_frame(f.as_ref()) }
    #[cfg(not(test))] { crate::frame_marks(f) }
}
struct Restore(Option<State>);
impl Drop for Restore {fn drop(&mut self){*STATE.lock().unwrap()=self.0.take();}}

pub(crate) fn observations()->Vec<(&'static str,bool)> {
    let _restore=Restore(STATE.lock().unwrap().take());
    let mut out=Vec::new();
    seed();let first=read(1000);
    out.push(("initial_fixed_mark_is_visible",first.len()==1 && first[0].position==[10.,0.,0.]));
    out.push(("read_does_not_follow_or_extend_mark",read(3000)==first && read(6000).is_empty()));
    seed();out.push(("clock_before_shot_is_empty",read(500).is_empty()));
    out.push(("clock_catchup_without_shot_does_not_revive",read(2000).is_empty()));
    seed();assert_eq!(read(3000).len(),1);
    out.push(("clock_rollback_after_last_view_clears_mark",read(2500).is_empty()));
    out.push(("second_clock_catchup_stays_empty",read(3500).is_empty()));
    for (case,label) in [(0,"scene_return_does_not_revive"),(1,"info_return_does_not_revive"),(2,"local_return_does_not_revive"),(3,"team_return_does_not_revive"),(4,"role_return_does_not_revive")] {
        seed();
        let (world,info,local,team,role)=match case {0=>(9,2,3,20,Role::Merc),1=>(1,9,3,20,Role::Merc),2=>(1,2,9,20,Role::Merc),3=>(1,2,3,10,Role::Merc),_=>(1,2,3,20,Role::Spy)};
        assert!(snapshot(world,info,local,team,role,1100).is_empty());
        out.push((label,read(1200).is_empty()));
    }
    seed();assert_eq!(view_snapshot(Some(frame(1000))).len(),1);
    assert!(view_snapshot(None).is_empty());
    out.push(("missing_view_frame_discards_copied_marks",view_snapshot(Some(frame(1100))).is_empty()));
    seed();let mut hidden=frame(1200);hidden.visible=false;
    out.push(("hidden_valid_hud_preserves_detection_lifetime",view_snapshot(Some(hidden)).len()==1));
    // A genuinely new valid shot can create a new mark after any invalidation.
    seed();out.push(("new_shot_can_create_fresh_mark",read(1000).len()==1));
    out
}

#[test]
fn no_old_snapshot_revival_without_a_new_shot() {
    let checks=observations();for(name,pass)in &checks{println!("{}={}",name,pass);}
    assert!(checks.iter().all(|(_,pass)|*pass),"ATS snapshot lifetime regression");
}
