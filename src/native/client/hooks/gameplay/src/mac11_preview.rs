//! Retail DX11 AEManikin only: pistol-slot MAC11 uses its existing one-hand
//! anchor, just like F40. B36560 otherwise selects the SMG paperdoll anchor.
//! No shared archetype, inventory, mesh, transform or save mutation. CFD480
//! still owns the entire attachment operation and is called exactly once.
use super::RETAIL_NAME;
use crate::equipment_native::{readable, word};
use retour::static_detour;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const MANIKIN_VT: u32 = 0x02ac59cc;
const WEAPON_VT: u32 = 0x02a85b7c;
const ARCHETYPE_VT: u32 = 0x02a176c4;
const BONE_QUERY: usize = 0x006b6860;
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
type AttachFn = unsafe extern "thiscall" fn(usize, usize, u32, f32) -> u32;
type BoneFn = unsafe extern "thiscall" fn(usize, *const u32) -> i32;
static_detour! {
    static Attach: unsafe extern "thiscall" fn(usize, usize, u32, f32) -> u32;
}

unsafe fn byte(p: usize, offset: usize) -> Option<u8> {
    let address = p.checked_add(offset)?;
    readable(address, 1).then(|| *(address as *const u8))
}
unsafe fn name_is(id: u32, expected: &[u8]) -> bool {
    super::native_name_is(id, expected)
}

