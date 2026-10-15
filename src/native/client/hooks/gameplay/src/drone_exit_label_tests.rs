//! Owned-memory guards. Real resource/cold/warm replay is a separate external gate.
use super::*;
use std::cell::Cell;
struct Fixture {
    full:Vec<u32>,short:Vec<u32>,fb:Vec<u16>,sb:Vec<u16>,fc:Vec<u32>,sc:Vec<u32>,fcb:Vec<u16>,scb:Vec<u16>,
}
impl Fixture {
    fn new()->Self {
        let mut s=Self{full:vec![0;3],short:vec![0;3],fb:vec![0;16],sb:vec![0;16],fc:vec![0;648],sc:vec![0;648],fcb:vec![0;16],scb:vec![0;16]};
        for b in [&mut s.fb,&mut s.sb,&mut s.fcb,&mut s.scb] {b[..3].copy_from_slice(&[0x5f15,0x7206,0]);}
        s.full.copy_from_slice(&[s.fb.as_ptr() as u32,3,16]);s.short.copy_from_slice(&[s.sb.as_ptr() as u32,3,16]);
        s.fc[528..531].copy_from_slice(&[s.fcb.as_ptr() as u32,3,16]);s.sc[528..531].copy_from_slice(&[s.scb.as_ptr() as u32,3,16]);s
    }
    unsafe fn run(&mut self,case:&str,apply:bool)->bool {
        let (mut action,mut normal,mut enabled,mut language)=(0xb0,true,true,b"cht\0".as_slice());
        let mut full=self.full.as_ptr() as usize;let mut short=self.short.as_ptr() as usize;
        match case {
            "cht_cold"=>self.full[2]=3,"cht_warm"=>{},"off"=>enabled=false,"int"=>language=b"int\0","unknown_locale"=>language=b"xxx\0",
            "nonnormal"=>normal=false,"action177"=>action=177,"other_action"=>action=88,
            "count_not3"=>self.full[1]=2,"cap_excess"=>self.full[2]=1025,"count_exceeds_cap"=>self.full[2]=2,
            "not_detonate"=>self.fb[0]=0x4e00,"bad_terminator"=>self.fb[2]=1,
            "short_empty"=>self.short[1]=0,"cache_invalid"=>self.fc[529]=0,
            "full_short_header_alias"=>short=full,"full_short_data_alias"=>self.full[0]=self.sb.as_ptr() as u32,
            "short_interior_alias"=>{self.sb[2..5].copy_from_slice(&[0x5f15,0x7206,0]);self.full[0]=self.sb.as_ptr().add(2) as u32;self.full[2]=12;},
            "cache_data_alias"=>self.full[0]=self.fcb.as_ptr() as u32,
            "cache_interior_alias"=>{self.fcb[2..5].copy_from_slice(&[0x5f15,0x7206,0]);self.full[0]=self.fcb.as_ptr().add(2) as u32;self.full[2]=12;},
            "cache_header_alias"=>full=self.fc.as_ptr().add(528) as usize,
            "null_full"=>full=0,
            _=>panic!("unknown case")
        }
        let before=(self.full.clone(),self.short.clone(),self.sb.clone(),self.fc.clone(),self.sc.clone(),self.fcb.clone(),self.scb.clone());
        let calls=Cell::new(0);let original=|a,f,s|{assert_eq!((a,f,s),(action,full,short));calls.set(calls.get()+1);0x13572468};
        let result=if apply {label_with(action,full,short,enabled,normal,language.as_ptr() as usize,self.fc.as_ptr() as usize,self.sc.as_ptr() as usize,original)} else {original(action,full,short)};
        let expected=if case.starts_with("cht_") {[0x9000,0x51fa,0]} else if case=="not_detonate" {[0x4e00,0x7206,0]} else if case=="bad_terminator" {[0x5f15,0x7206,1]} else {[0x5f15,0x7206,0]};
        if case.starts_with("cht_") {println!("C12_TEXT case={} text={}",case,String::from_utf16_lossy(&self.fb[..2]));}
        let unchanged=before==(self.full.clone(),self.short.clone(),self.sb.clone(),self.fc.clone(),self.sc.clone(),self.fcb.clone(),self.scb.clone());
        calls.get()==1 && result==0x13572468 && self.fb[..3]==expected && unchanged
    }
}
const GUARD_CASES:&[&str]=&["cht_cold","cht_warm","off","int","unknown_locale","nonnormal","action177","other_action","count_not3","cap_excess","count_exceeds_cap","not_detonate","bad_terminator","short_empty","cache_invalid","full_short_header_alias","full_short_data_alias","short_interior_alias","cache_data_alias","cache_interior_alias","cache_header_alias","null_full"];
#[test]
fn drone_label_exact_cht_and_guards() {
    assert_eq!(std::mem::size_of::<usize>(),4);
    for case in GUARD_CASES {assert!(unsafe{Fixture::new().run(case,true)},"{}",case);}
}
#[link(name="kernel32")]
extern "system" {fn VirtualAlloc(p:*mut c_void,n:usize,t:u32,prot:u32)->*mut c_void;fn VirtualFree(p:*mut c_void,n:usize,t:u32)->i32;}
unsafe extern "C" fn native_leaf()->usize {7}
unsafe extern "C" fn bridge_leaf()->usize {9}
unsafe fn installer_case()->bool {
    let a=VirtualAlloc(std::ptr::null_mut(),4096,0x3000,4) as usize;assert_ne!(a,0);
    let old=call(a,native_leaf as *const () as usize);std::ptr::copy_nonoverlapping(old.as_ptr(),a as *mut u8,5);(a as *mut u8).add(5).write(0xc3);
    let mut ignored=0;assert_ne!(VirtualProtect(a as _,4096,0x20,&mut ignored),0);
    let mut journal=None;let invoke:unsafe extern "C" fn()->usize=std::mem::transmute(a);
    assert_eq!(invoke(),7);
    assert!(!install_at(a,bridge_leaf as *const () as usize,bridge_leaf as *const () as usize,&mut journal));assert!(journal.is_none());assert_eq!(invoke(),7);
    assert!(install_at(a,native_leaf as *const () as usize,bridge_leaf as *const () as usize,&mut journal));assert_eq!(invoke(),9);
    assert!(restore(&mut journal));assert!(journal.is_none());assert_eq!(invoke(),7);
    assert!(install_at(a,native_leaf as *const () as usize,bridge_leaf as *const () as usize,&mut journal));assert_eq!(invoke(),9);
    assert!(restore(&mut journal));assert_eq!(invoke(),7);assert_ne!(VirtualFree(a as _,0,0x8000),0);true
}
#[test]
fn drone_label_owned_call_install_restore_reinit(){assert!(unsafe{installer_case()});}
