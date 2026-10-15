//! C54: preserve native EMF/thermal prop colour and contour draws.
//! Separate occlusion membership from appearance. Do not move highlighted props
//! into the ordinary background pipeline or change native opacity/materials.
//! C59: tune base RGB only; retain native Ambient/Shadow/Light colours.
//! Change only the native CPU RGB uniform, not shared materials or GPU state.
#[path = "vision_material_observer.rs"]
mod material_observer;
#[path = "vision_final_ps_probe.rs"]
mod final_ps_probe;
#[path = "vision_causal_probe.rs"]
mod causal_probe;
#[path = "vision_constant_reuse.rs"]
mod constant_reuse;
use crate::equipment_native::{readable, signature, word, writable};
use retour::{Function, GenericDetour, HookableWith};
use std::sync::OnceLock;
use std::cell::Cell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
static READY: AtomicBool = AtomicBool::new(false);
// Private A/B build only. Ordinary builds ignore the process environment.
// Latch once before enabling hooks; never poll the environment per draw.
static NATIVE_COMPARE: AtomicBool = AtomicBool::new(false);
#[inline]
fn comparison_build() -> bool {
    option_env!("SCBL_VISION_COMPARE_BUILD") == Some("1")
}
#[inline]
fn native_comparison() -> bool {
    comparison_build() && NATIVE_COMPARE.load(Ordering::Acquire)
}
static LOGS: AtomicU32 = AtomicU32::new(0);
static SKIPS: AtomicU32 = AtomicU32::new(0);
// High-frequency native entrypoints use typed direct callbacks rather than the
// StaticDetour FFI -> boxed Fn dispatch. Ownership is published before enabling;
// static slots retain trampolines after disable for callbacks already in flight.
type ClassifyFn = unsafe extern "thiscall" fn(usize);
type MaterialFn = unsafe extern "thiscall" fn(usize, usize, usize, usize) -> usize;
#[allow(non_upper_case_globals)]
static Classify: OnceLock<GenericDetour<ClassifyFn>> = OnceLock::new();
#[allow(non_upper_case_globals)]
static Material: OnceLock<GenericDetour<MaterialFn>> = OnceLock::new();
unsafe fn install_direct<T: Function + HookableWith<T>>(
    slot: &OnceLock<GenericDetour<T>>, target: T, callback: T,
) -> bool {
    if slot.get().is_some() { return false; }
    let Ok(hook) = GenericDetour::new(target, callback) else { return false; };
    if slot.set(hook).is_err() { return false; }
    slot.get().expect("published before enable").enable().is_ok()
}
static BRIGHTNESS_READY: AtomicBool = AtomicBool::new(false);
// Two records per native material stage, independently per view mode.
// Bounded diagnostics in the existing hook: no observer DLL or extra draw hook.
static BRIGHTNESS_LOGS: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];
fn brightness_log_bucket(mode: u32, stage: u32) -> Option<usize> {
    match (mode, stage) {
        (8, 2) => Some(0),
        (8, 4) => Some(1),
        (9, 7) => Some(2),
        (9, 8) => Some(3),
        _ => None,
    }
}
fn take_brightness_log(counter: &AtomicU32) -> bool {
    counter.load(Ordering::Relaxed) < 2 && counter.fetch_add(1, Ordering::Relaxed) < 2
}
const MAX_ITEMS: usize = 65536;
type Grow = unsafe extern "thiscall" fn(usize);

