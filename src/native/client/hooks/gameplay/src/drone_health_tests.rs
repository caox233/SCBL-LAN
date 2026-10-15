use super::*;
#[test]
fn health_pair_only_and_repeated_native_initialization_not_cumulative() {
    unsafe {
        let mut b = vec![0u32; 0xc00 / 4];
        b[0] = DRONE;
        b[0xb44 / 4] = 100;
        for _ in 0..3 {
            b[0x5dc / 4] = 100;
            b[0x5e0 / 4] = 100;
            let original = b.clone();
            assert_eq!(after_initialize(b.as_mut_ptr() as usize), Some((100, 50)));
            assert_eq!((b[0x5dc / 4], b[0x5e0 / 4]), (50, 50));
            b[0x5dc / 4] = 100;
            b[0x5e0 / 4] = 100;
            assert_eq!(original, b);
        }
        b[0x5dc / 4] = 50;
        assert_eq!(after_initialize(b.as_mut_ptr() as usize), None);
        b[0x5dc / 4] = 100;
        b[0] = 0x2a887dc;
        assert_eq!(after_initialize(b.as_mut_ptr() as usize), None);
        assert_eq!(after_initialize(0), None);
    }
}
#[test]
fn invalid_and_odd_health_preserves_native_integer_domain() {
    unsafe {
        let mut b = vec![0u32; 0xc00 / 4];
        b[0] = DRONE;
        for x in [0, 1, u32::MAX] {
            b[0xb44 / 4] = x;
            b[0x5dc / 4] = x;
            b[0x5e0 / 4] = x;
            assert_eq!(after_initialize(b.as_mut_ptr() as usize), None);
        }
        b[0xb44 / 4] = 101;
        b[0x5dc / 4] = 101;
        b[0x5e0 / 4] = 101;
        assert_eq!(after_initialize(b.as_mut_ptr() as usize), Some((101, 50)));
    }
}
#[test]
#[ignore = "checked retail AA5150 bytes, C55 fixture"]
fn c55_native_init_copies_health_before_scoped_adapter() {
    unsafe {
        use sha2::{Digest, Sha256};
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn VirtualAlloc(a: *mut c_void, n: usize, t: u32, p: u32) -> *mut c_void;
            fn VirtualProtect(a: *mut c_void, n: usize, p: u32, old: *mut u32) -> i32;
            fn VirtualFree(a: *mut c_void, n: usize, t: u32) -> i32;
            fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
        }
        let raw = std::fs::read(
            std::path::PathBuf::from(std::env::var("SCBL_R58_C55_FIXTURE").unwrap())
                .join("drone-initialize.bin"),
        )
        .unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(&raw)), SHA);
        let at = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4) as usize;
        assert_ne!(at, 0);
        let mut code = raw.clone();
        code.resize(128, 0xcc);
        code[64] = 0xc3;
        code[80..83].copy_from_slice(&[0xc2, 4, 0]);
        for (off, dest) in [(3, at + 64), (30, at + 80)] {
            assert_eq!(code[off], 0xe8);
            code[off + 1..off + 5].copy_from_slice(
                &(dest as u32)
                    .wrapping_sub((at + off + 5) as u32)
                    .to_le_bytes(),
            );
        }
        std::ptr::copy_nonoverlapping(code.as_ptr(), at as *mut u8, code.len());
        let mut old = 0;
        assert_ne!(VirtualProtect(at as _, 4096, 0x20, &mut old), 0);
        assert_ne!(FlushInstructionCache(-1isize as _, at as _, code.len()), 0);
        let native: unsafe extern "thiscall" fn(usize) = std::mem::transmute(at);
        let mut b = vec![0u32; 0xc00 / 4];
        b[0] = DRONE;
        b[0xb44 / 4] = 100;
        native(b.as_mut_ptr() as usize);
        assert_eq!((b[0x5dc / 4], b[0x5e0 / 4]), (100, 100));
        assert_eq!(after_initialize(b.as_mut_ptr() as usize), Some((100, 50)));
        // Original entry on respawn reads unmodified B44, not the reduced maximum.
        native(b.as_mut_ptr() as usize);
        assert_eq!((b[0x5dc / 4], b[0x5e0 / 4]), (100, 100));
        assert_eq!(after_initialize(b.as_mut_ptr() as usize), Some((100, 50)));
        println!("C55_HEALTH nativeAA5150 current=100 max=100; modified current=50 max=50; reinit=50 asset=100 damage/EMP/lifecycle=unchanged");
        assert_ne!(VirtualFree(at as _, 0, 0x8000), 0);
    }
}
