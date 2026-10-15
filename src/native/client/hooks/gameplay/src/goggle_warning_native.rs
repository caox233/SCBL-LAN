//! Private C33 HUD adapter. No detector result is invented or inferred here.
//! The goggle-feedback build consumes bounded validated peer feedback.
//! Default production builds do not compile this module.
use crate::equipment_native::{float, readable, signature, word};
use crate::goggle_exposure::{ActualDetections, Frame, Language, Life, Presentation, Warning};
use crate::goggle_warning_ui::{
    Binding, ColorApi, Transform, TransformApi, Ui, WarningKind, WarningUi,
};
use retour::static_detour;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

const TICK: usize = 0x01a68da0;
const SPY: u32 = 0x02ad3234;
const MERC: u32 = 0x02ad2abc;
static READY: AtomicBool = AtomicBool::new(false);
static_detour! { static Tick: unsafe extern "thiscall" fn(usize, f32); }
type Goggles = unsafe extern "thiscall" fn(usize) -> i32;
type Localize = unsafe extern "C" fn(usize, usize, usize, usize, i32, usize) -> usize;
type Text = unsafe extern "thiscall" fn(usize, *const u16) -> u8;
type Animation = unsafe extern "thiscall" fn(usize, *const u8);
thread_local! { static STATE: RefCell<Runtime> = RefCell::new(Runtime::default()); }

