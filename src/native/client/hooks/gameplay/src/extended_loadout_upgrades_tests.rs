use super::*;
use super::super::{Sidecar, SidecarRecord, encode_sidecar, decode_sidecar};
use std::sync::atomic::{AtomicUsize, Ordering};
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static FREED: AtomicUsize = AtomicUsize::new(0);
thread_local! { static OBSERVED: std::cell::RefCell<Vec<Selection>> = const { std::cell::RefCell::new(Vec::new()) }; }
fn samples() -> Vec<Selection> {
    vec![
        Selection { item_id: b"MP_UMP45".to_vec(), upgrade_ids: vec![b"Suppressor".to_vec(), b"ExtendedMagazine".to_vec()] },
        Selection { item_id: b"MP_F40".to_vec(), upgrade_ids: vec![] },
        Selection { item_id: b"MP_MAC11".to_vec(), upgrade_ids: vec![b"Ammo".to_vec()] },
    ]
}
fn sidecar(rows: Option<Vec<Selection>>) -> Sidecar {
    Sidecar { records: (0..6).map(|i| SidecarRecord {
        kind: (i / 3 + 1) as u32, slot: (i % 3) as u8,
        display_name: vec![65 + i as u16], gear_ids: vec![b"MP_UMP45".to_vec()],
        upgrades: rows.clone()
    }).collect() }
}
#[test]
fn v2_multiple_weapons_six_slots_preserve_exact_selection_and_empty_deselection() {
    let s = sidecar(Some(samples()));
    let wire = encode_sidecar(&s).unwrap();
    assert_eq!(decode_sidecar(&wire).unwrap(), s);
    assert_eq!(decode_sidecar(&encode_sidecar(&sidecar(Some(vec![]))).unwrap()).unwrap(),
        sidecar(Some(vec![])));
    assert_ne!(sidecar(None), sidecar(Some(vec![])));
}
#[test]
fn v1_retains_existing_native_defaults_without_fabricating_attachment_selection() {
    let s = sidecar(None);
    let mut out = b"SCBLLDO1".to_vec();
    push_u16(&mut out,1);push_u16(&mut out,6);
    for r in &s.records {
        out.extend_from_slice(&r.kind.to_le_bytes());out.extend([r.slot,0]);
        push_u16(&mut out,r.display_name.len() as u16);push_u16(&mut out,r.gear_ids.len() as u16);
        for v in &r.display_name { push_u16(&mut out,*v); }
        for id in &r.gear_ids { put_id(&mut out,id); }
    }
    let hash=Sha256::digest(&out);out.extend_from_slice(&hash);
    assert_eq!(decode_sidecar(&out).unwrap(),s);
    // A missing legacy field is not an explicit request to clear current ones.
    assert!(unsafe { apply_with(std::ptr::null_mut(),None,set_string,reset_string,observe_assign) });
}
#[test]
fn malformed_or_unbounded_attachment_tables_fail_closed() {
    let mut bad=samples();bad[0].item_id=vec![0];
    assert!(encode(&mut vec![],Some(&bad)).is_err());
    bad=samples();bad[0].upgrade_ids=vec![vec![65];MAX_SELECTIONS+1];
    assert!(encode(&mut vec![],Some(&bad)).is_err());
    assert!(decode(&((MAX_ROWS+1) as u16).to_le_bytes(),&mut 0).is_err());
    let mut bytes=vec![];encode(&mut bytes,Some(&samples())).unwrap();
    for n in 0..bytes.len() { assert!(decode(&bytes[..n],&mut 0).is_err(),"truncation {n}"); }
    let mut wire=encode_sidecar(&sidecar(Some(samples()))).unwrap();
    wire[20]^=1;assert!(decode_sidecar(&wire).is_err());
}
unsafe extern "thiscall" fn set_string(out:*mut u8,text:*const u8)->*mut u8 {
    let n=std::ffi::CStr::from_ptr(text.cast()).to_bytes_with_nul().len();
    let data=std::slice::from_raw_parts(text,n).to_vec().into_boxed_slice();
    let address=Box::into_raw(data) as *mut u8 as usize;
    let out=out.cast::<usize>();out.write(address);out.add(1).write(n);out.add(2).write(n);
    ALLOCATED.fetch_add(1,Ordering::SeqCst);out.cast()
}
unsafe extern "thiscall" fn reset_string(out:*mut u8) {
    let out=out.cast::<usize>();let data=out.read();let n=out.add(1).read();
    if data!=0 {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(data as *mut u8,n)));
        FREED.fetch_add(1,Ordering::SeqCst);
    }
    out.write(0);out.add(1).write(0);out.add(2).write(0);
}
unsafe extern "thiscall" fn observe_assign(dest:*mut u8,src:*const u8)->*mut u8 {
    // Inspect the exact ABI table supplied to the native deep-copy entrypoint.
    let mut entry=[0usize;0x60/4];
    std::ptr::copy_nonoverlapping(src,entry.as_mut_ptr().cast::<u8>().add(OFFSET),12);
    OBSERVED.with(|v|*v.borrow_mut()=capture(entry.as_ptr().cast()).unwrap());
    dest
}
#[test]
fn apply_supplies_exact_native_table_without_touching_gear_flags_or_leaking_strings() {
    unsafe {
        let alloc=ALLOCATED.load(Ordering::SeqCst);let freed=FREED.load(Ordering::SeqCst);
        let mut entry=[0usize;0x60/4];entry[0x24/4]=123;entry[0x40/4]=0x010101;
        let before=entry;
        assert!(apply_with(entry.as_mut_ptr().cast(),Some(&samples()),set_string,reset_string,observe_assign));
        OBSERVED.with(|x|assert_eq!(*x.borrow(),samples()));
        assert_eq!(entry,before);
        assert_eq!(ALLOCATED.load(Ordering::SeqCst)-alloc,FREED.load(Ordering::SeqCst)-freed);
        assert!(apply_with(entry.as_mut_ptr().cast(),Some(&[]),set_string,reset_string,observe_assign));
        OBSERVED.with(|x|assert!(x.borrow().is_empty()));
    }
}
#[test]
fn capture_rejects_unreadable_vectors_and_impossible_counts() {
    unsafe {
        assert!(capture(std::ptr::null()).is_none());
        let mut entry=[0usize;0x60/4];
        entry[0x44/4]=1;entry[0x48/4]=1;entry[0x4c/4]=1;
        assert!(capture(entry.as_ptr().cast()).is_none());
        entry[0x48/4]=MAX_ROWS+1;entry[0x4c/4]=MAX_ROWS+1;
        assert!(capture(entry.as_ptr().cast()).is_none());
    }
}
#[test]
fn native_catalog_sized_attachment_table_captures_roundtrips_and_restores_all_six_slots() {
    // The live native loadout contains 95 rows, even with only two guns equipped.
    // Exercise the actual capture/codec/assignment pipeline, not just a count.
    let rows:Vec<_>=(0..95).map(|i| Selection {
        item_id:format!("RetailItem_{i}").into_bytes(),
        upgrade_ids:if i%3==0 {vec![]} else {vec![format!("RetailItem_{i}.SelectedAttachment").into_bytes()]},
    }).collect();
    unsafe {
        let alloc=ALLOCATED.load(Ordering::SeqCst);let freed=FREED.load(Ordering::SeqCst);
        let staged=prepare(&rows,set_string,reset_string).unwrap();
        let mut entry=[0usize;0x60/4];
        entry[OFFSET/4]=staged.rows.as_ptr() as usize;
        entry[OFFSET/4+1]=95;entry[OFFSET/4+2]=95;
        let captured=capture(entry.as_ptr().cast()).unwrap();assert_eq!(captured,rows);
        let saved=sidecar(Some(captured));let wire=encode_sidecar(&saved).unwrap();
        let loaded=decode_sidecar(&wire).unwrap();assert_eq!(loaded,saved);
        for record in loaded.records {
            let mut restored=[0usize;0x60/4];restored[0x28/4]=record.kind as usize;
            let before=restored;
            assert!(apply_with(restored.as_mut_ptr().cast(),record.upgrades.as_deref(),set_string,reset_string,observe_assign));
            OBSERVED.with(|v|assert_eq!(*v.borrow(),rows));assert_eq!(restored,before);
        }
        drop(staged);
        assert_eq!(ALLOCATED.load(Ordering::SeqCst)-alloc,FREED.load(Ordering::SeqCst)-freed);
        println!("C48_SAVE rows=95 six_shadow_slots=roundtrip exact_selections=true empty_deselection=true native_custom_serializer=unchanged");
    }
}
#[test]
fn expanded_table_remains_bounded_and_large_six_slot_codec_is_not_rejected_at_64k() {
    let rows:Vec<_>=(0..MAX_ROWS).map(|i| Selection {
        item_id:format!("Item_{i}_{}", "x".repeat(32)).into_bytes(),
        upgrade_ids:vec![format!("Attachment_{i}_{}", "y".repeat(32)).into_bytes()],
    }).collect();
    let saved=sidecar(Some(rows.clone()));let wire=encode_sidecar(&saved).unwrap();
    assert!(wire.len()>64*1024);assert_eq!(decode_sidecar(&wire).unwrap(),saved);
    let mut too_many=rows;too_many.push(samples()[0].clone());
    assert!(encode(&mut vec![],Some(&too_many)).is_err());
    assert!(decode_sidecar(&vec![0;super::super::MAX_SIDECAR_BYTES+1]).is_err());
}
#[test]
fn absent_empty_and_explicit_attachment_fields_remain_distinct() {
    for value in [None,Some(vec![]),Some(samples())] {
        let mut wire=vec![];encode(&mut wire,value.as_deref()).unwrap();
        let mut at=0;assert_eq!(decode(&wire,&mut at).unwrap(),value);assert_eq!(at,wire.len());
    }
}
#[test]
#[ignore="exact retail serializer and assignment byte fixtures"]
fn retail_per_loadout_table_and_deep_assignment_contract() {
    let e=std::path::PathBuf::from(std::env::var("SCBL_R58_C24_FIXTURE").unwrap());
    let assign=std::fs::read(e.join("native-00f043d0.bin")).unwrap();
    assert_eq!(assign.len(),ASSIGN_BYTES);assert_eq!(format!("{:x}",Sha256::digest(&assign)),ASSIGN_SHA256);
    assert_eq!(&assign[ASSIGN_BYTES-3..],&[0xc2,4,0]); // thiscall, one argument
    let serial=std::fs::read(e.join("native-00ef9a53.bin")).unwrap();
    assert_eq!(format!("{:x}",Sha256::digest(&serial)),"b11c5dbe6447c19f29b8344119df2d4016bc8aae0dfcb7c93e0cc0716cf9e783");
    assert!(serial.windows(3).any(|x|x==[0x8b,0x41,0x44]));
    assert!(serial.windows(3).any(|x|x==[0x83,0xc0,0x0c])); // row+12 upgrades vector
    assert!(serial.windows(3).any(|x|x==[0x83,0xc6,0x18])); // row stride24
}


