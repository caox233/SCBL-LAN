use super::*;

fn fixture() -> SockAddrInRaw {
    make_bind_addr(Ipv4Addr::new(192, 0, 2, 200), [0xab, 0xcd])
}

fn pointer(value: &SockAddrInRaw) -> *const SOCKADDR {
    std::ptr::from_ref(value).cast()
}

#[test]
fn complete_ipv4_retains_high_bytes_and_network_port() {
    let value = fixture();
    assert_eq!(std::mem::size_of::<SockAddrInRaw>(), 16);
    assert_eq!(sockaddr_ipv4(pointer(&value), 16), Some(Ipv4Addr::new(192, 0, 2, 200)));
    assert_eq!(sockaddr_port(pointer(&value), 16), Some(0xabcd));
    assert_eq!(sockaddr_port_bytes(pointer(&value), 16), [0xab, 0xcd]);
}

#[test]
fn negative_and_truncated_addresses_are_not_rewritten() {
    let value = fixture();
    for len in [i32::MIN, -1, 0, 1, 3, 4, 7, 8, 15] {
        assert!(sockaddr_ipv4(pointer(&value), len).is_none());
        assert!(sockaddr_port(pointer(&value), len).is_none());
        assert_eq!(sockaddr_port_bytes(pointer(&value), len), [0, 0]);
    }
}

#[test]
fn null_address_is_not_dereferenced() {
    assert!(sockaddr_ipv4(std::ptr::null(), 16).is_none());
    assert!(sockaddr_port(std::ptr::null(), 16).is_none());
    assert_eq!(sockaddr_port_bytes(std::ptr::null(), 16), [0, 0]);
}

#[test]
fn other_families_do_not_enter_ipv4_rewrite() {
    let mut value = fixture();
    for family in [0, 23, u16::MAX] {
        value.sin_family = family;
        assert!(sockaddr_ipv4(pointer(&value), 16).is_none());
        assert!(sockaddr_port(pointer(&value), 16).is_none());
        assert_eq!(sockaddr_port_bytes(pointer(&value), 16), [0, 0]);
    }
}

#[test]
fn byte_aligned_buffers_are_read_without_mutation() {
    for offset in 0..4 {
        let mut bytes = [0xa5_u8; 32];
        let addr = unsafe { bytes.as_mut_ptr().add(offset).cast::<SockAddrInRaw>() };
        unsafe { addr.write_unaligned(fixture()) };
        let before = bytes;
        assert_eq!(sockaddr_ipv4(addr.cast(), 16), Some(Ipv4Addr::new(192, 0, 2, 200)));
        assert_eq!(sockaddr_port(addr.cast(), 16), Some(0xabcd));
        assert_eq!(sockaddr_port_bytes(addr.cast(), 16), [0xab, 0xcd]);
        assert_eq!(bytes, before);
    }
}

#[test]
fn larger_address_storage_keeps_the_ipv4_prefix() {
    let mut storage = [0_u32; 8];
    let addr = storage.as_mut_ptr().cast::<SockAddrInRaw>();
    unsafe { addr.write_unaligned(fixture()) };
    let before = storage;
    assert_eq!(sockaddr_ipv4(addr.cast(), 32), Some(Ipv4Addr::new(192, 0, 2, 200)));
    assert_eq!(sockaddr_port(addr.cast(), 32), Some(0xabcd));
    assert_eq!(storage, before);
}
