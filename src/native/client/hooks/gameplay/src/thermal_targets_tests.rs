use super::*;
struct Fixture {
    render: Vec<u32>, view: Vec<u32>, pass: Vec<u32>, table: Vec<u32>, shader: Vec<u32>,
    scene: Vec<u32>, primary: Vec<u32>, secondary: Vec<u32>,
    objects: Vec<Vec<u32>>, owners: Vec<Vec<u32>>,
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self {
            render: vec![0; 0x2400/4], view: vec![0; 0x400/4], pass: vec![0; 0x400/4],
            table: vec![0; 10], shader: vec![0x2c094a4], scene: vec![0; 0x400/4],
            primary: vec![0; 4], secondary: vec![0; 8],
            objects: (0..4).map(|_| vec![0; 0x220/4]).collect(),
            owners: (0..4).map(|_| vec![0; 0x310/4]).collect(),
        };
        f.render[0x20d0/4] = f.view.as_ptr() as u32;
        f.render[0x1c/4] = f.scene.as_ptr() as u32;
        f.render[0x2208/4] = u32::MAX;
        f.pass[0x14/4] = f.render.as_ptr() as u32;
        f.pass[8/4] = f.table.as_ptr() as u32;
        f.table[8] = f.shader.as_ptr() as u32;
        f.view[0x340/4] = 8;
        for i in 0..4 {
            f.objects[i][0x20/4] = f.owners[i].as_ptr() as u32;
            f.objects[i][0x210/4] = ([0, 120, 77, 64][i] << 24) | 0x0055cc11;
            f.owners[i][0] = [CAMERA, DRONE, 0x02ad2abc, 0x12345678][i];
            f.owners[i][0x20/4] = (i+1) as u32;
            f.owners[i][0x30c/4] = f.objects[i].as_ptr() as u32;
            f.primary[i] = f.objects[i].as_ptr() as u32;
            f.secondary[i*2] = f.objects[i].as_ptr() as u32;
            f.secondary[i*2+1] = 1;
        }
        f.set_lists(0x70,0xe8);
        f
    }
    fn set_lists(&mut self, p: usize, s: usize) {
        self.scene[p/4] = self.primary.as_ptr() as u32;
        self.scene[p/4+1] = 4; self.scene[p/4+2] = 4;
        self.scene[s/4] = self.secondary.as_ptr() as u32;
        self.scene[s/4+1] = 4; self.scene[s/4+2] = 4;
    }
    fn ptr(&self) -> usize { self.pass.as_ptr() as usize }
    fn categories(&self) -> Vec<u32> { self.objects.iter().map(|o| o[0x210/4]>>24).collect() }
}

#[test]
fn one_thermal_view_includes_camera_excludes_drone_then_restores_all_bytes() {
    let f=Fixture::new(); let before=f.objects.clone();
    unsafe { with_targets(f.ptr(),true,|| {
        assert_eq!(f.categories(),[1,0,77,64]);
        for object in &f.objects { assert_eq!(object[0x210/4]&0x00ffffff,0x0055cc11); }
    }); }
    assert_eq!(f.objects,before);
    assert!(ACTIVE.with(Cell::get).is_null());
}

#[test]
fn every_native_per_view_input_pair_is_supported_without_cross_view_writes() {
    for index in 0..4 {
        let mut f=Fixture::new(); f.render[0x2208/4]=index;
        f.scene[0x74/4]=0;f.scene[0xec/4]=0;
        f.set_lists(0xf4+index as usize*12,0x124+index as usize*12);
        unsafe { with_targets(f.ptr(),true,||assert_eq!(f.categories(),[1,0,77,64])); }
        assert_eq!(f.categories(),[0,120,77,64]);
    }
}

#[test]
fn nested_nonthermal_view_suspends_and_restores_parent_even_during_unwind() {
    let mut f=Fixture::new(); let pass=f.ptr();
    unsafe { with_targets(pass,true,|| {
        assert_eq!(f.categories(),[1,0,77,64]);
        f.view[0x340/4]=9;
        let unwind=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_targets(pass,true,|| {
                assert_eq!(f.categories(),[0,120,77,64]);
                panic!("controlled nested view unwind, not engine exception");
            });
        }));
        assert!(unwind.is_err());
        assert_eq!(f.categories(),[1,0,77,64]);
        f.view[0x340/4]=8;
    }); }
    assert_eq!(f.categories(),[0,120,77,64]);
    assert!(ACTIVE.with(Cell::get).is_null());
}