unsafe fn replacement(
    parent: usize,
    child: usize,
    incoming: u32,
    mut named: impl FnMut(u32, &[u8]) -> bool,
    bone_index: impl FnOnce(usize, u32) -> i32,
) -> Option<u32> {
    if word(parent, 0) != Some(MANIKIN_VT)
        || word(child, 0) != Some(WEAPON_VT)
        || word(parent, 0x978) != Some(child as u32)
        || byte(parent, 0x8f4) != Some(1) // Spy, never campaign or Merc
        || byte(parent, 0x8f6) != Some(0) // Pistol, never primary preview
        || byte(parent, 0x5c).is_none_or(|v| v & 1 != 0)
        || byte(child, 0x5c).is_none_or(|v| v & 1 != 0)
    {
        return None;
    }
    let props = word(child, 0x744)? as usize;
    if word(props, 0) != Some(ARCHETYPE_VT)
        || !matches!(byte(props, 0x3b), Some(1 | 2))
        || !named(word(child, 0x740)?, RETAIL_NAME)
        || !named(incoming, b"Gun_Paperdoll")
    {
        return None;
    }
    let default = word(child, 0x584)?;
    if default == incoming || !named(default, b"Gun_OneHanded") {
        return None;
    }
    // Original CFD480 calls this same native read-only bone resolver. A
    // missing/default-incompatible mesh keeps native behavior, never guesses.
    (bone_index(child, default) >= 0).then_some(default)
}
unsafe fn native_bone_index(child: usize, name: u32) -> i32 {
    let vt = word(child, 0).unwrap_or(0) as usize;
    if word(vt, 0x280) != Some(BONE_QUERY as u32) {
        return -1;
    }
    let get: BoneFn = std::mem::transmute(BONE_QUERY);
    get(child, &name)
}
unsafe fn attach_with(
    parent: usize,
    child: usize,
    bone: u32,
    blend: f32,
    selected: Option<u32>,
    original: impl FnOnce(usize, usize, u32, f32) -> u32,
) -> u32 {
    original(parent, child, selected.unwrap_or(bone), blend)
}
fn attach(parent: usize, child: usize, bone: u32, blend: f32) -> u32 {
    unsafe {
        let selected = if READY.load(Ordering::Acquire) {
            replacement(
                parent,
                child,
                bone,
                |n, s| name_is(n, s),
                |c, n| native_bone_index(c, n),
            )
        } else {
            None
        };
        let result = attach_with(parent, child, bone, blend, selected, |p, c, n, t| {
            Attach.call(p, c, n, t)
        });
        if selected.is_some() && LOGS.fetch_add(1, Ordering::Relaxed) < 16 {
            tracing::info!("SCBL_MOD_MAC11_PREVIEW revision=46 scope=spy-pistol-manikin from=Gun_Paperdoll to=Gun_OneHanded native_result={}", result);
        }
        result
    }
}
pub(super) unsafe fn init() {
    for &(address, len, hash) in SIGNATURES {
        if !readable(address, len)
            || format!(
                "{:x}",
                Sha256::digest(std::slice::from_raw_parts(address as *const u8, len))
            ) != hash
        {
            tracing::warn!("SCBL_MOD_MAC11_PREVIEW enabled=false reason=native-signature");
            return;
        }
    }
    if Attach
        .initialize(std::mem::transmute::<usize, AttachFn>(0xcfd480), attach)
        .is_err()
        || Attach.enable().is_err()
    {
        deinit();
        tracing::warn!("SCBL_MOD_MAC11_PREVIEW enabled=false reason=detour");
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_MAC11_PREVIEW revision=46 enabled=true native_attachment=true combat_unchanged=true");
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    if Attach.is_enabled() {
        let _ = Attach.disable();
    }
    // No borrowed native object survives a call; no live object restoration.
}
const SIGNATURES: &[(usize, usize, &str)] = &[
    (
        0xcfd480,
        256,
        "a9c88f7874bfeeffa412c0cfd434bc79f4bb417eaac15f50547797a9a35e0cf1",
    ),
    (
        0x6b6860,
        208,
        "9eb256b04c1d9d19d18c293f995762935130952b6626c360fe02c460f71d936e",
    ),
    (
        0xb36560,
        450,
        "c021f46da993552a8e1b3ae9f0c6a1b2ad8004492a89278b572f616ad47fedb3",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    struct Scene {
        parent: Vec<u32>,
        child: Vec<u32>,
        props: Vec<u32>,
    }
    impl Scene {
        fn new() -> Self {
            let props = vec![0; 0x100 / 4];
            let child = vec![0; 0x800 / 4];
            let parent = vec![0; 0xa00 / 4];
            let mut s = Self {
                parent,
                child,
                props,
            };
            s.parent[0] = MANIKIN_VT;
            s.parent[0x978 / 4] = s.child.as_ptr() as u32;
            s.parent[0x8f4 / 4] = 1;
            s.child[0] = WEAPON_VT;
            s.child[0x744 / 4] = s.props.as_ptr() as u32;
            s.child[0x740 / 4] = 10;
            s.child[0x584 / 4] = 12;
            s.props[0] = ARCHETYPE_VT;
            s.props[0x38 / 4] = 1 << 24;
            s
        }
        unsafe fn select(&self, bone: u32, index: i32) -> Option<u32> {
            replacement(
                self.parent.as_ptr() as usize,
                self.child.as_ptr() as usize,
                bone,
                |id, expected| match id {
                    10 => expected == RETAIL_NAME,
                    11 => expected == b"Gun_Paperdoll",
                    12 => expected == b"Gun_OneHanded",
                    _ => false,
                },
                |c, b| {
                    assert_eq!(c, self.child.as_ptr() as usize);
                    assert_eq!(b, 12);
                    index
                },
            )
        }
    }
    #[test]
    fn mac11_preview_changes_only_anchor_and_preserves_all_native_fields() {
        let s = Scene::new();
        let before = (s.parent.clone(), s.child.clone(), s.props.clone());
        for _ in 0..3 {
            assert_eq!(unsafe { s.select(11, 2) }, Some(12));
        }
        assert_eq!((s.parent, s.child, s.props), before);
    }
    #[test]
    fn all_non_preview_wrong_role_slot_child_and_destroyed_paths_pass_through() {
        for (offset, value) in [
            (0, 0),
            (0x978, 0),
            (0x8f4, 0),
            (0x8f4, 2),
            (0x8f4, 1 | (1 << 16)),
            (0x5c, 1),
        ] {
            let mut s = Scene::new();
            s.parent[offset / 4] = value;
            assert_eq!(unsafe { s.select(11, 2) }, None);
        }
        for (offset, value) in [(0, 0), (0x5c, 1), (0x740, 20), (0x584, 20), (0x744, 0)] {
            let mut s = Scene::new();
            s.child[offset / 4] = value;
            assert_eq!(unsafe { s.select(11, 2) }, None);
        }
    }
    #[test]
    fn missing_bone_wrong_incoming_and_unknown_archetype_are_not_overridden() {
        let mut s = Scene::new();
        assert_eq!(unsafe { s.select(11, -1) }, None);
        assert_eq!(unsafe { s.select(12, 2) }, None);
        assert_eq!(unsafe { s.select(20, 2) }, None);
        s.props[0] = 0;
        assert_eq!(unsafe { s.select(11, 2) }, None);
        s.props[0] = ARCHETYPE_VT;
        s.props[0x38 / 4] = 3 << 24;
        assert_eq!(unsafe { s.select(11, 2) }, None);
        s.props[0x38 / 4] = 2 << 24;
        assert_eq!(unsafe { s.select(11, 2) }, Some(12));
    }
    #[test]
    fn no_native_query_for_unrelated_actor_or_invalid_pointer() {
        for (p, c) in [(0, 0), (1, 1)] {
            assert_eq!(
                unsafe { replacement(p, c, 11, |_, _| panic!(), |_, _| panic!()) },
                None
            );
        }
        let mut s = Scene::new();
        s.child[0x740 / 4] = 20;
        assert_eq!(
            unsafe {
                replacement(
                    s.parent.as_ptr() as usize,
                    s.child.as_ptr() as usize,
                    11,
                    |_, _| false,
                    |_, _| panic!(),
                )
            },
            None
        );
    }
    #[test]
    fn original_once_result_and_blend_bits_preserved_with_or_without_change() {
        for selected in [None, Some(12)] {
            for result in [0, 1, 0xdeadbeef] {
                for bits in [0, 0x80000000, 0x3f000000, 0x7fc01234] {
                    let mut calls = 0;
                    let got = unsafe {
                        attach_with(7, 9, 11, f32::from_bits(bits), selected, |p, c, b, t| {
                            calls += 1;
                            assert_eq!(
                                (p, c, b, t.to_bits()),
                                (7, 9, selected.unwrap_or(11), bits)
                            );
                            result
                        })
                    };
                    assert_eq!(got, result);
                    assert_eq!(calls, 1);
                }
            }
        }
    }
    #[test]
    #[ignore = "explicit MSVC x86 native preview attachment ret12 ABI fixture"]
    fn native_preview_attachment_preserves_thiscall_and_stack() {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryW(p: *const u16) -> *mut c_void;
            fn GetProcAddress(m: *mut c_void, n: *const u8) -> *mut c_void;
        }
        fn fixture(p: usize, c: usize, b: u32, t: f32) -> u32 {
            unsafe {
                attach_with(
                    p,
                    c,
                    b,
                    t,
                    if b == 11 { Some(12) } else { None },
                    |p, c, b, t| Attach.call(p, c, b, t),
                )
            }
        }
        let path = std::env::var("SCBL_MAC11_PREVIEW_ABI_FIXTURE").unwrap();
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let dll = LoadLibraryW(path.as_ptr());
            assert!(!dll.is_null());
            let get = GetProcAddress(dll, b"_get_attach@0\0".as_ptr());
            let call = GetProcAddress(dll, b"_invoke_attach@24\0".as_ptr());
            assert!(!get.is_null() && !call.is_null());
            let get: unsafe extern "system" fn() -> usize = std::mem::transmute(get);
            let call: unsafe extern "system" fn(usize, usize, usize, u32, u32, *mut u32) -> u32 =
                std::mem::transmute(call);
            let entry = get();
            let mut state = [0u32; 5];
            let mut output = 0;
            for stage in 0..3 {
                if stage == 1 {
                    Attach
                        .initialize(std::mem::transmute::<usize, AttachFn>(entry), fixture)
                        .unwrap();
                    Attach.enable().unwrap();
                }
                if stage == 2 {
                    Attach.disable().unwrap();
                }
                for i in 0..1000 {
                    let bone = if i % 2 == 0 { 11 } else { 20 };
                    let bits = if i % 3 == 0 { 0x80000000 } else { 0x3f000000 };
                    state[0] = if i % 2 == 0 { 1 } else { 0 };
                    assert_eq!(
                        call(
                            entry,
                            state.as_mut_ptr() as usize,
                            0x234567,
                            bone,
                            bits,
                            &mut output
                        ),
                        1
                    );
                    assert_eq!(output, state[0]);
                    assert_eq!(
                        &state[1..4],
                        &[
                            0x234567,
                            if stage == 1 && bone == 11 { 12 } else { bone },
                            bits
                        ]
                    );
                    assert_eq!(state[4], stage * 1000 + i + 1);
                }
            }
        }
        println!("MSVC x86 attachment ret12 BASELINE bone11; MODIFIED bone12; ROLLBACK bone11; 3000 calls preserve ECX/child/blend/result/ESP and original-once. ABI model, not game visual proof.");
    }
}
