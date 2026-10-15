//! C36 offline native origin proof. Executable retail leaf blocks use synthetic
//! memory; no live hooks, packets, player identity, or engine UI are touched.
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use windows::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
};
fn root() -> std::path::PathBuf {
    std::env::var("SCBL_R58_C36_FIXTURE")
        .expect("C36 fixtures")
        .into()
}
fn retail(a: u32) -> Vec<u8> {
    std::fs::read(root().join(format!("native-{a:08x}.bin"))).unwrap()
}
struct Code(usize);
impl Code {
    unsafe fn new(bytes: &[u8], calls: &[(usize, usize)]) -> Self {
        let p = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(p, 0);
        assert!(bytes.len() < 4096);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
        for &(offset, target) in calls {
            assert_eq!(bytes[offset], 0xe8);
            ((p + offset + 1) as *mut u32)
                .write_unaligned((target as u32).wrapping_sub((p + offset + 5) as u32));
        }
        Self(p)
    }
}
impl Drop for Code {
    fn drop(&mut self) {
        unsafe { VirtualFree(self.0 as *mut _, 0, MEM_RELEASE).unwrap() }
    }
}
fn words(n: usize) -> Vec<u32> {
    vec![0; (n + 3) / 4]
}
fn patch_global(code: &mut [u8], address: u32, pointer: usize) {
    let needle = address.to_le_bytes();
    let indices: Vec<_> = code
        .windows(4)
        .enumerate()
        .filter(|(_, b)| *b == needle)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(indices.len(), 1);
    code[indices[0]..indices[0] + 4].copy_from_slice(&(pointer as u32).to_le_bytes());
}
#[test]
fn c36_retail_origin_fixtures_are_pinned() {
    let blocks: &[(u32, usize, &str)] = &[
        (
            0x01fd90e0,
            68,
            "b7322a72e785e7cf8e36dc2cd5b5a4cc4ee4e28fd0194513c355fa5022960dee",
        ),
        (
            0x01fdcdf0,
            193,
            "b9279d0a09a3745d8fb3cbeddb9ee27999c854e831c89b601691d486422965fe",
        ),
        (
            0x01fdcd10,
            213,
            "dbedb30674a317482fdc51341dd9f70c4fd2c88fcb8d7d8d1bba76fd3b6bb29d",
        ),
        (
            0x0079f5c0,
            4,
            "e27121b8816e0aef2ed29676899d26e79f66af7e5ee5aed902be7208122bb635",
        ),
        (
            0x01fdf550,
            56,
            "5f33afe435599e7920137c4fc7c567bb47dcccf727cb961ac14e913b4738ad18",
        ),
        (
            0x007f3099,
            20,
            "9453bbde9bd259988ca0a7fb5e9a9ad708a431dc94a6c98c63f35db4edb1e13a",
        ),
        (
            0x0083b32b,
            40,
            "2da89d240f07f810aa0ded07bb7f7c24a1a060199749c127a30bda7ae989a41c",
        ),
        (
            0x0078d190,
            4,
            "38058506e8e859d5be73a33baa621352c08ea01f2224ee170173253757193941",
        ),
    ];
    for &(a, length, hash) in blocks {
        let bytes = retail(a);
        assert_eq!(bytes.len(), length);
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), hash);
    }
    println!(
        "C36_ORIGIN pinned retail blocks; native envelope and message are separate; no engine send"
    );
}
unsafe fn packet(from: u32, target: u32, time: u32, msg: usize) -> Vec<u32> {
    let code = Code::new(&retail(0x1fd90e0), &[]);
    let f: unsafe extern "thiscall" fn(usize, u32, u32, usize, u32, u32, u32, u32) =
        std::mem::transmute(code.0);
    let mut p = words(0x40);
    f(p.as_mut_ptr() as usize, 1, 0x33, msg, from, target, time, 0);
    p
}
#[test]
fn c36_native_packet_source_does_not_come_from_message_body() {
    let mut m = words(0x68);
    m[1] = 0xdeadbeef;
    let a = unsafe { packet(17, 9, 1234, m.as_ptr() as usize) };
    m[1] = 0x12345678;
    let b = unsafe { packet(17, 9, 1234, m.as_ptr() as usize) };
    assert_eq!((a[2], a[3], a[4], a[7]), (m.as_ptr() as u32, 9, 17, 1234));
    assert_eq!(a, b);
    println!("C36_PACKET payload From mutated twice; envelope From stays17 destination9 time1234");
}
unsafe fn context(from: u32, time: u32, msg: usize) -> Vec<u32> {
    let globals = [0x11223344u32, 0x55667788];
    let mut base = retail(0x1fdcd10);
    patch_global(&mut base, 0x34c0084, globals.as_ptr() as usize);
    patch_global(&mut base, 0x34c0088, globals.as_ptr().add(1) as usize);
    let base = Code::new(&base, &[]);
    let setter = Code::new(&retail(0x1fdcdf0), &[(0x1b, base.0)]);
    let f: unsafe extern "thiscall" fn(usize, u32, u32, u32, u32, u32, *const u32, usize, u32) =
        std::mem::transmute(setter.0);
    let mut c = words(0x38);
    c[0x14 / 4] = 1; // native borrowed context branch, no reflection allocator
    let descriptor = [0x33445566u32, 0x778899aa];
    f(
        c.as_mut_ptr() as usize,
        1,
        2,
        3,
        4,
        from,
        descriptor.as_ptr(),
        msg,
        time,
    );
    assert_eq!((c[1], c[6], c[7], c[8]), (1, 2, 3, 4));
    assert_eq!((c[10], c[11]), (descriptor[0], descriptor[1]));
    c
}
#[test]
fn c36_native_context_getter_reports_transport_argument_not_payload_from() {
    let m = [0u32, 0xffffffff, 0, 0];
    let c = unsafe { context(72, 808, m.as_ptr() as usize) };
    let getter = unsafe { Code::new(&retail(0x79f5c0), &[]) };
    let get: unsafe extern "thiscall" fn(usize) -> u32 = unsafe { std::mem::transmute(getter.0) };
    assert_eq!(unsafe { get(c.as_ptr() as usize) }, 72);
    assert_eq!((c[0x30 / 4], c[0x34 / 4]), (m.as_ptr() as u32, 808));
    assert_eq!(m[1], 0xffffffff);
    println!("C36_CONTEXT From72 Time808 stored outside borrowed payload; getter reads context+24");
}
static RECEIVE_ARGS: Mutex<[u32; 8]> = Mutex::new([0; 8]);
unsafe extern "thiscall" fn capture_context(
    this: usize,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
    from: u32,
    desc: usize,
    msg: usize,
    time: u32,
) {
    let borrowed = ((this + 0x14) as *const u8).read();
    *RECEIVE_ARGS.lock().unwrap() = [a, b, c, d, from, desc as u32, msg as u32, time];
    // Store the native borrowed flag in a separate sentinel slot; no native writes.
    assert_eq!(borrowed, 1);
}
#[test]
fn c36_native_receive_callsite_uses_packet_source_and_time() {
    let mut bytes = vec![
        0x55, 0x8b, 0xec, 0x83, 0xec, 0x3c, 0x56, 0x57, 0x8b, 0xf1, 0x8b, 0x7d, 0x0c,
    ];
    let prefix = bytes.len();
    bytes.extend(retail(0x1fdf550));
    bytes.extend([0x5f, 0x5e, 0xc9, 0xc2, 0x14, 0]);
    let mut agent = words(0x30);
    let mut object = words(0x28);
    let mut manager = words(0x94);
    let descriptor = words(0x20);
    let message = words(0x68);
    object[0x18 / 4] = 3;
    object[0x1c / 4] = 4;
    agent[0x28 / 4] = object.as_ptr() as u32;
    manager[0x90 / 4] = 0x10203;
    unsafe {
        let code = Code::new(&bytes, &[(prefix + 0x33, capture_context as usize)]);
        let f: unsafe extern "thiscall" fn(usize, usize, usize, usize, usize, u32) =
            std::mem::transmute(code.0);
        for (from, time) in [(0, 30), (17, 400), (u32::MAX, 900)] {
            let p = packet(from, 9, time, message.as_ptr() as usize);
            f(
                agent.as_mut_ptr() as usize,
                manager.as_ptr() as usize,
                descriptor.as_ptr() as usize,
                p.as_ptr() as usize,
                message.as_ptr() as usize,
                2,
            );
            assert_eq!(
                *RECEIVE_ARGS.lock().unwrap(),
                [
                    0x10203,
                    2,
                    3,
                    4,
                    from,
                    descriptor.as_ptr() as u32 + 8,
                    message.as_ptr() as u32,
                    time
                ]
            );
        }
    }
    println!("C36_RX native callsite uses envelope+10/+1c; does not normalize unknown peer; validation still required");
}
unsafe fn queue_copy(m: &mut [u32], from: u32, time: u32, flags: u32) {
    let mut bytes = vec![0x55, 0x8b, 0xec, 0x56, 0x8b, 0xf1];
    bytes.extend(retail(0x7f3099));
    bytes.extend([0x5e, 0xc9, 0xc2, 0x10, 0]);
    let code = Code::new(&bytes, &[]);
    let f: unsafe extern "thiscall" fn(usize, usize, *const u32, u32, u32) =
        std::mem::transmute(code.0);
    f(m.as_mut_ptr() as usize, 0, &time, from, flags);
}
#[test]
fn c36_native_queue_replaces_all_untrusted_origin_time_flags() {
    let mut m = words(0x68);
    m[1] = 0xdeadbeef;
    m[2] = u32::MAX;
    m[3] = u32::MAX;
    m[21] = 2;
    let before = m.clone();
    unsafe {
        queue_copy(&mut m, 17, 500, 0);
    }
    assert_eq!(&m[1..4], &[17, 500, 0]);
    assert_eq!(&m[4..], &before[4..]);
    assert_eq!(m[0], before[0]);
    println!("C36_QUEUE cloned message+4/+8/+c overwritten; team/text/body unchanged");
}
unsafe fn lookup(net: &[u32]) -> u32 {
    let mut bytes = vec![0x55, 0x8b, 0xec, 0x56, 0x53, 0x8b, 0xf1, 0x8b, 0x4e, 0x50];
    bytes.extend(retail(0x83b32b));
    bytes.extend([0x8b, 0xc1, 0x5b, 0x5e, 0xc9, 0xc3]);
    let code = Code::new(&bytes, &[]);
    let f: unsafe extern "thiscall" fn(usize) -> u32 = std::mem::transmute(code.0);
    f(net.as_ptr() as usize)
}
#[test]
fn c36_native_peer_lookup_is_scoped_to_netactors_session_list() {
    let mut session = words(0x600);
    let mut net = words(0x90);
    let mut node = words(12);
    let peer = [17u32, 0, 0, 0];
    let other = [72u32, 0, 0, 0];
    let end = session.as_ptr() as u32 + 0x584;
    session[0x588 / 4] = node.as_ptr() as u32;
    node[1] = end;
    node[2] = peer.as_ptr() as u32;
    net[0x50 / 4] = session.as_ptr() as u32;
    net[0x18 / 4] = 17;
    assert_eq!(unsafe { lookup(&net) }, peer.as_ptr() as u32);
    // Equal human names or message IDs cannot substitute for native peer ownership.
    net[0x18 / 4] = 72;
    assert_eq!(unsafe { lookup(&net) }, 0);
    node[2] = other.as_ptr() as u32;
    assert_eq!(unsafe { lookup(&net) }, other.as_ptr() as u32);
    net[0x18 / 4] = u32::MAX;
    assert_eq!(unsafe { lookup(&net) }, 0);
    net[0x18 / 4] = 72;
    session[0x588 / 4] = end;
    assert_eq!(unsafe { lookup(&net) }, 0);
    println!("C36_PEER native actor+18 maps only within actor+50 peer list584; unknown/departed returnsnull");
}
#[test]
fn c36_full_envelope_to_peer_chain_keeps_source_separate_from_object_identity() {
    let mut message = words(0x68);
    message[1] = 999;
    let p = unsafe { packet(17, 9, 500, message.as_ptr() as usize) };
    let ctx = unsafe { context(p[4], p[7], p[2] as usize) };
    unsafe {
        queue_copy(&mut message, ctx[9], ctx[13], 0);
    }
    let mut session = words(0x600);
    let mut net = words(0x90);
    let mut node = words(12);
    let peer = [17u32, 0, 0, 0];
    let sentinel = session.as_ptr() as u32 + 0x584;
    session[0x588 / 4] = node.as_ptr() as u32;
    node[1] = sentinel;
    node[2] = peer.as_ptr() as u32;
    net[0x50 / 4] = session.as_ptr() as u32;
    net[0x18 / 4] = message[1];
    // +70/+74 is a separate 64-bit NetActor object identity, NOT From or account PID.
    net[0x70 / 4] = 0x12345678;
    net[0x74 / 4] = 0x9abcdef0;
    assert_eq!(unsafe { lookup(&net) }, peer.as_ptr() as u32);
    assert_eq!(message[1], 17);
    assert_eq!(&net[0x70 / 4..0x78 / 4], &[0x12345678, 0x9abcdef0]);
    println!("C36_CHAIN envelope17 -> context17 -> queued17 -> current native Peer17; object64 remains separate; packets=0");
}
