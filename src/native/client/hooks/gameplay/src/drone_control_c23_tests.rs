//! C23 exact native damage/event windows. Only engine transport/score leaves
//! are isolated; this does not claim live two-machine replication or visuals.
use super::*;
static ACCEPT: AtomicBool = AtomicBool::new(true);
static PARENT_CALLS: AtomicU32 = AtomicU32::new(0);
thread_local! { static LAYOUT: Cell<Option<Layout>> = const { Cell::new(None) }; }
type Hit = unsafe extern "thiscall" fn(usize,u32,usize,usize,usize,usize,usize)->u32;
unsafe extern "thiscall" fn hit_allowed(_:usize,_:u32,_:usize)->u32 {
    u32::from(ACCEPT.load(Ordering::SeqCst))
}
unsafe extern "thiscall" fn parent_hit(_:usize,_:u32,_:usize,_:usize,_:usize,_:usize,_:usize)->u32 {
    PARENT_CALLS.fetch_add(1,Ordering::SeqCst); 9
}
unsafe extern "thiscall" fn sound(_:usize,_:usize,_:u32,_:f32,_:u32,_:u32)->u32 {1}
unsafe extern "thiscall" fn event_native(_:usize,name:usize,a:usize,b:usize)->u32 {
    assert_eq!((name,a,b),(0x339f660,0,0));
    DISABLE.fetch_add(1,Ordering::SeqCst); 0x1234
}
unsafe extern "thiscall" fn event_bridge(body:usize,owner:usize,name:usize,a:usize,b:usize)->u32 {
    let layout=LAYOUT.with(|p|p.get().unwrap());
    let (result,_) = parked_electric_dispatch(body,owner,layout, |_|false,
        ||event_native(owner,name,a,b), |p|{native_fall(p);true});
    assert_eq!(result,0x1234);
    result
}
unsafe extern "thiscall" fn damage_cancel(a:usize,info:usize) {
    let keep=ACTIVE_FLOW.with(|p|p.get()!=0 && retain_parked_for_damage(&*(p.get() as *const ActionFlow),a,info));
    if !keep {
        let native:unsafe extern "thiscall" fn(usize)=
            std::mem::transmute(*(*(a as *const usize) as *const usize).add(0xb8/4));
        native(a);
    }
}
fn exact(name:&str, hash:&str)->Vec<u8> {
    let path=std::path::PathBuf::from(std::env::var("SCBL_R58_C23_FIXTURE").unwrap());
    let b=std::fs::read(path.join(name)).unwrap();
    assert_eq!(format!("{:x}",Sha256::digest(&b)),hash);b
}
unsafe fn owner_call(f:&Fixture, modern:bool)->unsafe extern "thiscall" fn(usize,usize) {
    let original=fixture("owner_hit_tail.bin","2ed5b49c8a6a30fbe34afd3fad89ae1b608083afd4d94c36769287f79610d286");
    let mut native=original[25..38].to_vec();
    let target=if modern {
        let thunk=owner_hit_context_thunk(damage_cancel as *const () as usize).unwrap();
        put(f.base+0x400,&thunk);f.base+0x400
    }else{guarded_cancel as *const () as usize};
    native[5..11].copy_from_slice(&owner_hit_patch(&original,target).unwrap());
    let mut code=vec![0x53,0x8b,0x5c,0x24,0x08]; // preserve EBX; actual DamageInfo argument
    code.extend(native);code.extend([0x5b,0xc2,0x04,0x00]);
    put(f.base+0x300,&code);
    FlushInstructionCache(GetCurrentProcess(),f.base as _,0x500);
    std::mem::transmute(f.base+0x300)
}
unsafe fn body_code(f:&mut Fixture,modern:bool)->Hit {
    let at=|v|f.base+v-0x400000;
    let mut code=exact("body_electric.bin","c8f002f2d592b8c5df2923338ea8418e7894e540ee4a9cc64cfb6f5077c92114");
    if modern {
        let target=f.base+0x500;
        put(target,&body_electric_context_thunk(event_bridge as *const () as usize).unwrap());
        let patch=body_electric_patch(&code,target).unwrap();
        // Same native callsite relocated together with all instruction fixtures.
        let mut relocated=patch;
        relocated[1..].copy_from_slice(&(target as u32).wrapping_sub((at(BODY_ELECTRIC_CALL)+5) as u32).to_le_bytes());
        code[0x18a..0x18f].copy_from_slice(&relocated);
    }
    put(at(0xb2a5f0),&code);
    put(at(0x8dcba0),&exact("electric_match.bin","578b7a9debeb033ff47b05d85c6476a267327da10e56562a6f9c17d6ecdb78a2"));
    put(at(0x8c86f0),&[0x8b,0x44,0x24,0x04,0x83,0xe0,0x0f,0xc3]);
    put(at(0xe8ca30),&[0xb8,1,0,0,0,0xc3]);
    for (va,target) in [(0xd873f0,event_native as *const () as usize),
        (0xef7520,score as *const () as usize),(0xf1b310,parent_hit as *const () as usize),
        (0xb07790,disable_event as *const () as usize)] {
        // B07790 isn't Disable: caller args(event data,flags), different ABI.
        if va!=0xb07790 {jump(at(va),target);}
    }
    put(at(0xb07790),&[0xc2,8,0]);
    *((f.layout.body as usize+0x698) as *mut u32)=hit_allowed as *const () as u32;
    *((f.layout.body as usize+0x368) as *mut u32)=sound as *const () as u32;
    LAYOUT.with(|p|p.set(Some(f.layout)));
    FlushInstructionCache(GetCurrentProcess(),f.base as _,0x3000000);
    std::mem::transmute(at(0xb2a5f0))
}
#[test]
#[ignore="requires exact C23 native fixtures"]
fn c23_same_input_owner_arrow_native_cancel_and_bullet_retention() {
    let _lock=LOCK.lock().unwrap();
    unsafe {
        for variant in ["BASELINE","MODIFIED","ROLLBACK"] {
            for damage in [0x16,0x26,0x36,1,2,6,0x46,0x86,0x106] {
                let mut f=Fixture::new();let flow=ActionFlow::default();
                ticket(&mut f,&flow);cancel_code(&f);
                ACTIVE_FLOW.with(|p|p.set(&flow as *const _ as usize));
                let mut info=[0u32;20];info[0x40/4]=damage;
                owner_call(&f,variant=="MODIFIED")(f.armor.as_ptr() as usize,info.as_ptr() as usize);
                let retained=f.ability[0xa0/4]==f.body() as u32;
                assert_eq!(retained,variant!="MODIFIED"||!drone_disabling_electric(damage));
                if damage==0x26 || damage==1 {
                    println!("C23_OWNER {variant} damage={damage:#x} retained={retained} owner_damage_unchanged=true");
                }
                ACTIVE_FLOW.with(|p|p.set(0));
            }
        }
    }
}
#[test]
#[ignore="requires exact C23 native fixtures"]
fn c23_same_input_body_arrow_preserves_native_score_and_completes_disable() {
    let _lock=LOCK.lock().unwrap();
    unsafe {
        for variant in ["BASELINE","MODIFIED","ROLLBACK"] {
            let mut f=Fixture::new();let flow=ActionFlow::default();ticket(&mut f,&flow);
            ACCEPT.store(true,Ordering::SeqCst);DISABLE.store(0,Ordering::SeqCst);
            SCORES.store(0,Ordering::SeqCst);FALLS.store(0,Ordering::SeqCst);
            PARENT_CALLS.store(0,Ordering::SeqCst);
            let damage=0x26u32;let call=body_code(&mut f,variant=="MODIFIED");
            let hp=f.body[0x5dc/4];
            assert_eq!(call(f.body(),0,&damage as *const _ as usize,0,0,0,0x1357),0);
            assert_eq!(f.body[0x910/4],if variant=="MODIFIED"{3}else{1});
            assert_eq!(DISABLE.load(Ordering::SeqCst),1);
            assert_eq!(SCORES.load(Ordering::SeqCst),1);
            assert_eq!(PARENT_CALLS.load(Ordering::SeqCst),0);
            assert_eq!(f.body[0x5dc/4],hp);
            println!("C23_BODY {variant} state={} disable=1 score=1 parent_damage=0 hp_unchanged=true",f.body[0x910/4]);
            call(f.body(),0,&damage as *const _ as usize,0,0,0,0x1357);
            assert_eq!(SCORES.load(Ordering::SeqCst),1);
            assert_eq!(FALLS.load(Ordering::SeqCst),u32::from(variant=="MODIFIED"));
        }
    }
}
#[test]
#[ignore="requires exact C23 native fixtures"]
fn c23_rejected_and_ordinary_damage_do_not_enter_body_bridge() {
    let _lock=LOCK.lock().unwrap();
    unsafe {
        for (accepted,damage) in [(false,0x26),(true,1),(true,2),(true,6),(true,0x46)] {
            let mut f=Fixture::new();let flow=ActionFlow::default();ticket(&mut f,&flow);
            ACCEPT.store(accepted,Ordering::SeqCst);DISABLE.store(0,Ordering::SeqCst);
            SCORES.store(0,Ordering::SeqCst);FALLS.store(0,Ordering::SeqCst);
            PARENT_CALLS.store(0,Ordering::SeqCst);
            let call=body_code(&mut f,true);
            assert_eq!(call(f.body(),1,&damage as *const _ as usize,0,0,0,0x1357),9);
            assert_eq!(f.body[0x910/4],1);
            assert_eq!(FALLS.load(Ordering::SeqCst),0);
            assert_eq!(DISABLE.load(Ordering::SeqCst),0);
            assert_eq!(SCORES.load(Ordering::SeqCst),0);
            assert_eq!(PARENT_CALLS.load(Ordering::SeqCst),1);
        }
        ACCEPT.store(true,Ordering::SeqCst);
    }
}
#[test]
#[ignore="requires exact C23 native fixtures"]
fn c23_electric_match_equals_retail_predicate() {
    let _lock=LOCK.lock().unwrap();
    unsafe {
        let f=Fixture::new();
        let code=exact("electric_match.bin","578b7a9debeb033ff47b05d85c6476a267327da10e56562a6f9c17d6ecdb78a2");
        put(f.base+0x300,&code);
        let call:unsafe extern "C" fn(u32,u32,u32)->u8=std::mem::transmute(f.base+0x300);
        for damage in (0..4096).chain([u32::MAX,0xffff_ff26,0xffff_ff16]) {
            assert_eq!(drone_disabling_electric(damage),call(damage,6,1)!=0||call(damage,6,2)!=0);
        }
    }
}
#[test]
#[ignore="requires exact C23 native fixtures"]
fn c23_parked_body_identity_and_control_fail_closed() {
    let _lock=LOCK.lock().unwrap();
    unsafe {
        for case in ["controlled","foreign","changed","source_changed","already_fell","replica","native_fell"] {
            let mut f=Fixture::new();let flow=ActionFlow::default();ticket(&mut f,&flow);
            let body=f.body();let owner=f.pawn.as_ptr() as usize;let ability=f.ability();
            match case {
                "controlled"=>f.body[0x910/4]=2,
                "foreign"=>f.body[0x9fc/4]=0,
                "already_fell"=>f.body[0x910/4]=3,
                "replica"=>f.body[0x4c/4]=2<<8,
                _=>{}
            }
            let calls=Cell::new(0);
            let (ret,done)=parked_electric_dispatch(body,owner,f.layout, |_|case=="controlled",||{
                calls.set(calls.get()+1);
                if case=="changed"{*((ability+0xa0) as *mut u32)=0;}
                if case=="source_changed"{*((body+0x2c0) as *mut u32)=123;}
                if case=="native_fell"{*((body+0x910) as *mut u32)=3;}
                42u32
            },|_|panic!("must not deliver another fall: {case}"));
            assert_eq!((ret,done,calls.get()),(42,false,1));
        }
    }
}
#[test]
#[ignore="requires exact C23 native fixtures"]
fn c23_unknown_owner_damage_preserves_native_cancel() {
    let _lock=LOCK.lock().unwrap();
    unsafe {
        let mut f=Fixture::new();let flow=ActionFlow::default();ticket(&mut f,&flow);cancel_code(&f);
        ACTIVE_FLOW.with(|p|p.set(&flow as *const _ as usize));
        owner_call(&f,true)(f.armor.as_ptr() as usize,0);
        assert_eq!(f.ability[0xa0/4],0);
        ACTIVE_FLOW.with(|p|p.set(0));
    }
}
#[test]
#[ignore="requires exact C23 native fixtures"]
fn c23_exact_callsite_and_context_thunks_reject_mismatch() {
    let code=exact("body_electric.bin","c8f002f2d592b8c5df2923338ea8418e7894e540ee4a9cc64cfb6f5077c92114");
    let patch=body_electric_patch(&code,0x12345678).unwrap();
    assert_eq!(patch[0],0xe8);
    assert_eq!((BODY_ELECTRIC_CALL as u32+5).wrapping_add(u32::from_le_bytes(patch[1..].try_into().unwrap())),0x12345678);
    for i in 0..code.len() {
        let mut bad=code.clone();bad[i]^=1;
        assert!(body_electric_patch(&bad,0x12345678).is_none());
    }
    assert!(body_electric_patch(&code,0).is_none());
    assert!(owner_hit_context_thunk(0).is_none());
    assert!(body_electric_context_thunk(0).is_none());
}
