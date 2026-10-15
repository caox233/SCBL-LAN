
pub fn run_lifecycle_fixture()->i32 {
    struct F { pc:Vec<u8>, pawn:Vec<u8>, input:Vec<u8>, manager:Vec<u8>, state:Vec<u8>, world:Vec<u8>, world2:Vec<u8>, map:Vec<u8>, keys:Vec<u8> }
    fn put(b:&mut[u8],o:usize,v:usize){b[o..o+4].copy_from_slice(&(v as u32).to_le_bytes());}
    fn names()->ReloadNames{ReloadNames{reload:101,empty_only:102,auto_timer:103,draw:104}}
    impl F {
        fn new()->Self {
            unsafe{deinit()};
            let mut f=Self{pc:vec![0;0x600],pawn:vec![0;0x1700],input:vec![0;0x200],manager:vec![0;0x60],state:vec![0;0xb0],world:vec![0;0x200],world2:vec![0;0x200],map:vec![0;0x40],keys:vec![0x50,27,0,0,0x4f,29,0,0,0x51,28,0,0,2,42,0,0]};
            for (i,b) in [&mut f.pc,&mut f.pawn,&mut f.input,&mut f.world,&mut f.world2].into_iter().enumerate(){put(b,0x20,100+i);}
            for b in [&f.pc,&f.pawn,&f.input,&f.manager,&f.state,&f.world,&f.world2,&f.map,&f.keys] {
                #[cfg(not(test))]
                crate::equipment_native::RANGES.with(|r|r.borrow_mut().push((b.as_ptr() as usize,b.as_ptr() as usize+b.len())));
                #[cfg(test)] let _ = b;
            }
            put(&mut f.pc,0x144,f.world.as_ptr() as usize);put(&mut f.pawn,0x144,f.world.as_ptr() as usize);
            put(&mut f.pc,0x520,f.pawn.as_ptr() as usize);put(&mut f.pc,0x550,f.map.as_ptr() as usize);
            put(&mut f.map,0x28,f.keys.as_ptr() as usize);put(&mut f.map,0x2c,4);
            put(&mut f.pawn,0x938,f.pc.as_ptr() as usize);put(&mut f.pawn,0x93c,f.input.as_ptr() as usize);
            put(&mut f.pawn,0x374,0x80000);put(&mut f.pawn,0x4b8,f.input.as_ptr() as usize);
            put(&mut f.pawn,0x16e4,f.manager.as_ptr() as usize);put(&mut f.manager,0x28,f.state.as_ptr() as usize);
            put(&mut f.state,0,RELOAD_VTABLE);put(&mut f.state,0x48,f.pawn.as_ptr() as usize);f.input[0x1a0]=1;
            super::LOCAL_PAWN_OBSERVATION.with(|s|s.set(Some((f.pawn.as_ptr() as usize,unsafe {super::GetTickCount64()}))));
            f.latch(); f
        }
        fn dispatch(&self,id:u32)->i32{dispatch_input_with(self.pc.as_ptr(),id,names(),||37)}
        fn latch(&self){self.dispatch(0);POLICY.with(|s|{let mut p=s.get();p.decide(self.pawn.as_ptr() as usize,true,ReloadKind::None,true,true,Action::Fire);s.set(p);});}
        fn changed(&mut self,v:usize){match v {
            0=>put(&mut self.pawn,0x20,201),1=>put(&mut self.pc,0x20,202),
            2=>{put(&mut self.pc,0x144,self.world2.as_ptr() as usize);put(&mut self.pawn,0x144,self.world2.as_ptr() as usize);},
            3=>put(&mut self.world,0x20,203),4=>put(&mut self.input,0x20,204),
            5=>put(&mut self.pawn,0x4b8,self.state.as_ptr() as usize),_=>unsafe{deinit()},
        }}
    }
    impl Drop for F{fn drop(&mut self){unsafe{deinit()};
        #[cfg(not(test))] crate::equipment_native::RANGES.with(|r|r.borrow_mut().clear());
        super::LOCAL_PAWN_OBSERVATION.with(|s|s.set(None));}}
    let mut count=0; let mut failures=0;
    let mut check=|name:&str,ok:bool|{count+=1;if !ok{failures+=1};println!("COMBAT_LIFECYCLE {name}={}",if ok{"PASS"}else{"FAIL"});};
    let notice=[0u8,0,0,0,0xbc,0x02,0,0]; // normal sReload = 700
    {
        let f=F::new();let calls=Cell::new(0);
        check("unchanged_owner_keeps_latch",f.dispatch(0)==37&&POLICY.with(|p|p.get().gate==SprintGate::AwaitRelease));
        let r=reload_complete_with(f.state.as_ptr().cast_mut().cast(),notice.as_ptr(),names(),||700,|_,_|{calls.set(calls.get()+1);73});
        check("normal_completion_once_exact_return",r==73&&calls.get()==1&&POLICY.with(|p|p.get().gate==SprintGate::Ready));
    }
    for v in 0..4 {
        let mut f=F::new();f.changed(v);f.dispatch(0);
        check(["pawn_object_change_resets","controller_object_change_resets","world_replacement_resets","world_object_change_resets"][v],POLICY.with(|p|p.get().gate==SprintGate::Ready));
    }
    for v in 0..7 {
        let mut f=F::new();let calls=Cell::new(0);
        let r=reload_complete_with(f.state.as_ptr().cast_mut().cast(),notice.as_ptr(),names(),||700,|_,_|{calls.set(calls.get()+1);f.changed(v);f.latch();73});
        check(["completion_pawn_replaced","completion_controller_replaced","completion_world_replaced","completion_world_object_replaced","completion_weapon_object_replaced","completion_weapon_pointer_replaced","completion_after_deinit"][v],r==73&&calls.get()==1&&POLICY.with(|p|p.get().gate==SprintGate::AwaitRelease));
    }
    for v in [0,2,6] {
        let mut f=F::new();let mut native=[0u8;0x18];native[0]=1;native[0x14]=1;
        native_reload_finish_with(f.state.as_ptr().cast_mut().cast(),native.as_ptr(),|s,_|{
            f.changed(v);f.latch();reload_complete_with(s,std::ptr::null(),names(),||700,|_,_|1)
        });
        check(if v==0{"null_proof_pawn_replaced"}else if v==2{"null_proof_world_replaced"}else{"null_proof_after_deinit"},POLICY.with(|p|p.get().gate==SprintGate::AwaitRelease));
    }
    {
        let f=F::new();let mut native=[0u8;0x18];native[0]=1;native[0x14]=1;
        native_reload_finish_with(f.state.as_ptr().cast_mut().cast(),native.as_ptr(),|s,n|{
            native_reload_finish_with(s,n,|_,_|{unsafe{deinit()};1});
            check("nested_shutdown_cannot_restore_old_proof",NATIVE_RELOAD_FINISH.with(|v|v.get()==0));1
        });
        check("shutdown_policy_reset",POLICY.with(|s|s.get().gate==SprintGate::Ready));
        f.latch();check("fresh_input_after_shutdown",POLICY.with(|s|s.get().gate==SprintGate::AwaitRelease));
    }
    {
        let f=F::new();super::LOCAL_PAWN_OBSERVATION.with(|s|s.set(None));
        check("nonlocal_edge_passthrough",f.dispatch(27)==37&&POLICY.with(|s|s.get().gate==SprintGate::AwaitRelease));
    }
    println!("COMBAT_LIFECYCLE observations={count} failed={failures}"); i32::from(failures!=0)
}

#[test]
fn lifecycle_fixed_input_regression() { assert_eq!(run_lifecycle_fixture(), 0); }