#[derive(Clone, Copy)]
struct Local {
    binding: Binding,
    now: u64,
    alive: bool,
    goggles: bool,
    marked: Option<bool>,
    merc: bool,
}
unsafe fn ptr(p: usize, offset: usize) -> Option<usize> {
    word(p, offset)
        .filter(|v| *v >= 0x10000)
        .map(|v| v as usize)
}
unsafe fn local(hud: usize, info: usize, goggles: Goggles) -> Option<Local> {
    if word(info, 0x330)? & 0x10 == 0 {
        return None; // multiplayer only; never infer from a campaign pawn
    }
    let world = ptr(info, 0x144)?;
    let pc = crate::local_player::controller(world)?;
    let pawn = ptr(pc, 0x520)?;
    let role = (word(pawn, 0)?, word(pawn, 0x374)? & 0xc0000);
    if !matches!(role, (SPY, 0x40000) | (MERC, 0x80000)) || word(pawn, 0x144)? as usize != world {
        return None;
    }
    let generation = word(pawn, 0x20)? as u64;
    let binding = Binding::resolve(hud, world, pawn, generation)?;
    let seconds = float(world, 0x5a0)? as f64;
    if !seconds.is_finite() || seconds < 0. || seconds * 1000. >= u64::MAX as f64 {
        return None;
    }
    let alive = word(pawn, 0x60)? & (1 << 11) == 0
        && word(pawn, 0x4b0)? & 0x80000000 == 0
        && word(pawn, 0xc50)? == 0;
    // Exact BA43C0 predicate used by native HUD tick. Only warning priority,
    // never evidence of detection by a goggle.
    let marked = word(pawn, 0x1a08).map(|f| f & 0x800 != 0);
    // D75D30 queries the equipped tactical item on MP and its active bit;
    // replicated peer+501 can lag and is intentionally not used.
    let merc = role.0 == MERC;
    let on = !merc && alive && ptr(pawn, 0x1410).is_some() && goggles(pawn) != 0;
    Some(Local {
        binding,
        now: (seconds * 1000.).round() as u64,
        alive,
        goggles: on,
        marked,
        merc,
    })
}
fn language_bytes(bytes: &[u8]) -> Option<Language> {
    match bytes {
        b"int\0" => Some(Language::English),
        b"cht\0" => Some(Language::TraditionalChinese),
        _ => None,
    }
}
unsafe fn language() -> Option<Language> {
    let p = 0x0337ce38;
    if !readable(p, 4) {
        return None;
    }
    language_bytes(std::slice::from_raw_parts(p as *const u8, 4))
}
unsafe fn wide_copy(p: usize) -> Option<Vec<u16>> {
    let mut out = Vec::new();
    for i in 0..256 {
        let a = p.checked_add(i * 2)?;
        if !readable(a, 2) {
            return None;
        }
        let n = (a as *const u16).read_unaligned();
        out.push(n);
        if n == 0 {
            return (out.len() > 1).then_some(out);
        }
    }
    None
}
// Exact victim warning branch B5EB5E: Pawn+1AA4, vtable+5DC, six stack
// arguments (cue, 7, 0.0, 0, 0, 0), ret 18h. Fifth argument disables
// replication. Never call AddMark, scoring, or a detector to make UI noise.
type PlayWarning = unsafe extern "thiscall" fn(usize, usize, u32, f32, usize, u32, u32) -> u32;
unsafe fn warning_sound(b: Binding, play: PlayWarning) -> bool {
    let pawn = b.life.1;
    if word(pawn, 0) != Some(SPY)
        || word(pawn, 0x144) != Some(b.life.0 as u32)
        || word(pawn, 0x20) != Some(b.life.2 as u32)
    {
        return false;
    }
    let Some(cue) = ptr(pawn, 0x1aa4).filter(|p| readable(*p, 0x44)) else {
        return false;
    };
    // C71DE0 dereferences this chain before its own null check.
    if ptr(pawn, 0x148)
        .and_then(|p| ptr(p, 0x38))
        .and_then(|p| ptr(p, 0x48))
        .is_none()
    {
        return false;
    }
    play(pawn, cue, 7, 0., 0, 0, 0) != 0
}
struct NativeUi {
    colors: ColorApi,
    transform: TransformApi,
}
impl NativeUi {
    unsafe fn new() -> Self {
        Self {
            transform: TransformApi {
                get: std::mem::transmute(0x02417420usize),
                set: std::mem::transmute(0x02417480usize),
            },
            colors: ColorApi {
                get: std::mem::transmute(0x0241c2d0usize),
                release: std::mem::transmute(0x0241ac60usize),
                set: std::mem::transmute(0x01862fd0usize),
            },
        }
    }
}
impl Ui for NativeUi {
    fn colors(&mut self, b: Binding) -> Option<[u32; 2]> {
        unsafe { Some([self.colors.read(b.texts[0])?, self.colors.read(b.texts[1])?]) }
    }
    fn text(&mut self, w: usize, s: &[u16]) -> bool {
        unsafe {
            std::mem::transmute::<usize, Text>(0x018bf320)(w, s.as_ptr());
            self.colors
                .force_text(w, s, std::mem::transmute(0x02418c60usize))
        }
    }
    fn color(&mut self, w: usize, color: u32) {
        unsafe {
            self.colors.write(w, color);
        }
    }
    fn transform(&mut self, w: usize) -> Option<Transform> {
        unsafe { self.transform.read(w) }
    }
    fn tint_rgb(&mut self, w: usize, rgb: Transform) -> bool {
        unsafe { self.transform.rgb(w, rgb) }
    }
    fn sound(&mut self, b: Binding) {
        let played = unsafe { warning_sound(b, std::mem::transmute(0x00c71de0usize)) };
        tracing::info!(
            "SCBL_MOD_EXPOSURE_UI sound_once=true native_detected_cue=true played={played}"
        );
    }
    fn native_label(&mut self) -> Option<Vec<u16>> {
        unsafe {
            // Same six arguments as 1AA5082/1AA50B1, not a cached locale pointer.
            let p = std::mem::transmute::<usize, Localize>(0x004b39c0)(
                0x02a06ac4, 0x02c44af4, 0x02a06ae4, 0, 0, 0,
            );
            wide_copy(p)
        }
    }
    fn animation(&mut self, w: usize, intro: bool) {
        unsafe {
            // Exact one-stack-argument vcall at 1A69002 (block 1A68FE4).
            // Ignore return just as retail does; do not write HUD+308.
            let Some(f) = ptr(w, 0).and_then(|v| ptr(v, 0x12c)) else {
                return;
            };
            if !(0x00401000..0x02900000).contains(&f) {
                return;
            }
            let name = if intro { b"IntroTag\0" } else { b"OutroTag\0" };
            std::mem::transmute::<usize, Animation>(f)(w, name.as_ptr());
        }
    }
}
#[derive(Default)]
struct Runtime {
    warning: Warning,
    interference: crate::rfd_interference::Warning,
    ui: WarningUi,
    last_presentation: Option<Presentation>,
    last_local_valid: Option<bool>,
    last_applied: Option<bool>,
}
impl Runtime {
    fn after_native(
        &mut self,
        local: Option<Local>,
        lang: Option<Language>,
        detections: ActualDetections,
        ui: &mut impl Ui,
    ) -> bool {
        let valid = local.is_some();
        if self.last_local_valid != Some(valid) {
            tracing::info!("SCBL_MOD_EXPOSURE_UI local_hud_valid={valid}");
            self.last_local_valid = Some(valid);
            self.last_presentation = None;
            self.last_applied = None;
        }
        let Some(l) = local else {
            self.warning = Warning::default();
            self.interference = crate::rfd_interference::Warning::default();
            return self
                .ui
                .apply(None, Presentation::Native, Language::English, ui);
        };
        let language = lang.unwrap_or(Language::English);
        let p = if l.merc {
            self.warning = Warning::default();
            self.interference.update(
                l.binding.life,
                l.now,
                l.alive && lang.is_some(),
                l.marked,
                crate::rfd_interference::receipt(l.binding.life, l.now),
            )
        } else {
            self.interference = crate::rfd_interference::Warning::default();
            self.warning.update(Frame {
                life: Life {
                    scene: l.binding.life.0 as u64,
                    spawn: (l.binding.life.1 as u64) << 32 | l.binding.life.2,
                },
                now_ms: l.now,
                dx11_mod: lang.is_some(),
                local_alive_spy: l.alive,
                tactical_goggles_on: l.goggles,
                native_marked: l.marked,
                detections,
            })
        };
        let kind = if l.merc {
            WarningKind::Interference
        } else {
            WarningKind::Exposed
        };
        let applied = self.ui.apply_kind(Some(l.binding), p, language, kind, ui);
        if self.last_presentation != Some(p) || self.last_applied != Some(applied) {
            tracing::info!("SCBL_MOD_EXPOSURE_UI kind={kind:?} state={p:?} applied={applied} alive={} goggles_on={} native_marked={:?} native_mark_priority=true", l.alive, l.goggles, l.marked);
            self.last_presentation = Some(p);
            self.last_applied = Some(applied);
        }
        applied
    }
}
fn original_then<R>(original: impl FnOnce(), after: impl FnOnce() -> R) -> R {
    original();
    after()
}
fn tick(hud: usize, dt: f32) {
    original_then(
        || unsafe {
            Tick.call(hud, dt);
        },
        || {
            if !READY.load(Ordering::Acquire) {
                return;
            }
            unsafe {
                // Same native HUD owner and goggles predicate; independent of text bindings.
                crate::overload_visual_policy::frame(ptr(0x0339e5a0, 0).unwrap_or(0));
            }
            STATE.with(|state| {
                let Ok(mut state) = state.try_borrow_mut() else {
                    return;
                };
                unsafe {
                    let l = ptr(0x0339e5a0, 0).and_then(|info| {
                        local(hud, info, std::mem::transmute::<usize, Goggles>(0x00d75d30))
                    });
                    let detections = l
                        .filter(|l| !l.merc)
                        .map(|l| {
                            crate::goggle_feedback_native::detections(
                                l.binding.life.0,
                                l.binding.life.1,
                                l.now,
                            )
                        })
                        .unwrap_or_default();
                    state.after_native(l, language(), detections, &mut NativeUi::new());
                }
            });
        },
    );
}
const CONTRACTS: &[(usize, usize, &str)] = &[
    (
        TICK,
        895,
        "60d6815d91ea07d0b2f1acd820bba0d57e91e5f173f891f8aecb28a8f7e32012",
    ),
    (
        0x00ba43c0,
        13,
        "dee6d1fcd904b722d5530925e51f64aaf6cded244ea2a3f99ee11d012204ab3b",
    ),
    (
        0x00d75d30,
        168,
        "7c40c25501d5f854bf9a8507a940651db8591cafe440d8d5e4c4344093613aed",
    ),
    (
        0x01a52310,
        71,
        "82b05f2280f757c010bcd55a2f4492f2f4e902f298ae5b71ee69d1d8f4e22316",
    ),
    (
        0x018bf320,
        60,
        "5125847850556767bc858280c4541dbf892a28ad36c6864e5fc29be2ed1495de",
    ),
    (
        0x01862fd0,
        23,
        "36ebbe709c41aa1580513a812b94bd445b5efecd72d1473e2a18d87a9223108c",
    ),
    (
        0x018611a0,
        310,
        "c6793f2699942bf77fcdde326a78a622fd9fcc2dd59060ad746acab7ee5576d0",
    ),
    (
        0x0241c2d0,
        347,
        "67e554688f751d8204ebbc07cf07355441649c9542bca2f79d95cb205a101511",
    ),
    (
        0x0241ac60,
        132,
        "d24ef3a06d0ddf9cd92c9332b814644d70e71626fd33882664774c7b32df1833",
    ),
    (
        0x004b39c0,
        1126,
        "3def92b51f841ede4b5d2069724f1f26ddecabf014b0675674d9bb383fa87e06",
    ),
    (
        0x02418c60,
        157,
        "e7649b44f204a5c7105d7b302eaf1f190b5194ed640493343bff6083d6e8d89f",
    ),
    (
        0x02417420,
        86,
        "5a024468ede23fd206fb3a05a3a897a5d839f730f4599dfba89f7e9f2c713749",
    ),
    (
        0x02417480,
        90,
        "8a3d5575d49c42bf24cc555b84cff84220e1873cd02f39a9ca95912ce9ef5230",
    ),
    (
        0x00c71de0,
        451,
        "8b44c9b9df6c432fe10a3ea41a551984fc67aceff8eaf98d5a711566840813fa",
    ),
];
pub(super) fn ready()->bool { READY.load(Ordering::Acquire) }
pub(crate) unsafe fn init(world_raycast: usize) {
    if READY.load(Ordering::Acquire) || world_raycast != 0x00589860 {
        return;
    }
    if !CONTRACTS.iter().all(|&(p, n, h)| signature(p, n, h))
        || word(SPY as usize, 0x704) != Some(0x00ba43c0)
        || word(MERC as usize, 0x704) != Some(0x00ba43c0)
    {
        return;
    }
    if Tick
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(TICK),
            tick,
        )
        .and_then(|h| h.enable())
        .is_err()
    {
        let _ = Tick.disable();
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!(
        "SCBL_MOD_EXPOSURE_UI private_adapter=true peer_delivery=validated_lease warning_enabled=receipt_only"
    );
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Tick.disable();
    // The host calls shutdown from process detach, not a live gameplay unload.
    // Never wait for a HUD tick or call GFx while it holds the loader lock.
    // TLS can already be destroyed here. A future live-unload API would need a
    // separate HUD-thread restoration protocol before disabling this hook.
    clear_thread_state();
}

fn clear_thread_state() -> bool {
    STATE
        .try_with(|s| {
            let Ok(mut state) = s.try_borrow_mut() else {
                return false;
            };
            *state = Runtime::default();
            true
        })
        .unwrap_or(false)
}
#[cfg(test)]
#[path = "goggle_warning_native_tests.rs"]
mod tests;