static EMPTY_ALLOC: AtomicUsize=AtomicUsize::new(0);
static EMPTY_FREE: AtomicUsize=AtomicUsize::new(0);
thread_local! {static COPIED: std::cell::RefCell<Vec<Selection>> = const {std::cell::RefCell::new(Vec::new())};}
unsafe extern "thiscall" fn own_set(dst:*mut u8,id:*const u8)->*mut u8 {
    let bytes=std::ffi::CStr::from_ptr(id.cast()).to_bytes_with_nul();let out=dst.cast::<usize>();
    // Exact native constructor semantics: empty input gives a null, zero-length header.
    if bytes.len()==1 {out.write(0);out.add(1).write(0);out.add(2).write(0);return dst;}
    let bytes=bytes.to_vec().into_boxed_slice();let n=bytes.len();
    out.write(Box::into_raw(bytes) as *mut u8 as usize);out.add(1).write(n);out.add(2).write(n);
    EMPTY_ALLOC.fetch_add(1,Ordering::Relaxed);dst
}
unsafe extern "thiscall" fn own_reset(dst:*mut u8) {
    let h=dst.cast::<usize>();let p=h.read();let n=h.add(1).read();
    if p!=0 {drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(p as *mut u8,n)));EMPTY_FREE.fetch_add(1,Ordering::Relaxed);}
    h.write(0);h.add(1).write(0);h.add(2).write(0);
}
thread_local! {static OWN_DEST: std::cell::RefCell<Option<Prepared>> = const {std::cell::RefCell::new(None)};}
unsafe extern "thiscall" fn own_assign(dst:*mut u8,src:*const u8)->*mut u8 {
    let mut object=[0usize;24];std::ptr::copy_nonoverlapping(src,object.as_mut_ptr().cast::<u8>().add(OFFSET),12);
    let rows=capture(object.as_ptr().cast()).expect("assigned table capture");
    let mut owned=prepare(&rows,own_set,own_reset).unwrap();
    let h=dst.cast::<usize>();h.write(owned.rows.as_mut_ptr() as usize);
    h.add(1).write(owned.rows.len());h.add(2).write(owned.rows.len());
    OWN_DEST.with(|v|*v.borrow_mut()=Some(owned));dst
}
fn source_rows()->Vec<Selection> {vec![
    Selection{item_id:b"OwnedWeapon".to_vec(),upgrade_ids:vec![b"Suppressor".to_vec(),vec![],b"Sight".to_vec()]},
    Selection{item_id:vec![],upgrade_ids:vec![vec![]]},
    Selection{item_id:b"OwnedArmor".to_vec(),upgrade_ids:vec![]},
]}
#[test]fn native_zero_length_id_is_not_a_missing_table(){unsafe{
    let mut children=[[0usize;3];1];let text=b"OwnedWeapon\0";
    let mut row=[text.as_ptr() as usize,text.len(),text.len(),children.as_mut_ptr() as usize,1,1];
    let mut object=[0usize;24];object[OFFSET/4]=row.as_mut_ptr() as usize;object[OFFSET/4+1]=1;object[OFFSET/4+2]=1;
    let captured=capture(object.as_ptr().cast());
    println!("EMPTY_CHILD_CAPTURE accepted={} original_child_count=1",captured.is_some());
    let captured=captured.expect("native legal empty child must not abort the entire loadout save");
    assert_eq!(captured[0].upgrade_ids,vec![Vec::<u8>::new()]);
}}
#[test]fn six_slots_names_gear_and_empty_attachment_slots_survive_owned_roundtrip(){unsafe{
    let before_a=EMPTY_ALLOC.load(Ordering::Relaxed);let before_f=EMPTY_FREE.load(Ordering::Relaxed);
    let rows=source_rows();let staged=prepare(&rows,own_set,own_reset).expect("preserve native empty slots");
    let mut object=[0usize;24];object[OFFSET/4]=staged.rows.as_ptr() as usize;
    object[OFFSET/4+1]=staged.rows.len();object[OFFSET/4+2]=staged.rows.len();
    let captured=capture(object.as_ptr().cast()).unwrap();assert_eq!(captured,rows);
    let side=Sidecar{records:(0..6).map(|i|SidecarRecord{kind:i/3+1,slot:(i%3)as u8,
        display_name:vec![65+i as u16,0],gear_ids:vec![b"OwnedWeapon".to_vec()],upgrades:Some(captured.clone())}).collect()};
    let wire=encode_sidecar(&side).unwrap();let loaded=decode_sidecar(&wire).unwrap();assert_eq!(side,loaded);
    for r in loaded.records {
        let mut dst=[0usize;24];dst[0x28/4]=r.kind as usize;let before=dst;
        assert!(apply_with(dst.as_mut_ptr().cast(),r.upgrades.as_deref(),own_set,own_reset,own_assign));
        assert_eq!(&dst[..OFFSET/4],&before[..OFFSET/4]);
        assert_eq!(capture(dst.as_ptr().cast()).unwrap(),rows);
        OWN_DEST.with(|v|*v.borrow_mut()=None);
    }
    drop(staged);assert_eq!(EMPTY_ALLOC.load(Ordering::Relaxed)-before_a,EMPTY_FREE.load(Ordering::Relaxed)-before_f);
    println!("SIX_SLOT_ROUNDTRIP names_codec=true gear_codec=true attachments_after_staging_drop=true empty_item=true empty_child=true order=true leaks=0");
}}
#[test]fn malformed_native_memory_and_nonempty_nul_identifiers_still_rejected(){unsafe{
    let mut object=[0usize;24];object[OFFSET/4]=1;object[OFFSET/4+1]=1;object[OFFSET/4+2]=1;
    assert!(capture(object.as_ptr().cast()).is_none());
    object[OFFSET/4+1]=MAX_ROWS+1;object[OFFSET/4+2]=MAX_ROWS+1;
    assert!(capture(object.as_ptr().cast()).is_none());
    assert!(encode(&mut vec![],Some(&[Selection{item_id:b"bad\0id".to_vec(),upgrade_ids:vec![]}])).is_err());
    assert!(encode(&mut vec![],Some(&[Selection{item_id:b"OK".to_vec(),upgrade_ids:vec![vec![65];MAX_SELECTIONS+1]}])).is_err());
    println!("NEGATIVE_GUARDS unreadable=true bounds=true embedded_nul=true excess_selection=true");
}}

