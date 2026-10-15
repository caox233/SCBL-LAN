//! One process-lifetime owner of the accepted C71/C72 mouse fixes.
//! The optional gameplay DLL must not install these hooks. Ordinary menu,
//! background windows, controllers and unsupported builds keep native input.
mod memory;
mod merc_mouse_sensitivity;
mod raw_mouse;
mod raw_mouse_native;

#[used]
#[no_mangle]
pub static SCBL_CORE_MOUSE_PROTOCOL: [u8; 23] = *b"SCBL_CORE_MOUSE=C71C72\0";

pub(super) unsafe fn init(world: usize) {
    merc_mouse_sensitivity::init(world);
    raw_mouse_native::init(world);
}

pub(super) unsafe fn deinit() {
    raw_mouse_native::deinit();
    merc_mouse_sensitivity::deinit();
}
