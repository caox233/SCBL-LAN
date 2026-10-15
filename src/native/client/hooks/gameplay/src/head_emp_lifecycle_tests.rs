
// Fixed observations for both complete pre-change and post-change modules.
thread_local! {
    static FIXTURE_OWNER: std::cell::Cell<*mut HeadFixture> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}
static FIXTURE_FEEDBACK: AtomicU32 = AtomicU32::new(0);
struct HeadFixture {
    pawn:Vec<u32>, source:Vec<u32>, armor:Vec<u32>, part:Vec<u32>, list:Vec<u32>, name:Vec<u8>,
    world:Vec<u32>, world2:Vec<u32>, replacement:Vec<u32>, source_vtable:Vec<u32>, instigator:Vec<u32>, instigator2:Vec<u32>, mode:usize, calls:usize, args:[f32;4],
}
impl HeadFixture {
    fn new(mode:usize)->Self {
        let mut f=Self{pawn:vec![0;0x1b00/4],source:vec![0;0x900/4],armor:vec![0;0x110/4],part:vec![0;0x110/4],list:vec![0],name:NAME.iter().copied().chain(Some(0)).collect(),world:vec![0;0x200/4],world2:vec![0;0x200/4],replacement:vec![0;0x1b00/4],source_vtable:vec![0;0x340/4],instigator:vec![0;0x900/4],instigator2:vec![0;0x900/4],mode,calls:0,args:[0.0;4]};
        f.pawn[0]=MERC; f.pawn[0x374/4]=1<<19;f.pawn[0x1a10/4]=3.0f32.to_bits();f.pawn[0x20/4]=1;f.pawn[0x144/4]=f.world.as_ptr() as u32;
        f.source[0]=CONTROLLER;f.source[0x60/4]=0x40000;f.source[0x20/4]=2;f.source[0x144/4]=f.world.as_ptr() as u32;
        if mode>=9 {
            f.instigator.copy_from_slice(&f.source);f.instigator[0x20/4]=7;
            f.instigator2.copy_from_slice(&f.source);f.instigator2[0x20/4]=8;
            f.source[0]=f.source_vtable.as_ptr() as u32;f.source[0x60/4]=0;f.source[0x5c/4]=0x800000;
            f.source_vtable[0x33c/4]=0x00b8b2e0;f.source[0x358/4]=f.instigator.as_ptr() as u32;
        }
        f.world[0x20/4]=3;f.world2[0x20/4]=4;f.replacement.copy_from_slice(&f.pawn);f.replacement[0x20/4]=5;
        f.armor[0]=ARMOR;f.armor[0x20/4]=6;f.armor[0x2c/4]=f.pawn.as_ptr() as u32;f.armor[0x50/4]=f.list.as_ptr() as u32;f.armor[0x54/4]=1;f.armor[0x58/4]=1;
        f.pawn[0xcf4/4]=f.armor.as_ptr() as u32;f.list[0]=f.part.as_ptr() as u32;
        f.part[0]=PART;f.part[0x28/4]=f.name.as_ptr() as u32;f.part[0x2c/4]=f.name.len() as u32;f.part[0x30/4]=f.name.len() as u32;
        for (i,v) in [0.8f32,1.5,1.5].into_iter().enumerate(){f.part[(0xd8+i*4)/4]=v.to_bits();f.armor[(0xf4+i*4)/4]=v.to_bits();}
        FIXTURE_FEEDBACK.store(0,Ordering::Relaxed);READY.store(true,Ordering::Release);f
    }
    fn enter(&mut self){FIXTURE_OWNER.with(|s|s.set(self as *mut Self));}
}
fn fixture_native_apply(target:usize,source:usize,status:f32,visual:f32,a:f32,b:f32){
    FIXTURE_OWNER.with(|s|unsafe{
        let f=&mut *s.get();assert_eq!(target,f.pawn.as_ptr() as usize);assert_eq!(source,f.source.as_ptr() as usize);
        f.calls+=1;f.args=[status,visual,a,b];
        if f.mode!=7{f.pawn[0x1a08/4]=1;f.pawn[0x1a0c/4]=status.to_bits();}
        match f.mode {
            1=>f.pawn[0x20/4]+=1,2=>f.source[0x20/4]+=1,3=>f.world[0x20/4]+=1,
            4=>f.pawn[0x144/4]=f.world2.as_ptr() as u32,
            5=>deinit(),6=>{deinit();READY.store(true,Ordering::Release);},
            8=>READY.store(true,Ordering::Release),9=>f.source[0x358/4]=f.instigator2.as_ptr() as u32,
            10=>f.instigator[0x20/4]+=1,_=>{},
        }
    });
}
fn fixture_native_aggregate(address:usize){
    FIXTURE_OWNER.with(|s|unsafe{
        let f=&mut *s.get();assert_eq!(address,f.armor.as_ptr() as usize);f.calls+=1;
        for (i,v) in [0.8f32,1.5,1.5].into_iter().enumerate(){f.armor[(0xf4+i*4)/4]=v.to_bits();}
        match f.mode {
            1=>f.armor[0x20/4]+=1,2=>f.pawn[0x20/4]+=1,3=>f.armor[0x2c/4]=f.replacement.as_ptr() as u32,
            4=>f.world[0x20/4]+=1,5=>deinit(),6=>{deinit();READY.store(true,Ordering::Release);},
            8=>READY.store(true,Ordering::Release),_=>{},
        }
    });
}
fn fixture_apply(f:&mut HeadFixture){
    f.enter();let (t,s)=(f.pawn.as_ptr() as usize,f.source.as_ptr() as usize);
    #[cfg(not(test))] apply(t,s,6.0,5.0,0.5,0.25);
    #[cfg(test)] apply_with(t,s,6.0,5.0,0.5,0.25,fixture_native_apply,|_|{FIXTURE_FEEDBACK.fetch_add(1,Ordering::Relaxed);});
}
fn fixture_aggregate(f:&mut HeadFixture){
    f.enter();let a=f.armor.as_ptr() as usize;
    #[cfg(not(test))] aggregate(a);
    #[cfg(test)] aggregate_with(a,fixture_native_aggregate);
}
#[cfg(not(test))]
unsafe extern "thiscall" fn fixture_local(_:usize)->i32{1}
#[cfg(not(test))]
unsafe extern "thiscall" fn fixture_send(event:usize,a:usize,b:usize){
    assert_eq!((event,a,b),(0x0339c048,0,0));FIXTURE_FEEDBACK.fetch_add(1,Ordering::Relaxed);
}
pub fn run_head_fixture()->i32 {
    let(mut observations,mut failed)=(0,0);
    let mut check=|name:&str,ok:bool|{observations+=1;if !ok{failed+=1;}println!("HEAD_CALLBACK {name}={}",if ok{"PASS"}else{"FAIL"});};
    for mode in 0..12 {
        let mut f=HeadFixture::new(mode);if mode==8{READY.store(false,Ordering::Release);}
        fixture_apply(&mut f);
        let wanted=if mode==0||mode==11{1}else{0};let args=if mode==8{[6.0,5.0,0.5,0.25]}else{[1.0,1.0,0.5,0.25]};
        check(["normal_apply_notify_once","apply_target_replaced","apply_source_replaced","apply_world_object_replaced","apply_world_pointer_replaced","apply_stopped","apply_stopped_reenabled","native_rejected_no_feedback","disabled_entry_remains_native","projectile_instigator_pointer_changed","projectile_instigator_object_changed","normal_projectile_instigator"][mode],f.calls==1&&f.args==args&&FIXTURE_FEEDBACK.load(Ordering::Relaxed)==wanted);
    }
    for mode in [0,1,2,3,4,5,6,8] {
        let mut f=HeadFixture::new(mode);if mode==8{READY.store(false,Ordering::Release);}let original_part=f.part.clone();fixture_aggregate(&mut f);
        let expected=if mode==0{[1.0f32;3]}else{[0.8f32,1.5,1.5]};
        check(["normal_aggregate_only_own_bonus","aggregate_armor_replaced","aggregate_owner_object_replaced","aggregate_owner_pointer_replaced","aggregate_world_replaced","aggregate_stopped","aggregate_stopped_reenabled","unused","aggregate_disabled_entry"][mode],f.calls==1&&f.armor[0xf4/4..0x100/4]==expected.map(f32::to_bits)&&f.part==original_part);
    }
    unsafe{deinit()};FIXTURE_OWNER.with(|s|s.set(std::ptr::null_mut()));
    println!("HEAD_CALLBACK observations={observations} failed={failed}");i32::from(failed!=0)
}
#[test]
fn head_callback_lifetime_fixed_regression(){assert_eq!(run_head_fixture(),0);}