#[test]
fn native_updates_and_replaced_proxy_owners_are_not_overwritten_by_restore() {
    for replacement in [false,true] {
        let mut f=Fixture::new(); let pass=f.ptr();
        unsafe { with_targets(pass,true,|| {
            if replacement { f.owners[0][0x20/4]=99; }
            f.objects[0][0x210/4] = (88<<24)|0x0055cc11;
        }); }
        assert_eq!(f.categories(),[88,120,77,64]);
    }
}

#[test]
fn disabled_wrong_mode_shader_owner_and_invalid_input_remain_native() {
    for fault in 0..8 {
        let mut f=Fixture::new();
        match fault {
            1=>f.view[0x340/4]=7,
            2=>f.shader[0]=0x2c0965c,
            3=>f.owners[0][0x30c/4]=0,
            4=>f.scene[0x78/4]=0,
            5=>f.render[0x2208/4]=4,
            6=>f.scene[0xe8/4]=1,
            7=>f.objects[0][0x20/4]=1,
            _=>{},
        }
        let before=f.objects.clone();
        unsafe { with_targets(f.ptr(),fault!=0,|| {
            assert_eq!(f.categories()[0],0);
            assert_eq!(f.categories()[1],if fault==3||fault==7 {0}else{120});
        }); }
        assert_eq!(f.objects,before);
    }
}

#[test]
fn duplicate_records_do_not_accumulate_or_leak_across_repeated_draws() {
    let mut f=Fixture::new();f.primary.fill(f.objects[0].as_ptr() as u32);
    for pair in f.secondary.chunks_exact_mut(2) {pair[0]=f.objects[0].as_ptr() as u32;}
    let before=f.objects.clone();
    for _ in 0..1000 {
        unsafe { with_targets(f.ptr(),true,||assert_eq!(f.categories(),[1,120,77,64])); }
        assert_eq!(f.objects,before);
    }
}

#[test]
fn readable_region_cache_does_not_cross_guard_pages_or_survive_draws() {
    unsafe {
        use windows::Win32::System::Memory::*;
        let base=VirtualAlloc(None,0x3000,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE);
        assert!(!base.is_null()); let p=base as usize; let mut old=PAGE_READWRITE;
        VirtualProtect((p+0x1000) as _,0x1000,PAGE_NOACCESS,&mut old).unwrap();
        let mut cache=ReadRanges::default();
        assert!(cache.contains(p,0x1000));assert!(cache.contains(p+8,32));
        assert!(!cache.contains(p+0xff0,32));assert!(!cache.contains(p+0x1000,1));
        assert!(cache.contains(p+0x2000,0x1000));
        assert!(!cache.contains(1,1));assert!(!cache.contains(usize::MAX-4,16));
        // Collection cache is discarded before foreign code can change protection.
        drop(cache);VirtualProtect(base,0x1000,PAGE_NOACCESS,&mut old).unwrap();
        let mut next_draw=ReadRanges::default();assert!(!next_draw.contains(p,1));
        VirtualFree(base,0,MEM_RELEASE).unwrap();
    }
}

#[test]
fn excessive_target_set_keeps_every_proxy_native_instead_of_partial_modification() {
    let mut f=Fixture::new();
    let mut objects:Vec<Vec<u32>>=(0..257).map(|_|vec![0;0x220/4]).collect();
    let mut owners:Vec<Vec<u32>>=(0..257).map(|_|vec![0;0x310/4]).collect();
    for i in 0..257 {
        objects[i][0x20/4]=owners[i].as_ptr() as u32;
        owners[i][0]=CAMERA;owners[i][0x20/4]=i as u32+1;
        owners[i][0x30c/4]=objects[i].as_ptr() as u32;
    }
    let entries:Vec<u32>=objects.iter().map(|o|o.as_ptr() as u32).collect();
    f.scene[0x70/4]=entries.as_ptr() as u32;f.scene[0x74/4]=257;f.scene[0x78/4]=257;
    f.scene[0xec/4]=0;
    let before=objects.clone();
    unsafe {with_targets(f.ptr(),true,||assert_eq!(objects,before));}
    assert_eq!(objects,before);
}

