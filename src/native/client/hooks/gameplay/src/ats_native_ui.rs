//! Owned native BATS_Icon handles. This uses the same factory, child names,
//! animations, projection, text and destructors as retail 1A86AA0/1A6F990.
//! It never inserts an entry into the game's ATS array and never needs an actor.
use super::{Frame, Mark, Surface};
use crate::equipment_native::{readable, word, writable};
type Str = [u32; 3];
#[derive(Clone, Copy)]
pub(super) struct Bindings {
    pub factory: unsafe extern "C" fn(usize, *const u8, *const u8, usize, u32) -> usize,
    pub init_str: unsafe extern "thiscall" fn(*mut Str, *const u8) -> usize,
    pub free_str: unsafe extern "thiscall" fn(*mut Str),
    pub child: unsafe extern "thiscall" fn(usize, *const Str) -> usize,
    pub text_child: unsafe extern "thiscall" fn(usize, *const Str) -> usize,
    pub project: unsafe extern "thiscall" fn(usize, *mut [f32; 3], *const [f32; 3], usize) -> usize,
    pub position: unsafe extern "thiscall" fn(usize, *const [f32; 3]),
    pub in_view: unsafe extern "C" fn(*const [f32; 3]) -> u8,
    pub text: unsafe extern "thiscall" fn(usize, *const Str) -> u8,
    pub sound: unsafe extern "C" fn(*const Str),
}
pub(super) struct Native {
    pub bindings: Bindings,
    pub root_global: usize,
    pub counter: usize,
}
#[derive(Default, Debug)]
pub(super) struct Handles {
    pub root: usize,
    pub anim: usize,
    pub distance: usize,
    pub weapon: usize,
    pub positioned: bool,
}
unsafe fn method(p: usize, offset: usize) -> Option<usize> {
    let vt = word(p, 0)? as usize;
    let f = word(vt, offset)? as usize;
    readable(f, 1).then_some(f)
}
unsafe fn visible(p: usize, yes: bool) {
    if let Some(f) = method(p, 0x108) {
        let call: unsafe extern "thiscall" fn(usize, u8) = std::mem::transmute(f);
        call(p, yes as u8);
    }
}
unsafe fn animate(p: usize, name: &'static [u8]) {
    if let Some(f) = method(p, 0x12c) {
        let call: unsafe extern "thiscall" fn(usize, *const u8) = std::mem::transmute(f);
        call(p, name.as_ptr());
    }
}
unsafe fn delete(p: usize) {
    if p != 0 {
        if let Some(f) = method(p, 0) {
            let call: unsafe extern "thiscall" fn(usize, u32) -> usize = std::mem::transmute(f);
            let _ = call(p, 1);
        }
    }
}
impl Native {
    pub(super) unsafe fn retail() -> Self {
        Self {
            root_global: 0x31d0240,
            counter: 0x344e23c,
            bindings: Bindings {
                factory: std::mem::transmute(0x18ca9a0usize),
                init_str: std::mem::transmute(0x42e2a0usize),
                free_str: std::mem::transmute(0x42e370usize),
                child: std::mem::transmute(0x18cf510usize),
                text_child: std::mem::transmute(0x193ca10usize),
                project: std::mem::transmute(0x1a69120usize),
                position: std::mem::transmute(0x18a2270usize),
                in_view: std::mem::transmute(0x6643b0usize),
                text: std::mem::transmute(0x18bf360usize),
                sound: std::mem::transmute(0x18653a0usize),
            },
        }
    }
    unsafe fn string<R>(&self, name: &[u8], f: impl FnOnce(*const Str) -> R) -> R {
        debug_assert_eq!(name.last(), Some(&0));
        let mut s = [0; 3];
        (self.bindings.init_str)(&mut s, name.as_ptr());
        let result = f(&s);
        (self.bindings.free_str)(&mut s);
        result
    }
    unsafe fn get_child(&self, root: usize, name: &[u8], text: bool) -> usize {
        self.string(name, |s| {
            if text {
                (self.bindings.text_child)(root, s)
            } else {
                (self.bindings.child)(root, s)
            }
        })
    }
}
impl Surface for Native {
    type Handle = Handles;
    fn create(&mut self, hud: usize) -> Option<Handles> {
        unsafe {
            let root = word(self.root_global, 0)? as usize;
            if !readable(root, 4) || !writable(self.counter, 4) {
                return None;
            }
            // Native factory IDs are shared by ordinary BATS icons; follow its exact
            // increment, on the same HUD thread, to prevent duplicate native instance names.
            let id = word(self.counter, 0)?.wrapping_add(1);
            (self.counter as *mut u32).write_unaligned(id);
            let name = format!("BATS_Icon_obj_{}\0", id as i32);
            let mut h = Handles {
                root: (self.bindings.factory)(
                    root,
                    b"BATS_Icon\0".as_ptr(),
                    name.as_ptr(),
                    hud,
                    id,
                ),
                ..Default::default()
            };
            if h.root == 0 {
                return None;
            }
            h.anim = self.get_child(h.root, b"IconAnim\0", false);
            h.distance = self.get_child(h.root, b"BATS_Distance\0", true);
            h.weapon = self.get_child(h.root, b"BATS_Weapon\0", true);
            // Unlike retail's unconditional dereference, a missing asset/child fails
            // closed and releases only successfully acquired native owned handles.
            if h.anim == 0
                || h.distance == 0
                || h.weapon == 0
                || method(h.root, 0x108).is_none()
                || method(h.root, 0x12c).is_none()
                || method(h.anim, 0x12c).is_none()
            {
                self.destroy(h);
                return None;
            }
            visible(h.weapon, false);
            visible(h.distance, false);
            visible(h.root, false);
            Some(h)
        }
    }
    fn restart(&mut self, h: &mut Handles) {
        unsafe {
            animate(h.root, b"high\0");
            animate(h.anim, b"IconStart\0");
        }
    }
    fn update(&mut self, hud: usize, h: &mut Handles, f: &Frame, m: &Mark) {
        unsafe {
            h.positioned = false;
            // Native offset is +25 game units, not a guessed head position or live actor.
            let at = [m.position[0], m.position[1], m.position[2] + 25.];
            if !f.visible
                || !at.iter().all(|x| x.is_finite())
                || (self.bindings.in_view)(&m.position) == 0
            {
                visible(h.root, false);
                return;
            }
            let mut screen = [f32::NAN; 3];
            (self.bindings.project)(hud, &mut screen, &at, 0);
            if !screen.iter().all(|x| x.is_finite()) {
                visible(h.root, false);
                return;
            }
            (self.bindings.position)(h.root, &screen);
            let metres = (m
                .position
                .iter()
                .zip(f.position)
                .map(|(a, b)| (*a as f64 - b as f64).powi(2))
                .sum::<f64>()
                .sqrt()
                / 100.)
                .trunc();
            let show_distance = metres.is_finite()
                && metres >= 0.
                && metres <= i32::MAX as f64
                && f.now.saturating_sub(m.shot_time_ms) > f.distance_delay_ms;
            if show_distance {
                let text = format!("{}m\0", metres as i32);
                self.string(text.as_bytes(), |s| {
                    (self.bindings.text)(h.distance, s);
                });
            }
            visible(h.distance, show_distance);
            visible(h.weapon, false);
            visible(h.root, true);
            h.positioned = true; // Submitted native position/visibility; not pixel confirmation.
        }
    }
    fn fade(&mut self, h: &mut Handles) {
        unsafe {
            animate(h.root, b"fade_low\0");
            animate(h.anim, b"IconEnd\0");
        }
    }
    fn destroy(&mut self, h: Handles) {
        // Retail deletes IconAnim and root only. Distance/weapon wrappers are owned
        // by the root; deleting them separately would double-free native children.
        unsafe {
            delete(h.anim);
            delete(h.root);
        }
    }
    fn notify(&mut self) {
        unsafe {
            self.string(b"DareStudio.Play_SFX_ADV_VMode_Spy_BATS_Highlight\0", |s| {
                (self.bindings.sound)(s)
            });
        }
    }
}
