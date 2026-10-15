//! Executed only in the isolated test process, not exported by the gameplay DLL.
use std::ffi::c_void;
pub type Get = unsafe extern "system" fn(u32) -> usize;
pub type Read = unsafe extern "system" fn(u32) -> u32;
#[test]
#[ignore = "requires reviewed MSVC x86 fixture; execute explicitly"]
fn native_combat_msvc_thiscall_detours_preserve_stack_and_arguments() {
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(p: *const u16) -> *mut c_void;
        fn GetProcAddress(m: *mut c_void, n: *const u8) -> *mut c_void;
    }
    let p = std::env::var("SCBL_COMBAT_ABI_FIXTURE").unwrap();
    let p: Vec<u16> = p.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let m = LoadLibraryW(p.as_ptr());
        assert!(!m.is_null());
        let g = GetProcAddress(m, b"_get@4\0".as_ptr());
        let n = GetProcAddress(m, b"_count@4\0".as_ptr());
        let v = GetProcAddress(m, b"_value@4\0".as_ptr());
        assert!(!g.is_null() && !n.is_null() && !v.is_null());
        let (g, n, v): (Get, Read, Read) = (
            std::mem::transmute(g),
            std::mem::transmute(n),
            std::mem::transmute(v),
        );
        crate::melee_native::fixture_abi(g, n, v);
        crate::drone_emp::fixture_abi(g, n, v);
        crate::drone_tags::fixture_abi(g, n, v);
        crate::head_emp::fixture_abi(g, n, v);
    }
    println!("NATIVE_COMBAT_ABI MSVC x86: create ret24, finish ret0, body ret20, explosion AL, deliver cdecl, damage ret24, tag ret28, drone-clear ret0, drone-scan ret4, EMP cdecl6, armor-aggregate ret0; 1000 detoured calls each preserve arguments/count/stack; disable restores original.");
}