// The native Base Color Mul is baked into props+190 RGB (16E2D40).
// 16E2A10 copies those constants to the CPU shader bank+ C30 before drawing.
// C59: user-selected common base peak 0.3; secondary palette stays native.
// These are linear shader intensities, not percentages of displayed pixels.
#[cfg(test)]
const SCENE_HIGHLIGHT_PEAK: f32 = 0.3;
// Retained for comparison only; user disabled the C58 secondary-colour policy.
#[cfg(test)]
const SUBDUE_SECONDARY_COLOURS: bool = false;
#[cfg(test)]
const SECONDARY_COLOUR_PEAK: f32 = 0.6;
#[cfg(test)]
const SECONDARY_COLOUR_OFFSETS: [usize; 3] = [0xc50, 0xc70, 0xc80];
// Native Ambient/Shadow/Light Color. Retain their relative channel ratios and
// dark values; unlike the common base, never raise a dim secondary colour.
#[cfg(test)]
fn subdued_rgb(rgb: [f32; 3]) -> Option<[f32; 3]> {
    if rgb.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return None;
    }
    let peak = rgb[0].max(rgb[1]).max(rgb[2]);
    let scale = SECONDARY_COLOUR_PEAK / peak.max(1.0);
    Some(rgb.map(|v| v * scale))
}
#[cfg(test)]
fn unified_rgb(rgb: [f32; 3]) -> Option<[f32; 3]> {
    if rgb.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return None;
    }
    let peak = rgb[0].max(rgb[1]).max(rgb[2]);
    if peak <= 0.000001 {
        return None; // Never light up a deliberately unlit/black material.
    }
    Some(rgb.map(|v| (v / peak) * SCENE_HIGHLIGHT_PEAK))
}
// C62: the native PS multiplies BaseColor by max(1, lit RGB) AFTER
// lighting and before the distance-dependent thermal/EMF colour lookup result.
// Compensate that amplification instead of editing the palette or lookup axes.
// This is a linear pre-postprocess signal ceiling, NOT a measured screen/NPC
// luminance ratio. The requested <=50% visual contrast remains a runtime gate.
#[cfg(test)]
const PROP_SIGNAL_CEILING: f32 = 0.20;
#[cfg(test)]
fn contrast_rgb(base: [f32; 3], envelope: f32) -> Option<[f32; 3]> {
    contrast_rgb_for_mode(8, base, envelope)
}
fn contrast_rgb_for_mode(mode: u32, base: [f32; 3], envelope: f32) -> Option<[f32; 3]> {
    // C65: C64's worst-case amplification bound suppressed a live EMF
    // prop from 1.0 to 0.009638554 (bound 40.7). The bound is not its actual
    // per-pixel lighting: using it as a full divisor loses native dim details.
    // Use a gentler sqrt-gain shoulder with unit low-signal slope for EMF.
    // No hard floor / brightening above native input. Thermal stays C63/C64.
    // The EMF knee is NOT a final-signal ceiling or a measured pixel ratio.
    let ceiling = match mode {
        9 => 0.40f32,
        8 => 0.20f32,
        _ => return None,
    };
    if !envelope.is_finite() || envelope < 1.0 || base.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return None;
    }
    let peak = base[0].max(base[1]).max(base[2]);
    if peak <= 0.000001 {
        return None;
    }
    // Smooth shoulder: never raise a dim signal and never flatten it to one
    // common value. f64 avoids overflow for otherwise finite native uniforms.
    let scale = if mode == 9 {
        (1.0 / (1.0 + peak as f64 * (envelope as f64).sqrt() / ceiling as f64)) as f32
    } else {
        // Keep the previous thermal arithmetic, including rounding, unchanged.
        (0.5 / (1.0 + (peak as f64 * envelope as f64) / (2.0 * ceiling as f64))) as f32
    };
    Some(base.map(|v| v * scale))
}
// C66: native stage 4 reuses the colour queue with +18C materials after
// the stage-3 mask pass. Stage 2/+18C==0 is a separate material family.
// A live thermal stage-4 draw is recorded; its mesh identity is NOT proven.
// Reduce only this second family's additive heat contribution. Keep the
// first family, EMF and native contour/alpha/distance inputs unchanged.
// 0.35 is a candidate RGB gain, not an on-screen opacity or NPC luminance ratio.
const THERMAL_SECONDARY_GAIN: f32 = 0.35;
fn contrast_rgb_for_draw(
    mode: u32,
    stage: u32,
    secondary_material: bool,
    base: [f32; 3],
    envelope: f32,
) -> Option<[f32; 3]> {
    let rgb = contrast_rgb_for_mode(mode, base, envelope)?;
    if mode == 8 && stage == 4 && secondary_material {
        Some(rgb.map(|v| v * THERMAL_SECONDARY_GAIN))
    } else {
        Some(rgb)
    }
}
fn lighting_envelope(
    palette: [[f32; 3]; 3],
    diffuse_scale: [f32; 3],
    diffuse_bias: [f32; 3],
    view_colour: [f32; 3],
    ao: [f32; 4],
) -> Option<f32> {
    // Registered Ambient/Shadow/Light RGB remain untouched. Native lookup,
    // albedo and AO samples are normalized; include native HDR colour inputs.
    if palette
        .iter()
        .flatten()
        .chain(diffuse_scale.iter())
        .chain(diffuse_bias.iter())
        .chain(view_colour.iter())
        .chain(ao.iter())
        .any(|v| !v.is_finite() || *v < 0.0)
    {
        return None;
    }
    let ao_max = (ao[0] as f64 + ao[1] as f64).max(1.0);
    let mut bound = 1.0f64;
    for i in 0..3 {
        let light = palette.iter().map(|v| v[i] as f64).sum::<f64>();
        let diffuse = diffuse_scale[i] as f64 * view_colour[i] as f64 + diffuse_bias[i] as f64;
        bound = bound.max(light * diffuse * ao_max);
    }
    if bound > f32::MAX as f64 {
        None
    } else {
        Some(bound as f32)
    }
}
unsafe fn material_envelope(bank: usize) -> Option<f32> {
    // Native material upload: selected+190 (90 bytes) -> bank+C30.
    // Reflection: BaseColor at CB+30, DiffuseScale at+A0, Bias at+B0.
    // Therefore scale/bias live at bank+CA0/CB0, not CD0/CE0.
    // C63: the native name/register table maps cViewModeDiffCol to AE0,
    // NOT C10. C10 holds signed distance parameters (174E706/1759CC9).
    // AE0 is uploaded LATER in this draw (16E5668); reading it here would
    // use the preceding object's colour. Its RGB is either native white or
    // an 8-bit HSV-to-RGB result (16E4645/1625C90), both bounded by 1.
    // Use that proven upper bound: no stale bank read, extra hook, native
    // lookup call, or palette write. This can darken a tinted prop further,
    // but cannot underestimate amplification because of previous draw order.
    let rgb = |offset| std::array::from_fn(|i| f32::from_bits(raw(bank, offset + i * 4)));
    lighting_envelope(
        [rgb(0xc50), rgb(0xc70), rgb(0xc80)],
        rgb(0xca0),
        rgb(0xcb0),
        [1.0; 3],
        std::array::from_fn(|i| f32::from_bits(raw(bank, 0xc60 + i * 4))),
    )
}
#[derive(Clone, Copy, Default)]
struct BrightnessScope {
    pass: usize,
    table: usize,
    shader: usize,
    render: usize,
    view: usize,
    root: usize,
    bank: usize,
    mode: u32,
}
thread_local! {
    static BRIGHTNESS_SCOPE: Cell<BrightnessScope> = const { Cell::new(BrightnessScope {
        pass: 0, table: 0, shader: 0, render: 0, view: 0, root: 0, bank: 0, mode: 0,
    }) };
}
unsafe fn brightness_scope(pass: usize, bank: usize) -> Option<BrightnessScope> {
    let render = word(pass, 0x14)? as usize;
    if word(render, 0x236c)? != pass as u32 {
        return None;
    }
    let view = word(render, 0x20d0)? as usize;
    let mode = word(view, 0x340)?;
    let vt = match mode {
        9 => 0x2c0965c,
        8 => 0x2c094a4,
        _ => return None,
    };
    let table = word(pass, 8)? as usize;
    let selected = word(table, mode as usize * 4)? as usize;
    let root = word(pass, 0)? as usize;
    if word(selected, 0)? != vt
        || word(selected, 4)? != root as u32
        || word(root, 0)? != 0x2c06404
        || !writable(bank.checked_add(0xc30)?, 0x90)
    {
        return None;
    }
    Some(BrightnessScope {
        pass,
        table,
        shader: selected,
        render,
        view,
        root,
        bank,
        mode,
    })
}
unsafe fn apply_brightness(
    scope: BrightnessScope,
    root: usize,
    render: usize,
    object: usize,
    selected: usize,
    bank: usize,
) -> Option<([f32; 3], [f32; 3])> {
    // These pointers are live native arguments AFTER a successful original
    // 16E2A10 call, which already reads the object/selected material. Frame
    // identity and bank bounds are validated once by the classifier, not via
    // VirtualQuery for every mesh/material. No allocation or GPU state calls.
    if scope.bank == 0
        || root != scope.root
        || render != scope.render
        || object == 0
        || selected == 0
        || bank != scope.bank
    {
        return None;
    }
    // The native pass/table can change while render/view/bank addresses stay
    // the same. Reject the old scope before reading the old pass or shader.
    if raw(render, 0x236c) as usize != scope.pass
        || raw(scope.pass, 0x14) as usize != render
        || raw(scope.pass, 0) as usize != scope.root
        || raw(scope.pass, 8) as usize != scope.table
        || raw(scope.table, scope.mode as usize * 4) as usize != scope.shader
        || raw(render, 0x20d0) as usize != scope.view
        || raw(scope.view, 0x340) != scope.mode
        || raw(selected, 0) != 0x2c06404
        || !scene_highlight(
            scope.mode,
            raw(object, 0x34),
            raw(object, 0x38),
            (raw(object, 0x210) >> 24) as u8,
        )
    {
        return None;
    }
    let before = std::array::from_fn(|i| f32::from_bits(raw(bank, 0xc30 + i * 4)));
    let after = contrast_rgb_for_draw(
        scope.mode,
        raw(render, 0x233c),
        (raw(selected, 0x18c) & 0xff) != 0,
        before,
        material_envelope(bank)?,
    )?;
    for (i, value) in after.iter().enumerate() {
        put(bank, 0xc30 + i * 4, value.to_bits());
    }
    // Write exactly BaseColor RGB. Keep ColorRange (native distance curve),
    // palette, AO, albedo controls, fourth components and native alpha untouched.
    Some((before, after))
}
// View-local checked-owner set. Never reused
// across an outer native view; fixed capacity fails closed instead of allocating.
#[derive(Clone, Copy)]
struct MercView {
    active: bool, owners: [usize; 16], count: usize,
}
impl MercView { const fn empty() -> Self { Self { active:false, owners:[0;16], count:0 } } }
thread_local! { static MERC_VIEW: Cell<MercView> = const { Cell::new(MercView::empty()) }; }
struct MercViewScope(MercView);
impl Drop for MercViewScope { fn drop(&mut self) { MERC_VIEW.with(|v|v.set(MercView {owners:[0;16],count:0,..self.0})); } }
unsafe fn with_merc_view(native: impl FnOnce()) {
    let previous=MERC_VIEW.with(|v|v.replace(MercView {active:true,..MercView::empty()}));
    let _scope=MercViewScope(previous); native();
}
unsafe fn merc_owner_readable(owner: usize) -> bool {
    MERC_VIEW.with(|v| {
        let mut s=v.get(); if !s.active {return false;}
        if owner >= 0x10000 && s.owners[..s.count].contains(&owner) {return true;}
        if s.count==s.owners.len() {return false;}
        // Failure also occupies one slot, bounding adversarial unique inputs.
        s.owners[s.count]=0; s.count+=1; v.set(s);
        if !readable(owner,0x310) {return false;}
        s.owners[s.count-1]=owner;v.set(s);true
    })
}

