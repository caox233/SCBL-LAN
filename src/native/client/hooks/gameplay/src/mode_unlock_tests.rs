use super::*;
#[test]
fn rank_guard_accepts_exact_retail_and_existing_twelve_player_patch_only() {
    let root=std::path::PathBuf::from(std::env::var_os("SCBL_R58_C27_FIXTURE").unwrap());
    let retail=std::fs::read(root.join("selection.bin")).unwrap();
    assert!(selection_contract(&retail));
    let mut twelve=retail.clone();
    assert_eq!(&twelve[433..436], &[0x3b,0x45,0xec]);
    twelve[433..436].copy_from_slice(&[0x83,0xf8,0x0c]);
    assert!(selection_contract(&twelve));
    assert_eq!(&twelve[433..436], &[0x83,0xf8,0x0c]);
    twelve[435]=13;
    assert!(!selection_contract(&twelve));
    for i in [0,100,432,436,720] {
        let mut altered=retail.clone(); altered[i]^=1;
        assert!(!selection_contract(&altered));
    }
    assert!(!selection_contract(&retail[..720]));
}
#[test]
fn projection_changes_only_eight_rank_bytes() {
    let mut row=[0x12345678u32;32];let before=row;
    unsafe {assert!(project(row.as_mut_ptr() as usize));}
    assert_eq!((row[18],row[19]),(0,i32::MAX as u32));
    for i in 0..32 {if i!=18 && i!=19 {assert_eq!(row[i],before[i]);}}
    unsafe {assert!(!project(row.as_mut_ptr() as usize));}
}
#[test]
fn invalid_record_does_not_write() {unsafe {assert!(!project(0));assert!(!project(usize::MAX));}}
#[test]
fn constructor_preserves_args_result_and_native_other_fields() {
    let mut row=[0u32;32];let at=row.as_mut_ptr() as usize;
    let result=constructed_with(at,12,34,56,true,|r,p,i,c| {
        assert_eq!((r,p,i,c),(at,12,34,56));
        row[18]=1;row[19]=50;row[20]=1;row[0]=i;r
    });
    assert_eq!(result,at);assert_eq!(row[0],34);assert_eq!(row[20],1);
    assert_eq!((row[18],row[19]),(0,i32::MAX as u32));
}
#[test]
fn disabled_constructor_and_different_result_are_native() {
    for enabled in [false,true] {
        let mut row=[0u32;32];let at=row.as_mut_ptr() as usize;
        let result=constructed_with(at,1,2,3,enabled,|r,_,_,_| {row[18]=1;row[19]=50;if enabled {0} else {r}});
        assert_eq!((row[18],row[19]),(1,50));
        assert_eq!(result,if enabled {0} else {at});
    }
}
#[test]
fn attribute_always_runs_native_first_once_and_projects_after() {
    let mut row=[0u32;32];let at=row.as_mut_ptr() as usize;let mut n=0;
    attribute_with(at,123,456,true,|r,k,v| {
        n+=1;assert_eq!((r,k,v),(at,123,456));row[18]=10;row[19]=30;row[16]=4;
    });
    assert_eq!(n,1);assert_eq!(row[16],4);assert_eq!((row[18],row[19]),(0,i32::MAX as u32));
}
#[test]
fn disabled_attribute_does_not_unlock() {
    let mut row=[0u32;32];let at=row.as_mut_ptr() as usize;
    attribute_with(at,123,456,false,|_,_,_|{row[18]=10;row[19]=30;});
    assert_eq!((row[18],row[19]),(10,30));
}
#[test]
fn dx9_rejected_before_native_reads() {unsafe {assert!(!init(0));}}
// BEGIN NATIVE FIXTURE
mod native {
    use std::{ffi::{c_void,CStr},path::PathBuf};
    use sha2::{Digest,Sha256};
    // External transaction runner substitutes ONLY this projection, never the oracle.
    const APPLY: bool = true;
    unsafe fn apply(record: usize) { super::super::project(record); }
    #[link(name="kernel32")]
    extern "system" {
        fn VirtualAlloc(p:*mut c_void,n:usize,t:u32,k:u32)->*mut c_void;
        fn VirtualFree(p:*mut c_void,n:usize,t:u32)->i32;
        fn VirtualProtect(p:*mut c_void,n:usize,k:u32,old:*mut u32)->i32;
        fn GetCurrentProcess()->*mut c_void;
        fn FlushInstructionCache(p:*mut c_void,a:*const c_void,n:usize)->i32;
    }
    struct Code(usize,Vec<Vec<u8>>);
    impl Drop for Code {fn drop(&mut self){unsafe {VirtualFree(self.0 as _,0,0x8000);}}}
    unsafe fn seal(at:usize,code:&[u8]) {
        std::ptr::copy_nonoverlapping(code.as_ptr(),at as *mut u8,code.len());
        let mut old=0;assert_ne!(VirtualProtect(at as _,4096,0x20,&mut old),0);
        FlushInstructionCache(GetCurrentProcess(),at as _,code.len());
    }
    unsafe extern "C" fn atoi(p:usize)->i32 {
        CStr::from_ptr(p as _).to_str().unwrap().parse::<i32>().unwrap()
    }
    unsafe extern "C" fn length(p:usize)->u32 {CStr::from_ptr(p as _).to_bytes().len() as u32}
    unsafe extern "C" fn boolean(p:usize)->u8 {(CStr::from_ptr(p as _).to_bytes()==b"true") as u8}
    unsafe extern "thiscall" fn assign(p:usize,text:usize,n:u32)->usize {
        (p as *mut usize).write(text);((p+4) as *mut u32).write(n);p
    }
    unsafe extern "thiscall" fn string_init(p:usize)->usize {std::ptr::write_bytes(p as *mut u8,0,16);p}
    unsafe extern "thiscall" fn container_init(p:usize)->usize {std::ptr::write_bytes(p as *mut u8,0,16);p}
    static COOKIE:u32=0;
    fn dir()->PathBuf {PathBuf::from(std::env::var_os("SCBL_R58_C27_FIXTURE").expect("C27 fixture directory"))}
    unsafe fn load(name:&str)->Code {
        let root=dir();
        let (expected,calls,strings):(&str,&[(usize,usize)],&[(usize,&[u8])])=match name {
            "match_parser"=>("dd2a5a8ee8b70b1e76c0ab0e4cb52f1e943a5045310b143cbb858f5ffb03c9fc",&[(68,0x425ac0),(81,0x613220),(98,0x613220),(161,0x425ac0),(174,0x613220),(191,0x613220),(254,0x425ac0),(267,0x613220),(284,0x613220),(346,0x24c520d),(410,0x24c520d),(474,0x24c520d),(538,0x77de70),(602,0x77de70),(666,0x24c520d),(730,0x24c520d),(894,0x24c520d),(961,0x24c520d),(1034,0x24c520d),(1101,0x24c520d)],&[(10,b"name\0"),(109,b"description\0"),(202,b"image\0"),(295,b"minPlayers\0"),(360,b"maxPlayers\0"),(424,b"maxPartySize\0"),(488,b"voting\0"),(549,b"shuffle\0"),(614,b"minRank\0"),(678,b"maxRank\0"),(742,b"requiresPassport\0"),(794,b"true\0"),(846,b"ladderStartTime\0"),(913,b"ladderEndTime\0"),(980,b"ladderStatboardSet\0"),(1053,b"ladderTrackedStat\0")]),
            "match_ctor"=>("13f1fb82dc0cc1314ab7595bff0744d74f7e6760c2509908abca97279205c23c",&[(57,0x57d4a0),(70,0x57d4a0),(82,0x57d4a0),(144,0x4207c0)],&[]),
            _=>panic!("unknown fixture")
        };
        let mut bytes=std::fs::read(root.join(format!("{name}.bin"))).unwrap();
        assert_eq!(format!("{:x}",Sha256::digest(&bytes)),expected);
        let at=VirtualAlloc(std::ptr::null_mut(),4096,0x3000,4) as usize;assert_ne!(at,0);
        for &(off,target) in calls {
            let f=match target {
                0x24c520d=>atoi as usize,0x425ac0=>length as usize,0x77de70=>boolean as usize,
                0x613220=>assign as usize,0x57d4a0=>string_init as usize,0x4207c0=>container_init as usize,
                x=>panic!("unreviewed native dependency {x:x}"),
            };
            assert_eq!(bytes[off],0xe8);
            bytes[off+1..off+5].copy_from_slice(&(f as u32).wrapping_sub((at+off+5) as u32).to_le_bytes());
        }
        let mut keep=Vec::new();
        for &(off,s) in strings {
            let text=s.to_vec();
            bytes[off..off+4].copy_from_slice(&(text.as_ptr() as u32).to_le_bytes());
            keep.push(text);
        }
        if name=="match_ctor" {
            assert_eq!(bytes[0x15],0xa1);
            bytes[0x16..0x1a].copy_from_slice(&(&COOKIE as *const u32 as u32).to_le_bytes());
        }
        seal(at,&bytes);Code(at,keep)
    }
    unsafe fn gate()->Code {
        let mut min=std::fs::read(dir().join("minimum_gate.bin")).unwrap();
        let mut max=std::fs::read(dir().join("maximum_gate.bin")).unwrap();
        assert_eq!(format!("{:x}",Sha256::digest(&min)),"5dace5a735e3f6a2a2298f5008346f3d7ef4b00af6c3d1f1e794131180eb8ffb");
        assert_eq!(format!("{:x}",Sha256::digest(&max)),"2d24b382c6386b8c7e54a4ce7e9c1167061cfa689da7ff606ebe94a0472161db");
        // cdecl(record,rank): preserve ESI/EDI; native min/max comparisons unchanged.
        let mut code=vec![0x56,0x57,0x8b,0x74,0x24,0x0c,0x8d,0x44,0x24,0x10];
        let start=code.len();let fail=start+min.len();
        let max_at=fail+5;
        min[9..13].copy_from_slice(&((max_at as i32)-(start as i32+13)).to_le_bytes());
        code.extend(min);code.extend([0x31,0xc0,0x5f,0x5e,0xc3]);
        max[5..9].copy_from_slice(&((fail as i32)-(max_at as i32+9)).to_le_bytes());
        code.extend(max);code.extend([0xb8,1,0,0,0,0x5f,0x5e,0xc3]);
        let at=VirtualAlloc(std::ptr::null_mut(),4096,0x3000,4) as usize;assert_ne!(at,0);
        seal(at,&code);Code(at,Vec::new())
    }
    #[test]
    fn c27_same_input_native_match_rank_gate() {
        unsafe {
            let ctor=load("match_ctor");let parser=load("match_parser");let oracle=gate();
            let construct:unsafe extern "thiscall" fn(usize,usize,u32,usize)->usize=std::mem::transmute(ctor.0);
            let parse:unsafe extern "stdcall" fn(usize,usize,usize)=std::mem::transmute(parser.0);
            let eligible:unsafe extern "C" fn(usize,i32)->u32=std::mem::transmute(oracle.0);
            let mut row=[0xccccccccu32;32];let at=row.as_mut_ptr() as usize;
            assert_eq!(construct(at,0x1234,77,0),at);
            let default_bounds=(row[18],row[19]);assert_eq!(default_bounds,(1,50));
            if APPLY {apply(at);}
            let default_999=eligible(at,999);
            let inputs=[(b"minRank\0".as_slice(),b"5\0".as_slice()),(b"maxRank\0",b"56\0"),
                (b"maxPlayers\0",b"8\0"),(b"maxPartySize\0",b"4\0"),(b"requiresPassport\0",b"true\0"),
                (b"voting\0",b"false\0"),(b"shuffle\0",b"true\0"),(b"unrecognizedField\0",b"123\0")];
            for (key,value) in inputs {
                parse(at,key.as_ptr() as usize,value.as_ptr() as usize);
                if APPLY {apply(at);}
            }
            assert_eq!(row[0],77);assert_eq!(row[1],0x1234);assert_eq!(row[14],2);
            assert_eq!(row[15],8);assert_eq!(row[16],4);
            assert_eq!((row[17]&0xffff),0x0100);assert_eq!(row[20]&255,1);
            let rank1=eligible(at,1);let rank5=eligible(at,5);let rank999=eligible(at,999);
            assert_eq!(rank1,APPLY as u32);assert_eq!(rank5,1);assert_eq!(rank999,APPLY as u32);
            assert_eq!(eligible(at,-1),0);
            assert_eq!((row[18],row[19]),if APPLY {(0,i32::MAX as u32)} else {(5,56)});
            // Exercise stdcall ret12 repeatedly; no stack drift, native key parsing retained.
            for _ in 0..1024 {parse(at,b"maxPlayers\0".as_ptr() as _,b"8\0".as_ptr() as _);}
            assert_eq!(row[15],8);
            println!("C27_SAME_INPUT min={} max={} rank1={} rank5={} rank999={} omitted_max_rank999={} passport=1 max_party=4 max_players=8 profile_writes=0 live_menu=false",row[18],row[19],rank1,rank5,rank999,default_999);
        }
    }
}
// END NATIVE FIXTURE
