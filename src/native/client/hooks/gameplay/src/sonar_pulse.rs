//! C69 trial: independent per-character sonar tails, native palette and draw passes.
//! Only MP sonar mode 7. Native classifier remains the authority for eligibility.
use crate::equipment_native::{float, signature, word, writable};
use retour::static_detour;
use std::{
    cell::RefCell,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
const SPEED: f32 = 3000.0;
const PERIOD: f64 = 3.0;
const END_PHASE: f32 = 9000.0;
const FADE_OFFSETS: [usize; 6] = [0xb90, 0xb94, 0xbb0, 0xbb4, 0xbd0, 0xbd4];
static_detour! {
    static Wave: unsafe extern "cdecl" fn(f32, usize);
    static Gate: unsafe extern "thiscall" fn(usize, usize, f32, f32) -> u8;
    static EndDraw: unsafe extern "thiscall" fn(usize, usize);
}
#[derive(Clone, Copy, Default)]
struct Target {
    object: usize,
    mesh: usize,
    wave: u64,
    hit: f64,
    frame: u64,
    used: bool,
}
struct Tracker {
    render: usize,
    context: usize,
    profile: usize,
    phase: f32,
    cycle: u64,
    generation: u64,
    wave_start: f64,
    emitting: bool,
    frame: u64,
    now: f64,
    targets: [Target; 128],
}
impl Default for Tracker {
    fn default() -> Self {
        Self {
            render: 0,
            context: 0,
            profile: 0,
            phase: 0.0,
            cycle: 0,
            generation: 0,
            wave_start: 0.0,
            emitting: true,
            frame: 0,
            now: 0.0,
            targets: [Target::default(); 128],
        }
    }
}
fn visibility(age: f64) -> f32 {
    if !age.is_finite() || age < 0.0 || age >= PERIOD {
        0.0
    } else if age <= 1.5 {
        1.0
    } else {
        ((PERIOD - age) / 1.5) as f32
    }
}
impl Tracker {
    fn begin(&mut self, render: usize, context: usize, profile: usize, mode: u32, phase: f32) {
        if mode != 7 || !phase.is_finite() || !(0.0..=END_PHASE).contains(&phase) {
            *self = Self::default();
            return;
        }
        let same = self.render == render && self.context == context && self.profile == profile;
        if !same || (phase < self.phase && !(self.phase > 5000.0 && phase < 1000.0)) {
            *self = Self::default();
            self.render = render;
            self.context = context;
            self.profile = profile;
        } else if phase < self.phase {
            self.cycle += 1;
        }
        self.wave_start = self.cycle as f64 * PERIOD;
        self.emitting = true;
        self.phase = phase;
        self.now = self.cycle as f64 * PERIOD + phase as f64 / SPEED as f64;
        self.frame = self.frame.wrapping_add(1);
        for t in &mut self.targets {
            if t.used && (self.now - t.hit >= PERIOD || self.now < t.hit) {
                t.used = false;
            }
        }
    }
    fn begin_clock(&mut self, render: usize, context: usize, clock: Clock) {
        if self.render != render
            || self.context != context
            || self.profile != clock.profile
            || self.generation != clock.generation
        {
            *self = Self::default();
            self.render = render;
            self.context = context;
            self.profile = clock.profile;
            self.generation = clock.generation;
        }
        self.cycle = clock.cycle;
        self.phase = clock.phase;
        self.now = clock.now;
        self.wave_start = clock.start;
        self.emitting = clock.active;
        self.frame = self.frame.wrapping_add(1);
        for t in &mut self.targets {
            if t.used && (self.now < t.hit || self.now - t.hit >= PERIOD) {
                t.used = false;
            }
        }
    }
    fn encounter(
        &mut self,
        object: usize,
        mesh: usize,
        distance: f32,
        radius: f32,
        range: f32,
    ) -> Option<u8> {
        if self.render == 0
            || ![distance, radius, range].iter().all(|v| v.is_finite())
            || distance < 0.0
            || !(0.0..=1000.0).contains(&radius)
            || !(0.0..=5000.0).contains(&range)
        {
            return None;
        }
        let mut index = self
            .targets
            .iter()
            .position(|t| t.used && t.object == object && t.mesh == mesh);
        if distance - radius >= range {
            if let Some(i) = index {
                self.targets[i].used = false;
            }
            return Some(1);
        }
        // The same native bounding-sphere wave-front test, but one hit per wave.
        let reached = self.emitting && distance - radius <= self.phase;
        if reached && index.map_or(true, |i| self.targets[i].wave != self.cycle) {
            if index.is_none() {
                index = self.targets.iter().position(|t| !t.used);
            }
            if let Some(i) = index {
                let hit = self.wave_start + (distance - radius).max(0.0) as f64 / SPEED as f64;
                self.targets[i] = Target {
                    object,
                    mesh,
                    wave: self.cycle,
                    hit,
                    frame: self.frame,
                    used: true,
                };
            } else {
                return None;
            } // bounded storage: fall back to native, never invent a target.
        }
        if let Some(i) = index {
            let t = &mut self.targets[i];
            if visibility(self.now - t.hit) > 0.0 {
                t.frame = self.frame;
                return Some(0);
            }
            t.used = false;
        }
        Some(1)
    }
    fn alpha(&self, render: usize, object: usize, mesh: usize) -> Option<f32> {
        if self.render != render {
            return None;
        }
        self.targets
            .iter()
            .find(|t| t.used && t.object == object && t.mesh == mesh && t.frame == self.frame)
            .map(|t| visibility(self.now - t.hit))
    }
}
#[derive(Clone, Copy, Default)]
struct Clock {
    view: usize,
    profile: usize,
    generation: u64,
    now: f64,
    start: f64,
    cycle: u64,
    phase: f32,
    active: bool,
    attached: bool,
}
static CLOCKS: std::sync::Mutex<([Clock; 8], u64)> = std::sync::Mutex::new((
    [Clock {
        view: 0,
        profile: 0,
        generation: 0,
        now: 0.0,
        start: 0.0,
        cycle: 0,
        phase: 0.0,
        active: false,
        attached: false,
    }; 8],
    0,
));
unsafe fn advance_clock(view: usize, dt: f32, started: bool) -> bool {
    let fields = (|| {
        let state = word(view, 0x538)? as usize;
        Some((
            state,
            word(state, 0x1c)?,
            word(state, 4)? as usize,
            float(state, 0x38)?,
        ))
    })();
    let Ok(mut clocks) = CLOCKS.lock() else {
        return false;
    };
    let slot = clocks
        .0
        .iter()
        .position(|x| x.view == view)
        .or_else(|| clocks.0.iter().position(|x| x.view == 0))
        .or_else(|| {
            if fields.map_or(false, |x| x.1 == 7) {
                clocks
                    .0
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, x)| x.generation)
                    .map(|(i, _)| i)
            } else {
                None
            }
        });
    let Some(i) = slot else {
        return false;
    };
    let Some((state, mode, profile, phase)) = fields else {
        clocks.0[i] = Clock::default();
        return false;
    };
    if mode != 7 || !dt.is_finite() || !(0.0..=1.0).contains(&dt) || !phase.is_finite() {
        clocks.0[i] = Clock::default();
        return false;
    }
    if clocks.0[i].view != view || clocks.0[i].profile != profile {
        clocks.1 = clocks.1.wrapping_add(1);
        clocks.0[i] = Clock {
            view,
            profile,
            generation: clocks.1,
            ..Clock::default()
        };
    }
    let x = &mut clocks.0[i];
    x.now += dt as f64;
    x.phase = phase;
    x.active = word(state, 0x34).map_or(false, |v| v & 255 != 0);
    if started {
        x.cycle = x.cycle.wrapping_add(1);
        x.start = x.now - phase as f64 / SPEED as f64;
    }
    x.attached
}
fn render_clock(profile: usize) -> Option<Clock> {
    let mut clocks = CLOCKS.lock().ok()?;
    let x = clocks
        .0
        .iter_mut()
        .find(|x| x.view != 0 && x.profile == profile)?;
    x.attached = true;
    Some(*x)
}
fn ended_clock(view: usize) {
    if let Ok(mut clocks) = CLOCKS.lock() {
        if let Some(x) = clocks.0.iter_mut().find(|x| x.view == view) {
            x.active = false;
            x.phase = 0.0;
        }
    }
}
#[derive(Clone, Copy)]
struct Saved {
    bank: usize,
    old: [u32; 6],
    written: [u32; 6],
}
thread_local! {
    static TRACK: RefCell<Tracker> = RefCell::new(Tracker::default());
    static SAVED: RefCell<Option<Saved>> = const {RefCell::new(None)};
}
unsafe fn restore(saved: Saved) {
    // Never overwrite a newer native pass upload, nor a freed/replaced bank.
    if !writable(saved.bank + 0xb90, 0x48) {
        return;
    }
    if FADE_OFFSETS
        .iter()
        .enumerate()
        .all(|(i, o)| ((saved.bank + o) as *const u32).read_unaligned() == saved.written[i])
    {
        for (i, o) in FADE_OFFSETS.iter().enumerate() {
            ((saved.bank + o) as *mut u32).write_unaligned(saved.old[i]);
        }
    }
}
pub(crate) unsafe fn restore_material() {
    SAVED.with(|s| {
        if let Some(saved) = s.borrow_mut().take() {
            restore(saved);
        }
    });
}
unsafe fn character(object: usize) -> Option<bool> {
    let lo = word(object, 0x34)?;
    let hi = word(object, 0x38)?;
    Some(lo & 0x400 != 0 || hi & 0xc0000 != 0)
}
unsafe fn context(pass: usize) -> Option<(usize, usize)> {
    let render = word(pass, 0x14)? as usize;
    if word(render, 0x236c)? as usize != pass {
        return None;
    }
    Some((render, word(render, 0x20d0)? as usize))
}
pub(crate) unsafe fn frame(pass: usize) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    restore_material();
    let fields = (|| {
        let (r, c) = context(pass)?;
        Some((
            r,
            c,
            word(c, 0x2d0)? as usize,
            word(c, 0x340)?,
            float(c, 0x2ec)?,
        ))
    })();
    TRACK.with(|t| {
        let mut t = t.borrow_mut();
        if let Some((r, c, p, 7, _)) = fields {
            if let Some(clock) = render_clock(p) {
                t.begin_clock(r, c, clock);
                return;
            }
        }
        *t = Tracker::default();
    });
}
unsafe fn gate_override(pass: usize, object: usize, range: f32) -> Option<u8> {
    if TRACK.with(|t| t.borrow().render == 0) {
        return None;
    }
    if !character(object)? {
        return None;
    }
    let (render, ctx) = context(pass)?;
    if word(ctx, 0x340)? != 7 {
        return None;
    }
    let dx = float(object, 0xd0)? - float(ctx, 0x2f0)?;
    let dy = float(object, 0xd4)? - float(ctx, 0x2f4)?;
    let distance = (dx * dx + dy * dy).sqrt();
    let radius = float(object, 0x3c)?;
    let mesh = word(object, 0xf8)? as usize;
    TRACK.with(|t| {
        let mut t = t.borrow_mut();
        if t.render != render || t.context != ctx {
            None
        } else {
            t.encounter(object, mesh, distance, radius, range)
        }
    })
}
fn gate(pass: usize, object: usize, tail: f32, range: f32) -> u8 {
    unsafe {
        let native = Gate.call(pass, object, tail, range);
        if READY.load(Ordering::Acquire) {
            if let Some(v) = gate_override(pass, object, range) {
                return v;
            }
        }
        native
    }
}
// Use the native request/end lifecycle: the next native request supplies a fresh
// player-origin/direction, rather than wrapping phase around a stale origin.
pub(super) unsafe fn finish_wave(view: usize) -> bool {
    let Some(state) = word(view, 0x538).map(|v| v as usize) else {
        return false;
    };
    if word(state, 0x1c) != Some(7) || !writable(state + 0x34, 8) {
        return false;
    }
    let Some(phase) = float(state, 0x38) else {
        return false;
    };
    if (state as *const u8).add(0x34).read() != 0 && phase.is_finite() && phase >= END_PHASE {
        ((state + 0x34) as *mut u8).write(0);
        ((state + 0x38) as *mut f32).write_unaligned(0.0);
        return true;
    }
    false
}
fn wave(dt: f32, view: usize) {
    unsafe {
        let state = word(view, 0x538).unwrap_or(0) as usize;
        let before = word(state, 0x34).unwrap_or(0) & 255 != 0;
        Wave.call(dt, view);
        if READY.load(Ordering::Acquire) {
            let active = word(state, 0x34).unwrap_or(0) & 255 != 0;
            let attached = advance_clock(view, dt, !before && active);
            // Do not shorten the native wave until its actual renderer has bound
            // the same private blended profile. A missing bridge stays native.
            if attached && finish_wave(view) {
                ended_clock(view);
                if LOGS.fetch_or(1, Ordering::Relaxed) & 1 == 0 {
                    tracing::info!("SCBL_MOD_SONAR_PULSE revision=C69 wave_period_s=3 renderer_bound=true native_restart=true range_m=50 speed_mps=30 gameplay_acceptance_pending=true");
                }
            }
        }
    }
}
fn end_draw(root: usize, render: usize) {
    unsafe {
        restore_material();
        EndDraw.call(root, render);
    }
}
unsafe fn write_fades(bank: usize, alpha: f32) -> Option<Saved> {
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) || !writable(bank + 0xb90, 0x48) {
        return None;
    }
    let old = std::array::from_fn(|i| ((bank + FADE_OFFSETS[i]) as *const u32).read_unaligned());
    let f = old.map(f32::from_bits);
    // Only native persistence/glow blocks. Do NOT stretch the thin visible scan
    // wave (third slope ~10), alter colors, fog/range/alpha or add a draw pass.
    if !f.iter().all(|v| v.is_finite())
        || f[0] <= 0.0
        || f[2] <= 0.0
        || !(0.0..0.001).contains(&f[4])
    {
        return None;
    }
    let written = [0.0f32, 1.0, 0.0, 1.0, 0.0, 1.0 - alpha].map(f32::to_bits);
    for (i, o) in FADE_OFFSETS.iter().enumerate() {
        ((bank + o) as *mut u32).write_unaligned(written[i]);
    }
    Some(Saved { bank, old, written })
}
pub(crate) unsafe fn material(root: usize, render: usize, object: usize, selected: usize) {
    if !READY.load(Ordering::Acquire)
        || selected == 0
        || !TRACK.with(|t| t.borrow().render == render && render != 0)
        || character(object) != Some(true)
    {
        return;
    }
    let scope = (|| {
        let ctx = word(render, 0x20d0)? as usize;
        if word(ctx, 0x340)? != 7 || word(root, 0)? != 0x2c06404 {
            return None;
        }
        let pass = word(render, 0x236c)? as usize;
        let variants = word(pass, 8)? as usize;
        let variant = word(variants, 7 * 4)? as usize;
        if word(variant, 4)? as usize != root || word(variant, 0)? != 0x2c09454 {
            return None;
        }
        Some(word(object, 0xf8)? as usize)
    })();
    let Some(mesh) = scope else {
        return;
    };
    let alpha = TRACK.with(|t| t.borrow().alpha(render, object, mesh));
    let Some(alpha) = alpha else {
        return;
    };
    let Some(bank) = word(0x33c3ed0, 0).map(|v| v as usize) else {
        return;
    };
    if let Some(saved) = write_fades(bank, alpha) {
        SAVED.with(|s| *s.borrow_mut() = Some(saved));
        if LOGS.fetch_add(2, Ordering::Relaxed) < 24 {
            tracing::info!("SCBL_MOD_SONAR_PULSE revision=C69 character_draw=true stage={:?} alpha={} clear_s=1.5 fade_s=1.5 native_palette=true",word(render,0x233c),alpha);
        }
    }
}
// Native MP restart has a second cooldown in the gadget. In mode7, let the
// three-second wave lifecycle own cadence; retain owner/motion gates and origin.
static RESTART: std::sync::Mutex<Option<retour::RawDetour>> = std::sync::Mutex::new(None);
unsafe fn restart_thunk(site: usize, enabled: usize) -> Option<usize> {
    const ORIGINAL: [u8; 10] = [0x0f, 0x57, 0xc0, 0x0f, 0x2f, 0x86, 0x7c, 7, 0, 0];
    if !crate::equipment_native::readable(site, 10)
        || std::slice::from_raw_parts(site as *const u8, 10) != ORIGINAL
    {
        return None;
    }
    let p = super::VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4) as usize;
    if p == 0 {
        return None;
    }
    let mut code = vec![0x9c, 0x80, 0x3d];
    code.extend_from_slice(&(enabled as u32).to_le_bytes());
    code.push(0);
    code.extend_from_slice(&[0x74, 0]);
    let disabled = code.len() - 1;
    // ECX is native view+538, ESI the gadget, established at B52EA0.
    code.extend_from_slice(&[0x83, 0x79, 0x1c, 7, 0x75, 0]);
    let other = code.len() - 1;
    code.extend_from_slice(&[0x9d, 0x0f, 0x57, 0xc0, 0x0f, 0x2f, 0xc0, 0xeb, 0]);
    let done = code.len() - 1;
    let native = code.len();
    code.push(0x9d);
    code.extend_from_slice(&ORIGINAL);
    let end = code.len();
    code[disabled] = (native - disabled - 1) as u8;
    code[other] = (native - other - 1) as u8;
    code[done] = (end - done - 1) as u8;
    code.push(0xe9);
    let rel = (site + 10).wrapping_sub(p + code.len() + 4) as u32;
    code.extend_from_slice(&rel.to_le_bytes());
    std::ptr::copy_nonoverlapping(code.as_ptr(), p as *mut u8, code.len());
    let mut old = 0;
    if super::VirtualProtect(p as _, 4096, 0x20, &mut old) == 0
        || super::FlushInstructionCache(super::GetCurrentProcess(), p as _, code.len()) == 0
    {
        return None;
    }
    Some(p) // retain executable thunk through process exit, matching C68 lifecycle
}
unsafe fn init_restart() -> bool {
    let Ok(mut slot) = RESTART.lock() else {
        return false;
    };
    let hook = restart_thunk(0xb52eb3, &READY as *const AtomicBool as usize)
        .and_then(|p| retour::RawDetour::new(0xb52eb3 as *const (), p as *const ()).ok());
    if let Some(h) = hook {
        if h.enable().is_ok() {
            *slot = Some(h);
            return true;
        }
    }
    false
}
const CONTRACTS: &[(usize, usize, &str)] = &[
    (
        0xa2e7f0,
        16,
        "6d0ff5e3f990390e444f185b6be84233ce73f4825dfab3b3366cc02ca916a415",
    ),
    (
        0x174fb40,
        16,
        "042cdb4229bd152d29c08de2b7fea02d4a4550d43758545a659e5250672f25e9",
    ),
    (
        0x16e2bd0,
        16,
        "69ae58517868a409ffe2ad542d67df8401fba5cb2bfff04dfc2296629302b03b",
    ),
    (
        0x16e44d0,
        0x36c,
        "6410c05c60565c8071e0e84b18783e9216a243648f05aab044ac15a201ce328c",
    ),
    (
        0xb52e99,
        0x80,
        "c503b28005363bba0013dc93fd0087b9b90abb5461fd2ccd9adefd1a57a764bb",
    ),
];
pub(crate) unsafe fn init() {
    if !crate::vision_transparency::material_ready()
        || !CONTRACTS.iter().all(|&(p, n, h)| signature(p, n, h))
    {
        tracing::warn!("SCBL_MOD_SONAR_PULSE initialized=false reason=contract-or-material-hook");
        return;
    }
    let ok = init_restart()
        && Wave
            .initialize(
                std::mem::transmute::<usize, unsafe extern "cdecl" fn(f32, usize)>(0xa2e7f0),
                wave,
            )
            .and_then(|h| h.enable())
            .is_ok()
        && Gate
            .initialize(
                std::mem::transmute::<
                    usize,
                    unsafe extern "thiscall" fn(usize, usize, f32, f32) -> u8,
                >(0x174fb40),
                gate,
            )
            .and_then(|h| h.enable())
            .is_ok()
        && EndDraw
            .initialize(
                std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, usize)>(0x16e2bd0),
                end_draw,
            )
            .and_then(|h| h.enable())
            .is_ok();
    if ok {
        READY.store(true, Ordering::Release);
        tracing::info!("SCBL_MOD_SONAR_PULSE revision=C69 initialized=true clear_s=1.5 fade_s=1.5 period_s=3 mode=7 candidate=true");
    } else {
        deinit();
        tracing::warn!("SCBL_MOD_SONAR_PULSE initialized=false reason=hook");
    }
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    if let Ok(mut h) = RESTART.lock() {
        if let Some(h) = h.take() {
            let _ = h.disable();
        }
    }
    restore_material();
    let _ = Wave.disable();
    let _ = Gate.disable();
    let _ = EndDraw.disable();
    TRACK.with(|t| *t.borrow_mut() = Tracker::default());
    if let Ok(mut clocks) = CLOCKS.lock() {
        clocks.0 = [Clock::default(); 8];
    }
    LOGS.store(0, Ordering::Relaxed);
}
#[cfg(test)]
#[path = "sonar_pulse_tests.rs"]
mod tests;