// C11 visual candidate: a small, neutral native-Light-derived thermal floor.
// Only the verified ordinary Merc / actual Char selected material. Not a smoke
// classifier fix or an assertion about the final PS family / displayed pixels.
static MERC_FLOOR_READY: AtomicBool = AtomicBool::new(false);
static MERC_FLOOR_LOGS: AtomicU32 = AtomicU32::new(0);
unsafe fn apply_merc_floor(
    scope: BrightnessScope, root: usize, render: usize, object: usize,
    selected: usize, bank: usize,
) -> Option<([f32; 3], [f32; 3])> {
    if scope.mode != 8 || scope.bank == 0 || scope.bank != bank
        || scope.root != root || scope.render != render || object == 0 || selected == 0
    { return None; }
    // Native already read these live render/selected/proxy arguments. Validate
    // the complete captured view owner again, never use previous-draw PS state.
    if raw(render, 0x236c) as usize != scope.pass
        || raw(scope.pass, 0x14) as usize != render
        || raw(scope.pass, 0) as usize != root
        || raw(scope.pass, 8) as usize != scope.table
        || raw(scope.table, 8 * 4) as usize != scope.shader
        || raw(scope.shader, 0) != 0x02c094a4
        || raw(scope.shader, 4) as usize != root
        || raw(render, 0x20d0) as usize != scope.view
        || raw(scope.view, 0x340) != 8 || raw(root, 0) != 0x02c06404
        || raw(render, 0x233c) != 4 || raw(selected, 0) != 0x02c06404
        || raw(selected, 0x14) != 0xa7bb48a1
        || raw(selected, 0x18c) & 0x00ffffff != 0x00fbfb01
        || raw(object, 0x210) >> 24 != 251
        || (raw(object, 0x210) >> 8) & 0xff != 255
        || (raw(object, 0x34) & 0x400 == 0
            && raw(object, 0x38) & (0x10000000 | 0xc0000) == 0)
    { return None; }
    let owner = raw(object, 0x20) as usize;
    // One successful checked span per owner within this synchronous outer view; no
    // per-field/per-material VQ and no retained address cache across frames.
    if !merc_owner_readable(owner) || raw(owner, 0) != 0x02ad2abc
        || raw(owner, 0x30c) as usize != object
        || raw(owner, 0x144) == 0 || raw(owner, 0x148) == 0
    { return None; }
    let serial = raw(owner, 0x20);
    let before: [f32; 3] = std::array::from_fn(|i| f32::from_bits(raw(bank, 0xc50 + i * 4)));
    let light: [f32; 3] = std::array::from_fn(|i| f32::from_bits(raw(bank, 0xc80 + i * 4)));
    if before.iter().chain(light.iter()).any(|v| !v.is_finite() || *v < 0.0) { return None; }
    // Real Char Light is neutral 200/255: /16 = 0.0490196, below 0.05.
    // Derive from its own Light, never copy Surv's palette or ability state.
    let after = std::array::from_fn(|i| before[i].max((light[i] * (1.0 / 16.0)).min(0.05)));
    if before == after || raw(object, 0x20) as usize != owner
        || raw(owner, 0x20) != serial || raw(owner, 0x30c) as usize != object
    { return None; }
    for (i, value) in after.iter().enumerate() { put(bank, 0xc50 + i * 4, value.to_bits()); }
    // Exactly three CPU constants. Next successful native upload replaces all
    // 0x90 bytes; selected props, BaseColor/hue, alpha, AO and 253 stay native.
    Some((before, after))
}

