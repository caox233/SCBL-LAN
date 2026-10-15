use std::ffi::CStr;
use std::mem::size_of;
use std::net::Ipv4Addr;
use windows::Win32::Foundation::BOOL;
use windows::Win32::NetworkManagement::IpHelper::IP_ADDR_STRING;

// GetAdaptersInfo uses 32-bit time_t on Win32, including after 2038.
// windows 0.54 projects these two members as i64, producing size=648/align=8
// instead of the Windows x86 ABI's size=640/align=4. Do not use that projection.
// https://learn.microsoft.com/windows/win32/api/iptypes/ns-iptypes-ip_adapter_info
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(non_camel_case_types, non_snake_case)]
pub(super) struct IP_ADAPTER_INFO {
    pub Next: *mut Self,
    pub ComboIndex: u32,
    pub AdapterName: [i8; 260],
    pub Description: [i8; 132],
    pub AddressLength: u32,
    pub Address: [u8; 8],
    pub Index: u32,
    pub Type: u32,
    pub DhcpEnabled: u32,
    pub CurrentIpAddress: *mut IP_ADDR_STRING,
    pub IpAddressList: IP_ADDR_STRING,
    pub GatewayList: IP_ADDR_STRING,
    pub DhcpServer: IP_ADDR_STRING,
    pub HaveWins: BOOL,
    pub PrimaryWinsServer: IP_ADDR_STRING,
    pub SecondaryWinsServer: IP_ADDR_STRING,
    pub LeaseObtained: i32,
    pub LeaseExpires: i32,
}

#[cfg(not(target_pointer_width = "32"))]
compile_error!("SCBL adapter filter must be built for the game's Windows x86 ABI");
const _: () = {
    assert!(size_of::<IP_ADAPTER_INFO>() == 640);
    assert!(std::mem::align_of::<IP_ADAPTER_INFO>() == 4);
    assert!(std::mem::offset_of!(IP_ADAPTER_INFO, LeaseObtained) == 632);
    assert!(std::mem::offset_of!(IP_ADAPTER_INFO, LeaseExpires) == 636);
};