#[test]
fn empty_slot_v3_does_not_relax_v2_or_equipped_gear_validation() {
    let rows=vec![Selection{item_id:vec![],upgrade_ids:vec![vec![]]}];
    let mut wire=vec![];encode(&mut wire,Some(&rows)).unwrap();
    assert_eq!(decode_versioned(&wire,&mut 0,3).unwrap(),Some(rows));
    assert!(decode_versioned(&wire,&mut 0,2).is_err());
    let original=sidecar(Some(samples()));
    let mut wire=encode_sidecar(&original).unwrap();
    wire[8..10].copy_from_slice(&2u16.to_le_bytes());
    let n=wire.len()-32;let hash=Sha256::digest(&wire[..n]);wire[n..].copy_from_slice(&hash);
    assert_eq!(decode_sidecar(&wire).unwrap(),original);
    let mut bad=sidecar(Some(samples()));bad.records[0].gear_ids[0].clear();
    assert!(encode_sidecar(&bad).is_err());
}

#[test]
fn capture_failure_has_precise_stage_without_string_or_pointer_content() { unsafe {
    let mut entry=[0usize;24];entry[OFFSET/4]=1;entry[OFFSET/4+1]=1;entry[OFFSET/4+2]=1;
    let e=capture_checked(entry.as_ptr().cast()).unwrap_err();
    assert_eq!((e.stage,e.detail),("table","storage"));
    assert_eq!((e.length,e.capacity),(1,1));
    let bad_text=b"not-terminated";
    let mut child=[bad_text.as_ptr() as usize,bad_text.len(),bad_text.len()];
    let item=b"owned-item\0";
    let row=[item.as_ptr() as usize,item.len(),item.len(),child.as_mut_ptr() as usize,1,1];
    entry[OFFSET/4]=row.as_ptr() as usize;
    let e=capture_checked(entry.as_ptr().cast()).unwrap_err();
    assert_eq!((e.stage,e.detail,e.row,e.child),("child","terminator",0,0));
}}

