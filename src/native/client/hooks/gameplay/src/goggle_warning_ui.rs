//! Reversible warning presentation for validated peer detection receipts.
//! Never writes Pawn marks, HUD+308 native tagged cache, detector or timer fields.
use crate::equipment_native::{readable, word};
use crate::goggle_exposure::{Language, Presentation};

const HUD: u32 = 0x2c41234;
const WARNING_AMBER: u32 = 0xefa63c;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WarningKind {
    Exposed,
    Interference,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct Value {
    interface: usize,
    kind: u32,
    data: u64,
}
impl Value {
    fn object(self) -> bool {
        matches!(self.kind & 0x8f, 9 | 10) && self.interface != 0 && self.data as u32 != 0
    }
    fn color(self) -> Option<u32> {
        match self.kind & 0x8f {
            3 => i32::try_from(self.data as u32).ok().map(|v| v as u32),
            4 => Some(self.data as u32),
            5 => {
                let n = f64::from_bits(self.data);
                (n.is_finite() && n >= 0. && n <= u32::MAX as f64 && n.fract() == 0.)
                    .then_some(n as u32)
            }
            _ => None,
        }
    }
}
pub(crate) type Get = unsafe extern "thiscall" fn(usize, usize, *const u8, *mut Value, u8) -> u8;
pub(crate) type Release = unsafe extern "thiscall" fn(usize, *mut Value, usize);
pub(crate) type SetColor = unsafe extern "thiscall" fn(usize, *const u8, u32);
pub(crate) type SetMember =
    unsafe extern "thiscall" fn(usize, usize, *const u8, *const Value, u8) -> u8;

/// Exact retail ABI: 241C2D0 GetMember / 241AC60 managed release;
/// 1862FD0 widget SetTextColor (this, child-path, packed RGB).
pub(crate) struct ColorApi {
    pub get: Get,
    pub release: Release,
    pub set: SetColor,
}
struct Owned<'a> {
    value: Value,
    api: &'a ColorApi,
}
impl Drop for Owned<'_> {
    fn drop(&mut self) {
        if self.value.kind & 0x40 != 0 && self.value.interface != 0 {
            unsafe {
                let object = self.value.data as usize;
                (self.api.release)(self.value.interface, &mut self.value, object);
            }
        }
    }
}
impl ColorApi {
    unsafe fn member(&self, object: Value, name: &'static [u8]) -> Option<Owned<'_>> {
        if !object.object() {
            return None;
        }
        let mut v = Owned {
            value: Value::default(),
            api: self,
        };
        let ok = (self.get)(
            object.interface,
            object.data as usize,
            name.as_ptr(),
            &mut v.value,
            u8::from(object.kind & 0x8f == 10),
        );
        (ok != 0).then_some(v)
    }
    pub(crate) unsafe fn read(&self, widget: usize) -> Option<u32> {
        let display = word(widget, 0x1a8)? as usize;
        let value = word(display, 0x5c)? as usize;
        if !readable(value, std::mem::size_of::<Value>()) {
            return None;
        }
        let root = (value as *const Value).read_unaligned();
        // Read exactly the child used by native SetTextColor, not another
        // widget's tint or an assumed default red/white restoration color.
        let field = self.member(root, b"textField\0")?;
        let color = self.member(field.value, b"textColor\0")?;
        color.value.color()
    }
    pub(crate) unsafe fn write(&self, widget: usize, color: u32) {
        (self.set)(widget, b"textField\0".as_ptr(), color);
    }
}

/// Retail Cxform: RGBA multipliers then normalized RGBA offsets. 2351939
/// converts packed colors and multiplies these offsets by 255.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Transform(pub [f32; 8]);
impl Transform {
    pub(crate) fn amber() -> Self {
        Self([0., 0., 0., 1., 239. / 255., 166. / 255., 60. / 255., 0.])
    }
    fn finite(self) -> bool {
        self.0.iter().all(|v| v.is_finite())
    }
    pub(crate) fn with_rgb(mut self, rgb: Self) -> Self {
        for i in [0, 1, 2, 4, 5, 6] {
            self.0[i] = rgb.0[i];
        }
        self // Native animation owns alpha. Never restore a stale alpha value.
    }
}
pub(crate) type GetTransform = unsafe extern "thiscall" fn(usize, usize, *mut Transform) -> u8;
pub(crate) type SetTransform = unsafe extern "thiscall" fn(usize, usize, *const Transform) -> u8;
pub(crate) struct TransformApi {
    pub get: GetTransform,
    pub set: SetTransform,
}
unsafe fn widget_value(widget: usize) -> Option<Value> {
    let display = word(widget, 0x1a8)? as usize;
    let p = word(display, 0x5c)? as usize;
    if !readable(p, std::mem::size_of::<Value>()) {
        return None;
    }
    let v = (p as *const Value).read_unaligned();
    (v.object() && v.kind & 0x8f == 10).then_some(v)
}
impl TransformApi {
    pub(crate) unsafe fn read(&self, widget: usize) -> Option<Transform> {
        let v = widget_value(widget)?;
        let mut out = Transform([f32::NAN; 8]);
        ((self.get)(v.interface, v.data as usize, &mut out) != 0 && out.finite()).then_some(out)
    }
    pub(crate) unsafe fn rgb(&self, widget: usize, rgb: Transform) -> bool {
        let Some(current) = self.read(widget) else {
            return false;
        };
        let Some(v) = widget_value(widget) else {
            return false;
        };
        let out = current.with_rgb(rgb);
        out.finite() && (self.set)(v.interface, v.data as usize, &out) != 0
    }
}
impl ColorApi {
    /// 18BDA80 can skip a cached string after the Flash timeline has reset it.
    /// Reassert the same native textField.text using a borrowed UTF-16 value.
    pub(crate) unsafe fn force_text(&self, widget: usize, text: &[u16], set: SetMember) -> bool {
        if text.last() != Some(&0) || text.len() > 256 {
            return false;
        }
        let Some(root) = widget_value(widget) else {
            return false;
        };
        let Some(field) = self.member(root, b"textField\0") else {
            return false;
        };
        if !field.value.object() {
            return false;
        }
        let value = Value {
            interface: 0,
            kind: 7,
            data: text.as_ptr() as u64,
        };
        set(
            field.value.interface,
            field.value.data as usize,
            b"text\0".as_ptr(),
            &value,
            u8::from(field.value.kind & 0x8f == 10),
        ) != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub life: (usize, usize, u64),
    pub hud: usize,
    pub container: usize,
    pub texts: [usize; 2],
    display: [usize; 3],
    gfx: [usize; 3],
}
impl Binding {
    /// Call on the game HUD thread with the already validated local life.
    /// Fresh identity comparisons prevent restoration into a rebuilt widget.
    pub(crate) unsafe fn resolve(
        hud: usize,
        world: usize,
        pawn: usize,
        generation: u64,
    ) -> Option<Self> {
        if generation == 0
            || world == 0
            || pawn == 0
            || word(hud, 0)? != HUD
            || word(pawn, 0x144)? as usize != world
            || word(word(hud, 0x25c)? as usize, 0x310)? as usize != pawn
        {
            return None;
        }
        // Bind only the objects this adapter reads/writes. Retail can leave
        // the separate HUD+32c icon's GFx value null (observed on local Spy).
        // It is owned by retail and never modified here; its initialization
        // must not disable valid container/label widgets or reset their style.
        let widgets = [
            word(hud, 0x320)? as usize,
            word(hud, 0x324)? as usize,
            word(hud, 0x328)? as usize,
        ];
        if widgets.contains(&0) || (0..3).any(|i| widgets[i + 1..].contains(&widgets[i])) {
            return None;
        }
        let mut display = [0; 3];
        let mut gfx = [0; 3];
        for i in 0..3 {
            display[i] = word(widgets[i], 0x1a8)? as usize;
            gfx[i] = word(display[i], 0x5c)? as usize;
            if gfx[i] == 0 || !readable(gfx[i], 16) {
                return None;
            }
        }
        Some(Self {
            life: (world, pawn, generation),
            hud,
            container: widgets[0],
            texts: [widgets[1], widgets[2]],
            display,
            gfx,
        })
    }
}

/// Only UI operations. The runtime adapter must use native 18BF320 UTF-16
/// text and the original IntroTag/OutroTag virtual animation calls. It must
/// restore the current language's LocID_M_HUD_Detected label, not cached English.
pub(crate) trait Ui {
    fn colors(&mut self, binding: Binding) -> Option<[u32; 2]>;
    fn text(&mut self, widget: usize, utf16: &[u16]) -> bool;
    fn transform(&mut self, widget: usize) -> Option<Transform>;
    fn tint_rgb(&mut self, widget: usize, rgb: Transform) -> bool;
    fn sound(&mut self, binding: Binding);
    fn color(&mut self, widget: usize, rgb: u32);
    fn native_label(&mut self) -> Option<Vec<u16>>;
    fn animation(&mut self, container: usize, intro: bool);
}
struct Saved {
    binding: Binding,
    colors: [u32; 2],
    native_text: Vec<u16>,
    transform: Transform,
    active: bool,
}
#[derive(Default)]
pub(crate) struct WarningUi {
    saved: Option<Saved>,
}
impl WarningUi {
    fn exposed_style(b: Binding, language: Language, kind: WarningKind, ui: &mut impl Ui) -> bool {
        let text = match kind {
            WarningKind::Exposed => crate::goggle_exposure::text(language),
            WarningKind::Interference => crate::rfd_interference::text(language),
        };
        for w in b.texts {
            if !ui.text(w, &text) {
                return false;
            }
            ui.color(w, WARNING_AMBER);
        }
        ui.tint_rgb(b.container, Transform::amber())
    }
    pub(crate) fn apply(
        &mut self,
        binding: Option<Binding>,
        state: Presentation,
        language: Language,
        ui: &mut impl Ui,
    ) -> bool {
        self.apply_kind(binding, state, language, WarningKind::Exposed, ui)
    }
    pub(crate) fn apply_kind(
        &mut self,
        binding: Option<Binding>,
        state: Presentation,
        language: Language,
        kind: WarningKind,
        ui: &mut impl Ui,
    ) -> bool {
        let Some(binding) = binding else {
            // No native calls without a fresh binding. Retain only the pristine
            // value snapshot; it can be used again only if the full binding
            // resolves identically. A different life/widget still discards it.
            return false;
        };
        if self
            .saved
            .as_ref()
            .is_some_and(|old| old.binding != binding)
        {
            self.saved = None;
        }
        if state == Presentation::Exposed {
            if self.saved.is_none() {
                let Some(colors) = ui.colors(binding) else {
                    return false;
                };
                let Some(transform) = ui.transform(binding.container) else {
                    return false;
                };
                let Some(native_text) = ui.native_label() else {
                    return false;
                };
                if native_text.last() != Some(&0) || native_text.len() > 256 {
                    return false;
                }
                self.saved = Some(Saved {
                    binding,
                    colors,
                    native_text,
                    transform,
                    active: false,
                });
            }
            let first = !self.saved.as_ref().unwrap().active;
            // Prepare before IntroTag and reassert after its timeline reset.
            if !Self::exposed_style(binding, language, kind, ui) {
                return false;
            }
            if first {
                ui.animation(binding.container, true);
                if !Self::exposed_style(binding, language, kind, ui) {
                    ui.animation(binding.container, false);
                    return false;
                }
                self.saved.as_mut().unwrap().active = true;
                // Interference is an ongoing instrument status, not a new tag.
                // Do not replay the Spy's detected cue on a Mercenary pawn.
                if kind == WarningKind::Exposed {
                    ui.sound(binding);
                }
            }
            return true;
        }
        if state == Presentation::Hidden {
            if let Some(saved) = self.saved.as_mut() {
                if saved.active {
                    ui.animation(binding.container, false);
                    saved.active = false;
                }
                // Don't restore red DETECTED while OutroTag is still visible.
                // Retain the pristine snapshot for the next real native mark.
            }
            return false;
        }
        if let Some(saved) = self.saved.as_mut() {
            // Current locale normally wins; a transient lookup failure still
            // must not leave EXPOSED styling covering a genuine native tag.
            let label = ui
                .native_label()
                .filter(|v| v.last() == Some(&0) && v.len() <= 256)
                .unwrap_or_else(|| saved.native_text.clone());
            // The native warning owns presentation now, even if a GFx write
            // temporarily fails. Do not suppress its animation or replay sound.
            saved.active = false;
            let mut restored = true;
            for (i, w) in binding.texts.into_iter().enumerate() {
                restored &= ui.text(w, &label);
                ui.color(w, saved.colors[i]);
            }
            restored &= ui.tint_rgb(binding.container, saved.transform);
            // A failed write must not lose the only pristine snapshot or let
            // the next episode snapshot our own partially restored yellow UI.
            if restored {
                self.saved = None;
            }
        }
        false
    }
}

#[cfg(test)]
#[path = "goggle_warning_ui_tests.rs"]
mod tests;