unsafe extern "thiscall" fn material(root: usize, render: usize, object: usize, surface: usize) -> usize {
    unsafe {
        causal_probe::material_call();
        let selected = Material.get().expect("published before enable").call(root, render, object, surface);
        if material_observer::enabled() {
            material_observer::clear_verified();
            let scope = BRIGHTNESS_SCOPE.with(Cell::get);
            if scope.mode == 8 && scope.root == root && scope.render == render {
                material_observer::after_native(scope.pass, root, render, object, surface, selected);
            }
            final_ps_probe::material_verified(material_observer::take_verified());
        }
        if selected != 0 && BRIGHTNESS_READY.load(Ordering::Acquire) && !native_comparison() {
            let scope = BRIGHTNESS_SCOPE.with(Cell::get);
            let bank = (0x33c3ed0 as *const u32).read() as usize;
            if MERC_FLOOR_READY.load(Ordering::Acquire) {
            if let Some((before, after)) = apply_merc_floor(scope, root, render, object, selected, bank) {
                if take_brightness_log(&MERC_FLOOR_LOGS) {
                    tracing::info!("SCBL_THERMAL_MERC_FLOOR revision=C11 category=251 selected={:#x} key=a7bb48a1 stage=4 before={:?} after={:?} base_native=true ability_native=true gpu_acceptance_pending=true", selected, before, after);
                }
            }
            }
            if let Some((before, after)) =
                apply_brightness(scope, root, render, object, selected, bank)
            {
                let stage = raw(render, 0x233c);
                if let Some(bucket) = brightness_log_bucket(scope.mode, stage) {
                    if take_brightness_log(&BRIGHTNESS_LOGS[bucket]) {
                        tracing::info!("SCBL_MOD_VISION_BRIGHTNESS revision=C66 mode={} stage={} object={:#x} selected={:#x} surface={:#x} before={:?} after={:?} envelope={:?} palette_native=true alpha_native=true character_unchanged=true gpu_state_hooks=0 render_acceptance_pending=true",scope.mode,stage,object,selected,surface,before,after,material_envelope(bank));
                    }
                }
            }
        }
        selected
    }
}

