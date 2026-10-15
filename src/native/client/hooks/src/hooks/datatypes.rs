use std::ffi::c_void;
use std::ffi::CStr;

use super::utils;

#[repr(C)]
pub struct NetFiniteStateVMT {
    unknown1: *const c_void,
    unknown2: *const c_void,
    change_state: *const c_void,
    unknown3: *const c_void,
    get_result: *const c_void,
    unknown4: *const c_void,
    unknown5: *const c_void,
    initialize: *const c_void,
    on_enter: *const c_void,
    leave_state: *const c_void,
    unknown6: *const c_void,
    unknown7: *const c_void,
    get_state_id: unsafe extern "thiscall" fn(*const NetFiniteState) -> u32,
    get_state_name: unsafe extern "thiscall" fn(*const NetFiniteState) -> *const i8,
    unknown8: *const c_void,
    unknown9: *const c_void,
}

#[repr(C)]
pub struct NetFiniteState {
    pub vtable: *const NetFiniteStateVMT,
}

impl NetFiniteState {
    // Native state/vtable must stay live for the callback. Its result may be
    // null or a valid NUL-terminated native string, not a Rust 'static borrow.
    pub fn get_state_name(&self) -> String {
        unsafe {
            let name = ((*self.vtable).get_state_name)(std::ptr::from_ref(self));
            if name.is_null() { return "<NULL>".to_owned(); }
            CStr::from_ptr(name).to_string_lossy().into_owned()
        }
    }
    pub fn get_state_id(&self) -> u32 {
        unsafe { ((*self.vtable).get_state_id)(std::ptr::from_ref(self)) }
    }
}

#[repr(C)]
pub struct NetFiniteStateMachineVMT {
    unknown1: *const c_void,
    unknown2: *const c_void,
    unknown3: *const c_void,
    unknown4: *const c_void,
    get_statemachine_id: extern "thiscall" fn(*const NetFiniteStateMachine, *mut NetFiniteStateID) -> *mut NetFiniteStateID,
}

#[repr(C)]
pub struct NetFiniteStateMachine {
    pub vtable: *const NetFiniteStateMachineVMT,
    unknown1: u32,
    unknown2: u32,
    unknown3: u32,
    pub current_state: *mut NetFiniteState,
    pub last_state: *mut NetFiniteState,
}

impl NetFiniteStateMachine {
    pub fn get_statemachine_id(&self, id: *mut NetFiniteStateID) -> *mut NetFiniteStateID {
        unsafe { ((*self.vtable).get_statemachine_id)(self, id) }
    }

    pub fn get_statemachine_name(&self) -> String {
        let mut id = NetFiniteStateID { id: 0, unknown: 0 };
        self.get_statemachine_id(&mut id);
        utils::id_to_name(id.id as usize)
    }
}

#[repr(C)]
pub struct NetFiniteStateID {
    pub id: u32,
    pub unknown: u32,
}

impl NetFiniteStateID {
    pub fn name(&self) -> String {
        utils::id_to_name(self.id as usize)
    }
}

#[repr(C)]
pub struct GearBasicStringInternal {
    ref_counter: u32,
    pub size: u32,
    capacity: u32,
}

impl GearBasicStringInternal {
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            let p = std::ptr::from_ref(self).cast::<u8>().offset(12);
            if p.is_null() {
                b"<NULL>"
            } else {
                std::slice::from_raw_parts(p, self.size as usize)
            }
        }
    }

    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.as_bytes())
    }
}

#[repr(C)]
pub struct GearBasicString {
    allocator: *mut c_void,
    unknown: u32,
    pub internal: *mut GearBasicStringInternal,
    tag: *mut i8,
}

impl GearBasicString {
    #[allow(dead_code)]
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        unsafe {
            if let Some(int) = self.internal.as_ref() {
                int.as_str()
            } else {
                "<NULL>".into()
            }
        }
    }
}

#[repr(C)]
pub struct MaybeGoal {
    pub unknown: usize,
    name: *const i8,
}

impl MaybeGoal {
    pub fn name(&self) -> Option<&CStr> {
        if self.name.is_null() {
            return None;
        }
        Some(unsafe { CStr::from_ptr(self.name) })
    }
}

#[repr(C)]
pub struct QuazalStep {
    pub callback: *const c_void,
    unknown: u32,
    pub description: *const i8,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize,Ordering};
    static NAME:AtomicUsize=AtomicUsize::new(0);
    unsafe extern "thiscall" fn name(_:*const NetFiniteState)->*const i8 {NAME.load(Ordering::Relaxed) as *const i8}
    unsafe extern "thiscall" fn id(_:*const NetFiniteState)->u32 {17}
    fn state()->(Vec<usize>,NetFiniteState) {
        let mut table=vec![0usize;16];table[12]=id as *const () as usize;table[13]=name as *const () as usize;
        let state=NetFiniteState{vtable:table.as_ptr().cast()};(table,state)
    }
    #[test] fn state_names_own_native_text_and_keep_ids() {
        let text=b"Ready\0";NAME.store(text.as_ptr() as usize,Ordering::Relaxed);
        let (_table,state)=state();let output:String=state.get_state_name();
        assert_eq!(output,"Ready");assert_ne!(output.as_ptr(),text.as_ptr());assert_eq!(state.get_state_id(),17);
    }
    #[test] fn invalid_utf8_is_lossy_and_null_has_placeholder() {
        let text=[b'A',255,b'B',0];NAME.store(text.as_ptr() as usize,Ordering::Relaxed);
        let (_table,state)=state();assert_eq!(state.get_state_name(),"A\u{fffd}B");
        NAME.store(0,Ordering::Relaxed);assert_eq!(state.get_state_name(),"<NULL>");
    }
    #[test] fn retained_native_string_layout_and_goal_borrow() {
        assert_eq!(std::mem::size_of::<GearBasicStringInternal>(),12);
        assert_eq!(std::mem::size_of::<GearBasicString>(),16);
        let data=[1u32,3,3,u32::from_le_bytes([b'a',255,b'b',0])];
        let internal=unsafe{&*data.as_ptr().cast::<GearBasicStringInternal>()};
        assert_eq!(internal.as_bytes(),[b'a',255,b'b']);assert_eq!(internal.as_str(),"a\u{fffd}b");
        let goal=MaybeGoal{unknown:0,name:c"OwnedGoal".as_ptr()};assert_eq!(goal.name().unwrap().to_bytes(),b"OwnedGoal");
        assert!(MaybeGoal{unknown:0,name:std::ptr::null()}.name().is_none());
    }
}
