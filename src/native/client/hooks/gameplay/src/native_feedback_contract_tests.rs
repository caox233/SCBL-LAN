//! C34 offline native ABI evidence. No hooks, sockets, sends, UI activation,
//! or production modules. Retail blocks execute with transport/UI stubbed.
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct FString {
    data: *const u16,
    count: i32,
    capacity: i32,
}
fn root() -> std::path::PathBuf {
    std::env::var("SCBL_R58_C34_FIXTURE")
        .expect("C34 native fixtures")
        .into()
}
fn retail(addr: u32) -> Vec<u8> {
    std::fs::read(root().join(format!("native-{addr:08x}.bin"))).unwrap()
}
struct Executable(usize);
impl Executable {
    unsafe fn allocate(code: &[u8], calls: &[(usize, usize)]) -> Self {
        assert!(code.len() < 4096);
        let m = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(m, 0);
        std::ptr::copy_nonoverlapping(code.as_ptr(), m as *mut u8, code.len());
        for &(offset, target) in calls {
            assert_eq!(code[offset], 0xe8);
            let relative = (target as u32).wrapping_sub((m + offset + 5) as u32);
            ((m + offset + 1) as *mut u32).write_unaligned(relative);
        }
        Self(m)
    }
}
impl Drop for Executable {
    fn drop(&mut self) {
        unsafe { VirtualFree(self.0 as *mut _, 0, MEM_RELEASE).unwrap() }
    }
}
fn words(bytes: usize) -> Vec<u32> {
    vec![0; (bytes + 3) / 4]
}
#[test]
fn retail_peer_contract_fixture_hashes_are_pinned() {
    let contracts = [
        (
            0x00ed0f80,
            285,
            "fd04a38294fc29aad21273afa9220da5a952dca2ce182db7862d6045ad56814d",
        ),
        (
            0x010b5110,
            319,
            "581ef3578c1fd05aa3f229bf89ff329f8bde8bce090fe9325daae99bda0183bc",
        ),
        (
            0x0104ed60,
            284,
            "2c39cb5ca133aa09c22e7c7a369d4059c280a4332db26069066daec7cd9d5c22",
        ),
        (
            0x0104ec70,
            94,
            "aa89b65033d164ab9766062fd3641a4f0ab60e9798eb6b19f531f81107cc7542",
        ),
        (
            0x007a5d60,
            18,
            "59aae6efed144d185fae1044cd601a548c3a19d4373e06470b473374917d130c",
        ),
        (
            0x01a715c5,
            16,
            "330e13b66e6f4218f762c91fd9e8a2de2caa92d53398b2e4b5a1f37cf944eff2",
        ),
    ];
    for (a, len, hash) in contracts {
        let b = retail(a);
        assert_eq!(b.len(), len);
        assert_eq!(format!("{:x}", Sha256::digest(&b)), hash);
    }
    assert_eq!(std::mem::size_of::<FString>(), 12);
    println!("C34_CONTRACT 6 retail fixtures pinned; x86 FString=12; no network event");
}
static SENT: AtomicU32 = AtomicU32::new(0);
unsafe extern "thiscall" fn sender_stub(
    manager: usize,
    text: *const FString,
    team_only: i32,
) -> i32 {
    assert_eq!(manager, 0x123456);
    assert_eq!(team_only, 1);
    let s = &*text;
    assert_eq!((s.count, s.capacity), (4, 4));
    assert_eq!(std::slice::from_raw_parts(s.data, 4), &[65, 66, 67, 0]);
    SENT.fetch_add(1, Ordering::SeqCst);
    0x11223344
}
#[test]
fn retail_send_callsite_passes_thiscall_and_utf16_without_sending() {
    let mut code = vec![
        0x55, 0x8b, 0xec, 0x83, 0xec, 0x2c, 0x8b, 0x45, 0x08, 0x89, 0x45, 0xf0, // manager
        0x8b, 0x55, 0x0c, 0x8b, 0x02, 0x89, 0x45, 0xd8, 0x8b, 0x42, 0x04, 0x89, 0x45, 0xdc, 0x8b,
        0x42, 0x08, 0x89, 0x45, 0xe0, 0x8b, 0x45, 0x10, 0x89, 0x45, 0xd4, // teamOnly
    ];
    let offset = code.len();
    code.extend(retail(0x1a715c5));
    code.extend([0xc9, 0xc3]);
    let s = FString {
        data: [65u16, 66, 67, 0].as_ptr(),
        count: 4,
        capacity: 4,
    };
    let before = SENT.load(Ordering::SeqCst);
    unsafe {
        let m = Executable::allocate(&code, &[(offset + 11, sender_stub as usize)]);
        let f: unsafe extern "C" fn(usize, *const FString, i32) -> i32 = std::mem::transmute(m.0);
        for _ in 0..1000 {
            assert_eq!(f(0x123456, &s, 1), 0x11223344);
        }
    }
    assert_eq!(SENT.load(Ordering::SeqCst) - before, 1000);
    println!("C34_SEND_ABI retail_callsite=1000 iterations; transport_stub=true; packets_sent=0");
}
#[test]
fn retail_match_manager_getter_follows_world_and_writes_nothing() {
    let mut context = words(0x30);
    let mut world = words(0x6d4);
    let mut info = words(0x37c);
    let holder = [world.as_ptr() as u32];
    context[0x28 / 4] = holder.as_ptr() as u32;
    world[0x6d0 / 4] = info.as_ptr() as u32;
    info[0x378 / 4] = 0x123450;
    let before = (context.clone(), world.clone(), info.clone());
    unsafe {
        let m = Executable::allocate(&retail(0x7a5d60), &[]);
        let f: unsafe extern "thiscall" fn(usize) -> usize = std::mem::transmute(m.0);
        assert_eq!(f(context.as_ptr() as usize), 0x123450);
    }
    assert_eq!((context, world, info), before);
    println!("C34_MANAGER context+28 -> world -> world+6D0 -> info+378; read_only=true");
}
#[test]
fn retail_length_guard_is_144_utf16_units_not_a_trusted_input_validator() {
    let whole = retail(0x10b5110);
    let mut body = whole[0x40..0x51].to_vec(); // 10B5150..10B5161
    assert_eq!(body, retail(0x10b5150));
    body.extend([0xeb, 0x14]); // skip accepted stub after native xor al,al
    body.resize(0x25, 0xcc);
    body.extend([0xb0, 1]); // native accepted target 10B5175
    let mut code = vec![0x55, 0x8b, 0xec, 0x53, 0x8b, 0x5d, 0x08];
    code.extend(body);
    code.extend([0x5b, 0xc9, 0xc3]);
    unsafe {
        let m = Executable::allocate(&code, &[]);
        let f: unsafe extern "C" fn(*const FString) -> u8 = std::mem::transmute(m.0);
        for (count, accepted) in [(0, 1), (1, 1), (145, 1), (146, 0), (999, 0), (-1, 1)] {
            let s = FString {
                data: std::ptr::null(),
                count,
                capacity: count,
            };
            assert_eq!(f(&s), accepted);
        }
    }
    println!(
        "C34_LENGTH native=144 UTF16 units; negative_count_not_validated; no construction/network"
    );
}
#[test]
fn native_text_getter_writes_terminator_even_when_capacity_is_sufficient() {
    // Only execute the existing non-null/no-growth path; never call the
    // allocator, receiver, or unproven polymorphic fallback in a fixture.
    let code = retail(0x4195e0);
    assert_eq!(&code[0x30..0x32], &[0x5e, 0xc3]);
    let mut buffer = words(0x0c + 10);
    buffer[0] = 1;
    buffer[1] = 4;
    buffer[2] = 5;
    let mut inner = words(16);
    inner[8 / 4] = buffer.as_ptr() as u32;
    unsafe {
        let text = (buffer.as_mut_ptr() as usize + 0x0c) as *mut u16;
        for (i, v) in [65, 66, 67, 68, 0xffff].into_iter().enumerate() {
            text.add(i).write(v)
        }
        let m = Executable::allocate(&code[..0x32], &[]);
        let f: unsafe extern "thiscall" fn(usize) -> usize = std::mem::transmute(m.0);
        assert_eq!(f(inner.as_ptr() as usize), text as usize);
        assert_eq!(std::slice::from_raw_parts(text, 5), &[65, 66, 67, 68, 0]);
        assert_eq!(&buffer[..3], &[1, 4, 5]);
    }
    println!("C34_TEXT native_getter_mutates_terminator=true; not a read-only collector accessor");
}
static UI_CALLS: AtomicU32 = AtomicU32::new(0);
static UI_FLAGS: AtomicU32 = AtomicU32::new(0);
unsafe extern "thiscall" fn ui_stub(
    manager: usize,
    name: *const FString,
    text: *const FString,
    team_only: i32,
    same_team: i32,
) {
    assert_eq!(manager, 0x123456);
    assert_eq!((*name).count, 2);
    assert_eq!((*text).count, 4);
    assert_eq!(std::slice::from_raw_parts((*name).data, 2), &[78, 0]);
    assert_eq!(
        std::slice::from_raw_parts((*text).data, 4),
        &[65, 66, 67, 0]
    );
    UI_FLAGS.store((team_only as u32) << 1 | same_team as u32, Ordering::SeqCst);
    UI_CALLS.fetch_add(1, Ordering::SeqCst);
}
#[test]
fn retail_receive_team_filter_is_presentation_not_authenticated_sender_proof() {
    let whole = retail(0x104ed60);
    let block = &whole[0x7d..0xa5]; // 104EDDD..104EE05
    assert_eq!(block, retail(0x104eddd));
    let mut code = vec![
        0x55, 0x8b, 0xec, 0x83, 0xec, 0x30, 0x53, 0x57, 0x8b, 0x45, 0x08, 0x89, 0x45,
        0xf0, // manager
        0x8b, 0x55, 0x0c, 0x8b, 0x02, 0x89, 0x45, 0xd8, 0x8b, 0x42, 0x04, 0x89, 0x45, 0xdc, 0x8b,
        0x42, 0x08, 0x89, 0x45, 0xe0, // name
        0x8b, 0x55, 0x10, 0x8b, 0x02, 0x89, 0x45, 0xe4, 0x8b, 0x42, 0x04, 0x89, 0x45, 0xe8, 0x8b,
        0x42, 0x08, 0x89, 0x45, 0xec, // text
        0x83, 0x7d, 0x14, 0, 0x0f, 0x94, 0xc3, // bl = !teamOnly
        0x8b, 0x7d, 0x18, 0x8b, 0x45, 0x1c, // sender team, local team
    ];
    let offset = code.len();
    code.extend(block);
    code.extend([0x5f, 0x5b, 0xc9, 0xc3]);
    let name_data = [78u16, 0];
    let text_data = [65u16, 66, 67, 0];
    let name = FString {
        data: name_data.as_ptr(),
        count: 2,
        capacity: 2,
    };
    let text = FString {
        data: text_data.as_ptr(),
        count: 4,
        capacity: 4,
    };
    unsafe {
        let m = Executable::allocate(&code, &[(offset + 35, ui_stub as usize)]);
        let f: unsafe extern "C" fn(usize, *const FString, *const FString, i32, u32, u32) =
            std::mem::transmute(m.0);
        for (team_only, sender_team, local_team, shown, flags) in [
            (0, 1, 2, true, 0),
            (0, 1, 1, true, 1),
            (1, 1, 1, true, 3),
            (1, 1, 2, false, 0),
            (1, 255, 0, false, 0),
            (1, 255, 1, false, 0),
            (1, 255, 2, false, 0),
            (1, 255, 3, false, 0),
        ] {
            let before = UI_CALLS.load(Ordering::SeqCst);
            f(0x123456, &name, &text, team_only, sender_team, local_team);
            assert_eq!(UI_CALLS.load(Ordering::SeqCst) - before, u32::from(shown));
            if shown {
                assert_eq!(UI_FLAGS.load(Ordering::SeqCst), flags)
            }
        }
    }
    println!(
        "C34_RECEIVE native_team_filter=4 cases; UI_stub=true; sender_authentication=not_proven"
    );
}