fn scene_highlight(mode: u32, lo: u32, hi: u32, heat: u8) -> bool {
    // Retail 1755D50's character predicate: lo:400 / hi:C0000, plus EMF
    // character family hi:10000000. lo:200/100/60 alone are NOT this predicate.
    let character = lo & 0x400 != 0 || hi & (0x10000000 | 0xc0000) != 0;
    // These are native highlighted scene objects, not a new visibility source.
    !character
        && lo & 0x10 == 0
        && match mode {
            9 => hi & 0x20000000 != 0,
            8 => heat > 0 && (heat < 255 || hi & 0x08000000 != 0),
            _ => false,
        }
}
#[derive(Clone, Copy, Default)]
struct Cached {
    object: usize,
    eligible: bool,
}
struct Policy {
    mode: u32,
    cache: [Cached; 256],
    readable_blocks: [usize; 128],
}
impl Policy {
    fn new(mode: u32) -> Self {
        Self {
            mode,
            cache: [Cached::default(); 256],
            readable_blocks: [0; 128],
        }
    }
    // Cache only successful checks of complete 4 KiB address blocks, not OS
    // page assumptions. A partial/unreadable block falls back to the exact
    // original range check. No negative cache, allocation or cross-frame state.
    // The render pass owns these objects through compaction; invalidate before
    // any native callback, just like the existing object eligibility cache.
    unsafe fn readable_fields(&mut self, p: usize, n: usize) -> bool {
        if p < 0x10000 || n == 0 { return false; }
        let Some(end) = p.checked_add(n) else { return false; };
        let block = p & !0xfff;
        let Some(limit) = block.checked_add(0x1000) else { return false; };
        if end > limit { return readable(p, n); }
        let slot = (block >> 12) & 127;
        if self.readable_blocks[slot] == block { return true; }
        if readable(block, 0x1000) {
            self.readable_blocks[slot] = block;
            true
        } else {
            readable(p, n)
        }
    }
    unsafe fn eligible(&mut self, object: usize) -> bool {
        let i = (object >> 4) & 255;
        if object != 0 && self.cache[i].object == object {
            return self.cache[i].eligible;
        }
        let yes = object.checked_add(0x34).is_some_and(|p| self.readable_fields(p, 0x1e0))
            && scene_highlight(
                self.mode,
                raw(object, 0x34),
                raw(object, 0x38),
                (raw(object, 0x210) >> 24) as u8,
            );
        self.cache[i] = Cached {
            object,
            eligible: yes,
        };
        yes
    }
}
unsafe fn raw(p: usize, o: usize) -> u32 {
    ((p + o) as *const u32).read_unaligned()
}
unsafe fn put(p: usize, o: usize, v: u32) {
    ((p + o) as *mut u32).write_unaligned(v)
}
// C60: native queue allocations can span adjacent VirtualQuery regions.
// Keep the original fast path. On a boundary only, verify every 4 KiB page
// segment with the same committed/read-write/no-guard checks; never skip an
// inaccessible tail merely because the current length fits the first region.
// This is queue-local: do not relax the shared memory checks for other hooks.
unsafe fn queue_writable(p: usize, n: usize) -> bool {
    if p < 0x10000 || n == 0 {
        return false;
    }
    let Some(end) = p.checked_add(n) else {
        return false;
    };
    if writable(p, n) {
        return true;
    }
    let mut current = p;
    while current < end {
        let chunk = (0x1000 - (current & 0xfff)).min(end - current);
        if !writable(current, chunk) {
            return false;
        }
        current += chunk;
    }
    true
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Array {
    header: usize,
    data: usize,
    len: usize,
    cap: usize,
}
impl Array {
    unsafe fn load(header: usize) -> Option<Self> {
        if !queue_writable(header, 12) {
            return None;
        }
        let (data, len, cap) = (
            raw(header, 0) as usize,
            raw(header, 4) as usize,
            raw(header, 8) as usize,
        );
        if len > cap || cap > MAX_ITEMS || (cap != 0 && !queue_writable(data, cap * 4)) {
            return None;
        }
        Some(Self {
            header,
            data,
            len,
            cap,
        })
    }
    unsafe fn get(&self, i: usize) -> usize {
        debug_assert!(i < self.len);
        raw(self.data, i * 4) as usize
    }
    unsafe fn contains(&self, object: usize) -> bool {
        (0..self.len).any(|i| self.get(i) == object)
    }
    unsafe fn push(&mut self, object: usize) -> bool {
        if self.contains(object) { return true; }
        // Growth is completed and revalidated before either occlusion queue is
        // compacted. There must be no native callback inside compaction.
        if self.len == self.cap { return false; }
        put(self.data, self.len * 4, object as u32);
        self.len += 1;
        put(self.header, 4, self.len as u32);
        true
    }
    unsafe fn push_indexed(&mut self, object: usize, index: &mut HashSet<usize>) -> bool {
        if index.contains(&object) { return true; }
        if self.len == self.cap { return false; }
        put(self.data, self.len * 4, object as u32);
        self.len += 1;
        put(self.header, 4, self.len as u32);
        index.insert(object);
        true
    }
    unsafe fn compact(&mut self, mut remove: impl FnMut(usize) -> bool) -> u32 {
        let mut kept = 0;
        let old = self.len;
        for i in 0..old {
            let p = self.get(i);
            if !remove(p) {
                put(self.data, kept * 4, p as u32);
                kept += 1;
            }
        }
        self.len = kept;
        put(self.header, 4, kept as u32);
        (old - kept) as u32
    }
}
fn overlap(a: usize, an: usize, b: usize, bn: usize) -> bool {
    an != 0 && bn != 0 && a < b.saturating_add(bn) && b < a.saturating_add(an)
}
fn independent_queues(queues: [Array; 3]) -> bool {
    // Loading validated each extent. Writable is not exclusive ownership:
    // neither backing storage nor queue headers may alias one another.
    for (i, a) in queues.iter().enumerate() {
        for (j, b) in queues.iter().enumerate() {
            if overlap(a.data, a.cap * 4, b.header, 12)
                || (i != j && (overlap(a.data, a.cap * 4, b.data, b.cap * 4)
                    || overlap(a.header, 12, b.header, 12)))
            { return false; }
        }
    }
    true
}
unsafe fn same_route(pass: usize, render: usize, view: usize, mode: u32, table: usize, selected: usize) -> bool {
    word(pass, 0x14) == Some(render as u32)
        && word(render, 0x236c) == Some(pass as u32)
        && word(render, 0x20d0) == Some(view as u32)
        && word(view, 0x340) == Some(mode)
        && word(pass, 8) == Some(table as u32)
        && word(table, mode as usize * 4) == Some(selected as u32)
        && word(selected, 0) == Some(if mode == 9 { 0x2c0965c } else { 0x2c094a4 })
}
#[derive(Default, Debug, PartialEq, Eq)]
struct Changes {
    primary: u32,
    highlight: u32,
    mask: u32,
}
unsafe fn reroute(pass: usize, grow: Grow) -> Changes {
    let unchanged = Changes::default();
    let Some(render) = word(pass, 0x14).map(|p| p as usize) else {
        return unchanged;
    };
    if word(render, 0x236c) != Some(pass as u32) {
        return unchanged;
    }
    let Some(view) = word(render, 0x20d0).map(|p| p as usize) else {
        return unchanged;
    };
    let Some(mode) = word(view, 0x340) else {
        return unchanged;
    };
    if mode != 8 && mode != 9 {
        return unchanged;
    }
    // Resolve the current mode through the native per-mode shader table.
    // This does not depend on the cached +10 selection. Retail's full view
    // function sets +10 before invoking the classifier; standalone calls need
    // not have done so. Keep the explicit mode-table validation in either case.
    let Some(selected) = word(pass, 8)
        .and_then(|table| word(table as usize, mode as usize * 4))
        .map(|p| p as usize)
    else {
        note_skip(0, mode, pass);
        return unchanged;
    };
    if word(selected, 0) != Some(if mode == 9 { 0x2c0965c } else { 0x2c094a4 }) {
        note_skip(1, mode, pass);
        return unchanged;
    }
    let (colour_offset, mask_offset) = if mode == 9 {
        (0xcc, 0xd8)
    } else {
        (0x6c, 0x60)
    };
    // Validate every queue we can modify before changing one. Background and
    // the other view mode's queues are deliberately not read or written.
    let (Some(mut primary), Some(mut colour), Some(mut mask)) = (
        Array::load(pass + 0x24),
        Array::load(pass + colour_offset),
        Array::load(pass + mask_offset),
    ) else {
        note_skip(3, mode, pass);
        return unchanged;
    };
    if !independent_queues([primary, colour, mask]) { return unchanged; }
    let table = word(pass, 8).unwrap_or(0) as usize;
    let mut policy = Policy::new(mode);
    // Count missing unique props without allocating a per-frame vector. Reserve
    // first so native Grow never runs while primary/mask compaction is writing.
    // The usual preallocated queue already has room for every primary entry.
    // Do not add a second eligibility/deduplication walk on that hot path.
    let missing = if primary.len <= colour.cap - colour.len { 0 } else {
        // A tight queue can require this preflight even when no Grow is needed.
        // Index only this large, capacity-shortage branch. Both sets die before
        // any native callback; allocation failure retains the original scan.
        let mut preflight_indexes = if primary.len.saturating_add(mask.len) >= 256 {
            let mut preflight_colours = HashSet::new();
            let mut preflight_seen = HashSet::new();
            if preflight_colours.try_reserve(colour.len).is_ok()
                && preflight_seen.try_reserve(primary.len).is_ok()
            {
                for i in 0..colour.len { preflight_colours.insert(colour.get(i)); }
                Some((preflight_colours, preflight_seen))
            } else { None }
        } else { None };
        (0..primary.len).filter(|&i| {
            let o = primary.get(i);
            match preflight_indexes.as_mut() {
                Some((preflight_colours, preflight_seen)) => {
                    // Record every prior primary pointer, not just eligible ones,
                    // matching the original prefix test for mixed native queues.
                    let first = preflight_seen.insert(o);
                    policy.eligible(o) && !preflight_colours.contains(&o) && first
                }
                None => policy.eligible(o) && !colour.contains(o)
                    && !(0..i).any(|j| primary.get(j) == o),
            }
        }).count()
    };
    let Some(required) = colour.len.checked_add(missing).filter(|n| *n <= MAX_ITEMS) else { return unchanged };
    while colour.cap < required {
        if colour.cap >= MAX_ITEMS / 2 { return unchanged; }
        let previous = colour;
        // No cached object facts or readable blocks may survive native Grow.
        policy = Policy::new(mode);
        grow(colour.header);
        // Grow may relocate only colour storage under the native allocator
        // contract. Reject changed owner/peer queues, shape or newly aliased
        // storage before any of our queue/header writes. Do not undo callbacks.
        if !same_route(pass, render, view, mode, table, selected) { return unchanged; }
        let (Some(p), Some(c), Some(m)) = (Array::load(primary.header), Array::load(colour.header), Array::load(mask.header)) else { return unchanged };
        if p != primary || m != mask || c.len != previous.len || c.cap <= previous.cap
            || !independent_queues([p, c, m]) { return unchanged; }
        colour = c;
    }
    let old_colour_count = colour.len;
    // Large native queues otherwise repeat linear colour/primary membership
    // scans for every item. Keep both indexes local to this validated pass;
    // allocation failure takes the unchanged linear path.
    let mut indexes = if primary.len.saturating_add(mask.len) >= 256 {
        let mut colours = HashSet::new();
        let mut primaries = HashSet::new();
        if colours.try_reserve(colour.len.saturating_add(primary.len).min(colour.cap)).is_ok()
            && primaries.try_reserve(primary.len).is_ok()
        {
            for i in 0..colour.len { colours.insert(colour.get(i)); }
            Some((colours, primaries))
        } else { None }
    } else { None };
    // +24 contributes to the early native depth pass. Keep each scene object's
    // original native colour path before removing its occlusion membership.
    // A primary-only object is registered once; allocation failure leaves it
    // entirely native. Never delete objects from +CC/+6C as C53 did.
    let p = primary.compact(|o| {
        policy.eligible(o) && match indexes.as_mut() {
            Some((colours, _)) => colour.push_indexed(o, colours),
            None => colour.push(o),
        }
    });
    if let Some((_, primaries)) = indexes.as_mut() {
        for i in 0..primary.len { primaries.insert(primary.get(i)); }
    }
    let m = mask.compact(|o| {
        match indexes.as_ref() {
            // The large-queue indexes already prove these objects cannot be
            // removed. Do not query/read their fields merely to retain them.
            // No callback or ownership change occurs between index and use.
            Some((colours, primaries)) => colours.contains(&o)
                && !primaries.contains(&o) && policy.eligible(o),
            // Preserve the small/allocation-fallback predicate order: moving
            // linear membership scans ahead of eligibility can cost more.
            None => policy.eligible(o) && colour.contains(o) && !primary.contains(o),
        }
    });
    let a = (colour.len - old_colour_count) as u32;
    if p + a + m == 0 {
        note_skip(4, mode, pass);
    }
    Changes {
        primary: p,
        highlight: a,
        mask: m,
    }
}

fn note_skip(reason: u32, mode: u32, pass: usize) {
    // One log per reason, never per draw/frame. The guarded read-only values
    // distinguish invalid queues from a genuinely empty eligible scene.
    if !READY.load(Ordering::Acquire)
        || SKIPS.fetch_or(1 << reason, Ordering::Relaxed) & (1 << reason) != 0
    {
        return;
    }
    unsafe {
        let render = word(pass, 0x14).unwrap_or(0) as usize;
        tracing::info!("SCBL_MOD_VISION_ROUTE revision=C60 reason={} mode={} view_index={:?} primary={:?} emf={:?} thermal={:?}",reason,mode,word(render,0x2208),word(pass,0x28),word(pass,0xd0),word(pass,0x70));
    }
}
unsafe extern "thiscall" fn classify(pass: usize) {
    unsafe {
        BRIGHTNESS_SCOPE.with(|s| s.set(BrightnessScope::default()));
        let causal_start = causal_probe::classify_start();
        Classify.get().expect("published before enable").call(pass);
        causal_probe::classify_end(pass, causal_start);
        if !READY.load(Ordering::Acquire) || native_comparison() {
            causal_probe::classify_done();
            return;
        }
        if BRIGHTNESS_READY.load(Ordering::Acquire) {
            let bank = (0x33c3ed0 as *const u32).read() as usize;
            if let Some(scope) = brightness_scope(pass, bank) {
                BRIGHTNESS_SCOPE.with(|s| s.set(scope));
            }
        }
        let changes = reroute(pass, std::mem::transmute::<usize, Grow>(0x665120));
        causal_probe::classify_done();
        if changes != Changes::default()
            && LOGS.load(Ordering::Relaxed) < 4
            && LOGS.fetch_add(1, Ordering::Relaxed) < 4
        {
            tracing::info!("SCBL_MOD_VISION_TRANSPARENCY revision=C60 native_colour_preserved=true fixed_opacity=false gpu_state_hooks=0 depth_primary_removed={} colour_added={} depth_mask_removed={} render_acceptance_pending=true",changes.primary,changes.highlight,changes.mask);
        }
    }
}
const CONTRACTS: &[(usize, usize, &str)] = &[
    (
        0x1755d50,
        0x992,
        "48e02c24f1ab417991485eb1f76342b7dcad61b80cfd46ba01f201ccefe439e9",
    ),
    (
        0x665120,
        0x68,
        "d4a2eaf00871ea94f74d487dc6325c796fb2db3ff5e45f6df3b7b27f0420509a",
    ),
    (
        0x175b539,
        0x40,
        "ede04a9d8069f363b1bfb691dbbfe8e9d0dd0169d52d8ec9d8caee120b195da3",
    ),
    (
        0x175b8c2,
        0xd1,
        "7ea31860424c51b435a097226a3890607a739f5dc86e801a6f511586080d7476",
    ),
    (
        0x175b9ce,
        0x75,
        "774c19605261f29d3a383eb2798f0885cfdef66179c4ba88af7c2258ed908edb",
    ),
    (
        0x175bfe5,
        0x17a,
        "5d0fb24359ddcb2f0931db18b3bb9f2f59992aeb0fb8a5db7d83aed32a7c3c5e",
    ),
    (
        0x175aec0,
        0xf8,
        "339f344f1bfdcd3ff38f9980af098cb5047c3ffc8728d6c59ddc35a84d7b8540",
    ),
];
const BRIGHTNESS_CONTRACTS: &[(usize, usize, &str)] = &[
    (
        0x16e2a10,
        0x1b3,
        "836e80ffcdab6edac41f9f67b766c9ad3008e34e7c2b687febb65344ac5e19f5",
    ),
    (
        0x16e2000,
        0xc4,
        "97d6e8ef04d09e5aaa35697c38cf33044f9c8f9a730d23da165952d2119dfdfa",
    ),
    (
        0x16e2d40,
        0x278,
        "4ede17cf5f31be99fd7a7635fdd3237c2d01021119117f6358df58492c78a02f",
    ),
];
#[cfg(test)] // Historical C62 upload; not the ViewModeDiffCol semantic mapping.
const CONTRAST_UPLOAD_CONTRACT: (usize, usize, &str) = (
    0x175bf20,
    0x1b,
    "a220d85f9bdb670761e19abe56c25da384b02bb0069c3168130e353ef560c297",
);
const CONTRAST_NATIVE_CONTRACTS: &[(usize, usize, &str)] = &[
    (
        0x31938a8,
        0x108,
        "e3b98e8559f292815da93cc54bcd7a8896724d0a9ed3e4fbef4f4b3b12633984",
    ),
    (
        0x3193068,
        0x108,
        "d16afcbf3a3f8cc841b4b446c4a4b11d290f12d4892c2ab7848c53f890ed4342",
    ),
    (
        0x1625c90,
        0x1b0,
        "d19554ff4c2fc89b76f1b78f2e5aaf2b1cb0f9869ddf40ecc27077727c6e328c",
    ),
    (
        0x16e4645,
        0x4f,
        "ca56f71d4ac80cfa5a2dff5bb1471e2b8f5c36ae1b978a58e45746699eaeec42",
    ),
    (
        0x16e5668,
        0x1f,
        "e0e65c8f100e2cc4cf964b39be397611181cb97d72bf4023a5cc3f3998b17bb5",
    ),
    (
        0x2d75c00,
        0x4,
        "e00e5eb9444182f352323374ef4e08ebcb784725fdd4fd612d7730540b3e0c8c",
    ),
];
// Both adapters retain their independent build/scope/environment guards.
// The common owner controls only the outer native call, not mode eligibility.
pub(crate) unsafe fn with_native_view(pass: usize, native: impl FnOnce()) {
    with_merc_view(|| material_observer::with_view(pass, || causal_probe::with_view(pass, || constant_reuse::with_view(pass, native))));
}
// Failure of the shared owner must not leave its upload/queue observers active.
// Deliberately does not stop classifier/material, thermal smoke, or range hooks.
pub(crate) unsafe fn deinit_view_clients() {
    final_ps_probe::deinit();
    material_observer::deinit();
    constant_reuse::deinit();
    causal_probe::deinit();
}
pub(crate) unsafe fn init(world: usize) {
    if world != 0x589860 || !CONTRACTS.iter().all(|&(p, n, h)| signature(p, n, h)) {
        return;
    }
    if comparison_build() {
        let requested = std::env::var("SCBL_VISION_NATIVE_COMPARE");
        let without_hooks = requested.as_deref() == Ok("2");
        NATIVE_COMPARE.store(
            matches!(requested.as_deref(), Ok("1") | Ok("2")),
            Ordering::Release,
        );
        tracing::info!("SCBL_VISION_COMPARE diagnostic_build=true native_visuals={} hooks_disabled={} scope=custom_prop_route_and_rgb_only range_unchanged=true", native_comparison(), without_hooks);
        // Private C control: native prop rendering WITHOUT the two detours.
        // Keep range and other gameplay untouched; regular builds ignore it.
        if without_hooks {
            tracing::info!("SCBL_MOD_VISION_TRANSPARENCY initialized=false reason=diagnostic_no_hooks");
            tracing::info!("SCBL_MOD_VISION_BRIGHTNESS initialized=false reason=diagnostic_no_hooks");
            return;
        }
    }
    if install_direct(
        &Classify,
        std::mem::transmute::<usize, ClassifyFn>(0x1755d50),
        classify as ClassifyFn,
    )
    {
        READY.store(true, Ordering::Release);
        causal_probe::init();
        constant_reuse::init();
        tracing::info!("SCBL_MOD_VISION_TRANSPARENCY revision=C60 native_colour_preserved=true fixed_opacity=false gpu_state_hooks=0 initialized=true");
        if BRIGHTNESS_CONTRACTS
            .iter()
            .all(|&(p, n, h)| signature(p, n, h))
            && CONTRAST_NATIVE_CONTRACTS
                .iter()
                .all(|&(p, n, h)| signature(p, n, h))
            && install_direct(
                &Material,
                std::mem::transmute::<usize, MaterialFn>(0x16e2a10),
                material as MaterialFn,
            )
        {
            BRIGHTNESS_READY.store(true, Ordering::Release);
            // Extra contract gates only C11, never the pre-existing scene policy.
            MERC_FLOOR_READY.store(signature(0x16e452e, 13, "731868ecabed500c7ae9ca943c60f2931dfbbfa22157382196a26a14d333b7e9"), Ordering::Release);
            material_observer::init();
            final_ps_probe::init();
            tracing::info!("SCBL_MOD_VISION_BRIGHTNESS revision=C66 emf_signal_knee=0.40 emf_gain_sqrt=true emf_low_signal_slope=1 thermal_signal_ceiling=0.20 thermal_first_family_unchanged=true thermal_second_family_gain=0.35 native_gain_inputs=true view_colour_unit_bound=true native_palette_and_distance=true initialized=true gpu_state_hooks=0");
        }
    }
}
pub(crate) unsafe fn deinit() {
    final_ps_probe::deinit();
    material_observer::deinit();
    constant_reuse::deinit();
    causal_probe::deinit();
    MERC_FLOOR_READY.store(false, Ordering::Release);
    BRIGHTNESS_READY.store(false, Ordering::Release);
    if let Some(hook) = Material.get() { let _ = hook.disable(); }
    BRIGHTNESS_SCOPE.with(|s| s.set(BrightnessScope::default()));
    READY.store(false, Ordering::Release);
    if let Some(hook) = Classify.get() { let _ = hook.disable(); }
    NATIVE_COMPARE.store(false, Ordering::Release);
}
#[cfg(test)]
#[path = "vision_transparency_tests.rs"]
mod tests;