/// Keep the requested adapter at the beginning of the caller's Windows buffer.
/// Safety: `buffer` must cover `length` writable bytes returned successfully by
/// GetAdaptersInfo. No references into that buffer are kept across the write.
pub(super) unsafe fn filter_adapters(buffer: *mut IP_ADAPTER_INFO, length: usize, target: Ipv4Addr) -> Result<bool, &'static str> {
    let record_size = size_of::<IP_ADAPTER_INFO>();
    if buffer.is_null() || length < record_size {return Err("missing or short adapter buffer");}
    let base = buffer as usize;
    if base.checked_add(length).is_none() {return Err("adapter buffer address overflow");}
    let mut node = buffer;
    let target = target.to_string();
    for _ in 0..length / record_size {
        if node.is_null() {return Ok(false);}
        let Some(offset) = (node as usize).checked_sub(base) else {return Err("adapter pointer before buffer");};
        if offset > length - record_size {return Err("adapter pointer outside buffer");}
        // Snapshot the exact ABI record; callers may provide only 4-byte alignment.
        let mut selected = node.read_unaligned();
        let ip_bytes = selected.IpAddressList.IpAddress.String.map(|v| v as u8);
        let ip = CStr::from_bytes_until_nul(&ip_bytes).map_err(|_| "unterminated adapter address")?;
        if ip.to_bytes() == target.as_bytes() {
            selected.Next = std::ptr::null_mut();
            // Separate stack value: no overlapping immutable/mutable slices and
            // no i64 tail overwrite. Embedded list pointers remain in the buffer.
            buffer.write_unaligned(selected);
            return Ok(true);
        }
        node = selected.Next;
    }
    if node.is_null() {Ok(false)} else {Err("cyclic or overlapping adapter chain")}
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {bytes: Vec<u64>, offset: usize}
    impl Fixture {
        fn new(offset: usize) -> Self {
            let mut f=Self{bytes:vec![0u64;180],offset};
            unsafe {
                let head=f.head();let second=head.cast::<u8>().add(640).cast::<IP_ADAPTER_INFO>();
                let mut a:IP_ADAPTER_INFO=std::mem::zeroed();a.Next=second;
                for (d,s) in a.IpAddressList.IpAddress.String.iter_mut().zip(b"192.0.2.1\0") {*d=*s as i8;}
                head.write_unaligned(a);
                a.Next=std::ptr::null_mut();a.IpAddressList.IpAddress.String[8]=b'2' as i8;second.write_unaligned(a);
            }
            f
        }
        fn head(&mut self)->*mut IP_ADAPTER_INFO {unsafe{self.bytes.as_mut_ptr().cast::<u8>().add(self.offset).cast()}}
        fn target()->Ipv4Addr {Ipv4Addr::new(192,0,2,2)}
    }
    #[test] fn win32_abi_layout() {assert_eq!(size_of::<IP_ADAPTER_INFO>(),640);assert_eq!(std::mem::align_of::<IP_ADAPTER_INFO>(),4);}
    #[test] fn four_byte_alignment_second_adapter_and_canaries() {
        let mut f=Fixture::new(4);let before=f.bytes.clone();let p=f.head();
        assert_eq!(unsafe{filter_adapters(p,1280,Fixture::target())},Ok(true));
        assert!(unsafe{p.read_unaligned().Next}.is_null());
        let bytes=unsafe{std::slice::from_raw_parts(f.bytes.as_ptr().cast::<u8>(),f.bytes.len()*8)};
        let original=unsafe{std::slice::from_raw_parts(before.as_ptr().cast::<u8>(),before.len()*8)};
        assert_eq!(&bytes[..4],&original[..4]);assert_eq!(&bytes[644..],&original[644..]);
    }
    #[test] fn byte_alignment_does_not_create_typed_references() {let mut f=Fixture::new(1);assert_eq!(unsafe{filter_adapters(f.head(),1280,Fixture::target())},Ok(true));}
    #[test] fn first_adapter_is_not_self_copied() {let mut f=Fixture::new(4);assert_eq!(unsafe{filter_adapters(f.head(),1280,Ipv4Addr::new(192,0,2,1))},Ok(true));}
    #[test] fn missing_target_preserves_buffer() {let mut f=Fixture::new(4);let before=f.bytes.clone();assert_eq!(unsafe{filter_adapters(f.head(),1280,Ipv4Addr::LOCALHOST)},Ok(false));assert_eq!(before,f.bytes);}
    #[test] fn null_buffer_rejected() {assert!(unsafe{filter_adapters(std::ptr::null_mut(),1280,Fixture::target())}.is_err());}
    #[test] fn short_buffer_preserved() {let mut f=Fixture::new(4);let before=f.bytes.clone();assert!(unsafe{filter_adapters(f.head(),639,Fixture::target())}.is_err());assert_eq!(before,f.bytes);}
    #[test] fn out_of_range_next_preserved() {let mut f=Fixture::new(4);let p=f.head();unsafe{std::ptr::addr_of_mut!((*p).Next).write_unaligned(p.cast::<u8>().add(1279).cast());}let before=f.bytes.clone();assert!(unsafe{filter_adapters(p,1280,Fixture::target())}.is_err());assert_eq!(before,f.bytes);}
    #[test] fn cycle_is_bounded() {let mut f=Fixture::new(4);let p=f.head();unsafe{std::ptr::addr_of_mut!((*p).Next).write_unaligned(p);}let before=f.bytes.clone();assert!(unsafe{filter_adapters(p,1280,Fixture::target())}.is_err());assert_eq!(before,f.bytes);}
    #[test] fn unterminated_ip_rejected_without_write() {let mut f=Fixture::new(4);let p=f.head();unsafe{let mut a=p.read_unaligned();a.IpAddressList.IpAddress.String=[b'x' as i8;16];p.write_unaligned(a);}let before=f.bytes.clone();assert!(unsafe{filter_adapters(p,1280,Fixture::target())}.is_err());assert_eq!(before,f.bytes);}
}