// Real VirtualQuery boundaries, not a mocked successful readability result.
#[link(name="kernel32")]
extern "system" {
    #[link_name="VirtualAlloc"] fn boundary_alloc(p:*mut std::ffi::c_void,n:usize,t:u32,protect:u32)->*mut std::ffi::c_void;
    #[link_name="VirtualFree"] fn boundary_free(p:*mut std::ffi::c_void,n:usize,t:u32)->i32;
    #[link_name="VirtualProtect"] fn boundary_protect(p:*mut std::ffi::c_void,n:usize,protect:u32,old:*mut u32)->i32;
}
struct AdjacentRegions(usize);
impl AdjacentRegions {
    unsafe fn new()->Self {
        let base=boundary_alloc(std::ptr::null_mut(),0x20000,0x2000,4);
        assert!(!base.is_null());assert_ne!(boundary_free(base,0,0x8000),0);
        let a=boundary_alloc(base,0x10000,0x3000,4);assert_eq!(a,base);
        let b=boundary_alloc((base as usize+0x10000) as _,0x10000,0x3000,4);
        if b.is_null(){boundary_free(a,0,0x8000);panic!("cannot reserve adjacent owned fixture region");}
        assert_eq!(b as usize,base as usize+0x10000);Self(base as usize)
    }
    fn split(&self)->usize {self.0+0x10000}
    unsafe fn protect_second(&self,protection:u32){let mut old=0;assert_ne!(boundary_protect(self.split() as _,0x10000,protection,&mut old),0);}
}
impl Drop for AdjacentRegions {fn drop(&mut self){unsafe {boundary_free(self.0 as _,0,0x8000);boundary_free(self.split() as _,0,0x8000);}}}
unsafe fn boundary_entry(p:&AdjacentRegions)->[usize;24]{
    let mut entry=[0usize;24];entry[OFFSET/4]=p.split()-48;
    entry[OFFSET/4+1]=99;entry[OFFSET/4+2]=114;entry
}
#[test]
fn committed_cross_region_table_restores_six_loadouts_without_resetting_names_or_gear(){unsafe{
    let p=AdjacentRegions::new();let mut dst=boundary_entry(&p);
    assert!(!readable(dst[OFFSET/4],99*24),"fixture must expose old single-region rejection");
    let wire=encode_sidecar(&sidecar(Some(samples()))).unwrap();
    let saved=decode_sidecar(&wire).unwrap();
    for record in &saved.records {
        let before=dst;
        let ok=apply_with(dst.as_mut_ptr().cast(),record.upgrades.as_deref(),own_set,own_reset,own_assign);
        println!("CROSS_REGION_RESTORE kind={} slot={} accepted={ok}",record.kind,record.slot);
        assert!(ok,"committed adjacent heap regions are a valid native attachment table");
        assert_eq!(&dst[..OFFSET/4],&before[..OFFSET/4]);
        assert_eq!(capture_checked(dst.as_ptr().cast()).unwrap(),samples());
        OWN_DEST.with(|v|*v.borrow_mut()=None);dst=boundary_entry(&p);
    }
    assert_eq!(decode_sidecar(&encode_sidecar(&saved).unwrap()).unwrap(),saved);
}}
#[test]
fn committed_cross_region_tables_headers_and_strings_are_capturable(){unsafe{
    let p=AdjacentRegions::new();let entry=boundary_entry(&p);
    let captured=capture_checked(entry.as_ptr().cast());
    println!("CROSS_REGION_SAVE accepted={}",captured.is_ok());
    assert_eq!(captured.unwrap().len(),99);
    let text=p.split()-3;std::ptr::copy_nonoverlapping(b"ABCDE\0".as_ptr(),text as *mut u8,6);
    let string=[text,6,6];assert_eq!(read_id(string.as_ptr() as usize,"item",0,0).unwrap(),b"ABCDE");
    let header=p.split()-4;std::ptr::copy_nonoverlapping(string.as_ptr(),header as *mut usize,3);
    assert!(checked_vector(header,1,MAX_ID+1,"item",0,0).is_ok());
}}
#[test]
fn inaccessible_cross_region_storage_remains_rejected(){unsafe{
    let p=AdjacentRegions::new();let mut dst=boundary_entry(&p);
    for protect in [1,4|0x100] {
        p.protect_second(protect);
        assert!(capture_checked(dst.as_ptr().cast()).is_err());
        assert!(!apply_with(dst.as_mut_ptr().cast(),Some(&samples()),own_set,own_reset,own_assign));
        p.protect_second(4);
    }
    // Missing committed tail must also fail; no probe dereferences it.
    assert_ne!(boundary_free(p.split() as _,0x10000,0x4000),0);
    assert!(capture_checked(dst.as_ptr().cast()).is_err());
    assert!(!apply_with(dst.as_mut_ptr().cast(),Some(&samples()),own_set,own_reset,own_assign));
    assert_ne!(boundary_alloc(p.split() as _,0x10000,0x1000,4) as usize,0);
    dst[OFFSET/4]=usize::MAX-16;
    assert!(capture_checked(dst.as_ptr().cast()).is_err());
    println!("CROSS_REGION_GUARDS noaccess=true guard=true uncommitted=true overflow=true");
}}