// C8 bounded selected-address index controls; original eight tests stay byte-identical.
// Only input construction is synthetic. No game addresses are dereferenced,
// no validation leaf is mocked, and no category is changed by this experiment.
fn populated(targets: usize, total: usize, duplicate_targets: bool) -> Fixture {
    assert_eq!(std::mem::size_of::<usize>(), 4, "C8_COLLECT_SETUP i686 required");
    assert!(targets > 0 && targets <= MAX_TARGETS && total % (2*targets) == 0);
    let mut f = Fixture::new();
    f.objects = (0..=targets).map(|_|vec![0;0x220/4]).collect();
    f.owners = (0..=targets).map(|_|vec![0;0x310/4]).collect();
    for i in 0..=targets {
        f.objects[i][0x20/4] = f.owners[i].as_ptr() as u32;
        f.objects[i][0x210/4] = (77 << 24) | 0x0055cc11;
        f.owners[i][0] = if i == targets { 0x02ad2abc } else if i % 2 == 0 { CAMERA } else { DRONE };
        f.owners[i][0x20/4] = i as u32 + 1;
        f.owners[i][0x30c/4] = f.objects[i].as_ptr() as u32;
    }
    let address = |i: usize| {
        let index = if i < targets { i } else if duplicate_targets { i % targets } else { targets };
        f.objects[index].as_ptr() as u32
    };
    f.primary = (0..total/2).map(address).collect();
    f.secondary = (total/2..total).flat_map(|i|[address(i),1]).collect();
    f.scene[0x70/4] = f.primary.as_ptr() as u32;
    f.scene[0xe8/4] = f.secondary.as_ptr() as u32;
    for offset in [0x74,0x78,0xec,0xf0] { f.scene[offset/4] = (total/2) as u32; }
    f
}

#[test]
fn capacity256_reverse_addresses_duplicates_preserve_order() {
    let mut f=populated(MAX_TARGETS,MAX_TARGETS*2,true);
    let mut order:Vec<u32>=f.objects[..MAX_TARGETS].iter().map(|x|x.as_ptr() as u32).collect();
    order.sort_unstable_by(|a,b|b.cmp(a));
    f.primary.copy_from_slice(&order);
    for (i,pair) in f.secondary.chunks_exact_mut(2).enumerate() {pair[0]=order[i];}
    let before=f.objects.clone();let mut observation=None;
    unsafe {
        let selected=collect(f.ptr(),false,&mut observation).expect("full selected set stays valid");
        assert_eq!(selected.0.iter().map(|x|x.object as u32).collect::<Vec<_>>(),order,
            "index order must not replace original Swap order");
        let mut calls=0;
        with_targets(f.ptr(),true,|| {
            calls+=1;
            for i in 0..MAX_TARGETS {
                assert_eq!(f.objects[i][0x210/4]>>24,if i%2==0 {1}else{0});
            }
            assert_eq!(f.objects[MAX_TARGETS],before[MAX_TARGETS]);
        });
        assert_eq!(calls,1);
    }
    assert_eq!(f.objects,before);
    assert!(ACTIVE.with(Cell::get).is_null());
}

#[test]
fn malformed_second_array_keeps_full_first_array_native() {
    let mut f=populated(MAX_TARGETS,MAX_TARGETS*2,true);
    f.scene[0xf0/4]=0; // second count exceeds its capacity; first list is valid/full.
    let before=f.objects.clone();let mut calls=0;
    unsafe { with_targets(f.ptr(),true,|| {calls+=1;assert_eq!(f.objects,before);}); }
    assert_eq!(calls,1);
    assert_eq!(f.objects,before);
    assert!(ACTIVE.with(Cell::get).is_null());
}
