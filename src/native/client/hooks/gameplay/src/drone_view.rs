//! C16: parked MP drones use their last controlled view, never the merc camera.
//! Retail B2A7C0 reads the GLOBAL local camera and pawn+BC0 visibility accumulated
//! by D9E970 from that camera. Neither is valid after leaving drone control.
//! 8BC900 -> 94D360 also rejects targets not rendered by that camera in the last
//! two frames. Preserve its hidden flag, but not its local render-recency gate.
//! Keep the native team/dead/range/immunity predicate and the entire B51FC0
//! acquire/linger/source/RPC path. Replace only its three camera-dependent gates.
use crate::equipment_native::{float, readable, signature, word};
use retour::static_detour;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        Mutex,
    },
};
const FILTER: usize = 0xb2a7c0;
const FILTER_SHA: &str = "0dd34351372a4647826d84d9921a9f8de7e40cdeca0ac5bd6857b7a21a4cc6d3";
const BONES: [u32; 8] = [2, 6, 20, 16, 8, 12, 27, 28]; // 2AE8ED4, D9ECD1
static READY: AtomicBool = AtomicBool::new(false);
static BANK: AtomicUsize = AtomicUsize::new(0);
static LOGS: AtomicU32 = AtomicU32::new(0);
static DIAG: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];
fn diag(slot: usize, event: &'static str) {
    if DIAG[slot]
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < 8).then_some(n + 1)
        })
        .is_ok()
    {
        tracing::info!("SCBL_MOD_DRONE_VIEW candidate=16 event={}", event);
    }
}
static VIEWS: Mutex<BTreeMap<usize, Saved>> = Mutex::new(BTreeMap::new());
static_detour! {
    static Candidate: unsafe extern "thiscall" fn(usize, usize) -> u8;
    static Station: unsafe extern "thiscall" fn(usize);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    owner: usize,
    ability: usize,
    controller: usize,
    source: [u32; 2],
}
#[derive(Clone, Copy, Debug)]
struct View {
    projection: [f32; 16],
    screen: [f32; 16],
    viewport: [f32; 2],
    origin: [f32; 3],
    body: [f32; 3],
}
struct Saved {
    id: Identity,
    view: View,
    samples: BTreeMap<usize, Samples>,
    frozen: bool,
}
#[derive(Default)]
struct Samples {
    source: [u32; 2],
    clear: [bool; 8],
    next: usize,
}
fn finite(v: &[f32]) -> bool {
    v.iter().all(|f| f.is_finite() && f.abs() < 1.0e9)
}
unsafe fn floats<const N: usize>(p: usize, offset: usize) -> Option<[f32; N]> {
    let mut v = [0.; N];
    for (i, f) in v.iter_mut().enumerate() {
        *f = float(p, offset + i * 4)?;
    }
    finite(&v).then_some(v)
}
unsafe fn source(p: usize) -> Option<[u32; 2]> {
    let node = word(p, 0x2c0)? as usize;
    let id = [word(node, 0x70)?, word(node, 0x74)?];
    (id != [0, 0]).then_some(id)
}
unsafe fn identity(body: usize) -> Option<Identity> {
    if word(body, 0)? != 0x02a88f54
        || !matches!(word(body, 0x910)? & 255, 1 | 2)
        || word(body, 0x4b0)? & 0x80000000 != 0
        || word(body, 0x5dc)? as i32 <= 0
    {
        return None;
    }
    let owner = word(body, 0x9fc)? as usize;
    if word(body, 0x8c)? as usize != owner
        || word(owner, 0)? != 0x02ad2abc
        || word(owner, 0x60)? & 0x800 != 0
    {
        return None;
    }
    let controller = word(owner, 0x938)? as usize;
    if controller == 0 || word(controller, 0x4c)? >> 8 & 255 == 2 {
        return None;
    }
    let armor = word(owner, 0xcf4)? as usize;
    if word(armor, 0)? != 0x02ad42ac {
        return None;
    }
    let ability = word(armor, 0x5c)? as usize;
    let charge = float(ability, 0x7c)?;
    if word(ability, 0)? != 0x02a3f14c
        || word(ability, 0xa0)? as usize != body
        || word(ability, 0x6c)? as usize != owner
        || word(ability, 0x40)? & 0x2e != 8
        || !charge.is_finite()
        || !(0.0..=1.0).contains(&charge)
        || charge == 0.0
    {
        return None;
    }
    Some(Identity {
        owner,
        ability,
        controller,
        source: source(body)?,
    })
}
unsafe fn capture(body: usize, id: Identity) -> Option<View> {
    let camera = word(id.controller, 0x6a4)? as usize;
    let proj = word(camera, 0x7e8)? as usize;
    // No calls into a live renderer, and no retained camera / viewport pointers.
    if word(word(proj, 0)? as usize, 8)? != 0x563770 {
        return None;
    }
    let core = word(camera, 0x61c)? as usize;
    let screen = word(camera, 0x7ec)? as usize;
    let viewport = match word(core, 0x1434)? {
        0 => word(word(core, 0x20)? as usize, 0x354)? as usize,
        p => p as usize,
    };
    let size = [word(viewport, 0x4a0)? as f32, word(viewport, 0x4a4)? as f32];
    if size.iter().any(|x| !(16.0..=16384.0).contains(x)) {
        return None;
    }
    let view = View {
        projection: floats(proj, 0x160)?,
        screen: floats(screen, 0x20)?,
        viewport: size,
        origin: floats(camera, 0x9c)?,
        body: floats(body, 0x9c)?,
    };
    // Reject a still-transitioning merc camera instead of parking the wrong view.
    let d: f32 = (0..3)
        .map(|i| (view.origin[i] - view.body[i]).powi(2))
        .sum();
    (d <= 500.0 * 500.0 && view.projection.iter().any(|x| *x != 0.0)).then_some(view)
}
fn transform(m: &[f32; 16], p: [f32; 3], i: usize) -> f32 {
    // Match 563770 / D4CCE0's double intermediate, then single storage.
    ((m[4 + i] as f64 * p[1] as f64 + m[i] as f64 * p[0] as f64)
        + m[8 + i] as f64 * p[2] as f64
        + m[12 + i] as f64) as f32
}
impl View {
    fn contains(&self, point: [f32; 3], current_body: [f32; 3]) -> bool {
        if !finite(&point) || !finite(&current_body) {
            return false;
        }
        let p = std::array::from_fn(|i| point[i] - current_body[i] + self.body[i]);
        let w = transform(&self.projection, p, 3);
        if !w.is_finite() || w <= 0.0001 {
            return false;
        } // no behind-camera wrap
        let inv = (1.0f64 / w as f64) as f32;
        let clip =
            std::array::from_fn(|i| (transform(&self.projection, p, i) as f64 * inv as f64) as f32);
        let x = transform(&self.screen, clip, 0);
        let y = transform(&self.screen, clip, 1);
        // Exact native rectangle: 8% horizontal, 10% vertical inset.
        x.is_finite()
            && y.is_finite()
            && x >= self.viewport[0] * 0.08
            && x <= self.viewport[0] * 0.92
            && y >= self.viewport[1] * 0.1
            && y <= self.viewport[1] * 0.9
    }
    fn origin_at(&self, body: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|i| self.origin[i] + body[i] - self.body[i])
    }
}
pub(super) unsafe fn observe(body: usize) {
    if !ready() {
        return;
    }
    let Some(id) = identity(body) else {
        forget(body);
        return;
    };
    if word(body, 0x910).map(|s| s & 255) != Some(2) {
        return;
    }
    {
        let mut map = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
        if map.get(&body).is_some_and(|s| s.id != id) {
            map.remove(&body);
        }
        if map.get(&body).is_some_and(|s| s.frozen) {
            return;
        }
    }
    let Some(view) = capture(body, id) else {
        // A transient camera rebuild must not erase the last valid drone view.
        diag(0, "capture_unavailable_keep_last");
        return;
    };
    let mut map = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
    if !map.contains_key(&body) && map.len() >= 8 {
        return;
    }
    // Controlled view is refreshed; occlusion samples must not cross reentry.
    map.insert(
        body,
        Saved {
            id,
            view,
            samples: BTreeMap::new(),
            frozen: false,
        },
    );
}
pub(super) unsafe fn freeze(body: usize) {
    let Some(id) = identity(body) else {
        forget(body);
        return;
    };
    observe(body);
    let mut map = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(saved) = map.get_mut(&body).filter(|s| s.id == id) {
        saved.frozen = true;
        diag(1, "view_frozen_before_transout");
    } else {
        diag(2, "freeze_missing_saved_view");
    }
}
// AF5F60 is also called by native gear deactivation, not just drone destruction.
// Keep the view of the same living parked body; discard only its ray samples.
pub(super) unsafe fn clear_tracking(body: usize) {
    let id = identity(body);
    let mut map = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
    match map.get_mut(&body) {
        Some(saved) if Some(saved.id) == id => {
            saved.samples.clear();
            diag(3, "tracking_cleared_live_view_retained");
        }
        _ => {
            map.remove(&body);
        }
    }
}
pub(super) fn forget(body: usize) {
    VIEWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&body);
}
fn station(body: usize) {
    unsafe {
        // OnStation can run after the camera was already returned to the merc.
        // Do not recapture it here: TransOut froze the last controlled frame.
        Station.call(body);
    }
}
fn candidate(body: usize, target: usize) -> u8 {
    unsafe {
        let mark_state = if ready() && word(body, 0) == Some(0x02a88f54) {
            word(body, 0x910).map(|s| s & 255)
        } else {
            None
        };
        if let Some(limit_cm) = mark_state.and_then(mark_limit_cm) {
            if !within_mark_range(body, target, limit_cm) {
                return 0;
            }
        }
        if mark_state == Some(1) && ready() {
            let f: unsafe extern "thiscall" fn(usize, usize) -> u8 =
                std::mem::transmute(BANK.load(Ordering::Acquire));
            return f(body, target);
        }
        Candidate.call(body, target)
    }
}
fn mark_limit_cm(state: u32) -> Option<f64> {
    match state {
        1 => Some(3_000.0), // parked after native TransOut
        2 => Some(1_000.0), // actively controlled; keep the original MOD cap
        _ => None,          // native handles unknown, retired and transitional states
    }
}
unsafe fn within_mark_range(body: usize, target: usize, limit_cm: f64) -> bool {
    let (Some(a), Some(b)) = (floats::<3>(body, 0x9c), floats::<3>(target, 0x9c)) else {
        return false;
    };
    // Centimetre world coordinates, measured from this drone body in 3D.
    (0..3)
        .map(|i| (a[i] as f64 - b[i] as f64).powi(2))
        .sum::<f64>()
        <= limit_cm * limit_cm
}
#[repr(C, align(16))]
struct Hit([u8; 0x48]);
type Ray = unsafe extern "thiscall" fn(
    usize,
    *mut u8,
    usize,
    *const [f32; 3],
    *const [f32; 3],
    usize,
    usize,
    *const [f32; 3],
) -> u8;
unsafe fn ray_visible(body: usize, target: usize, bone: u32, origin: [f32; 3]) -> Option<bool> {
    let mesh = word(target, 0x1f0)? as usize;
    let skeleton = word(mesh, 0xc0)? as usize;
    let model = word(skeleton, 0x28)? as usize;
    if !readable(model, 0x1c) || !readable(skeleton, 0xafc) {
        return None;
    }
    let world = word(body, 0x148)? as usize;
    if world == 0 || word(target, 0x148)? as usize != world || word(world, 0x28)? == 0 {
        return None;
    }
    let get: unsafe extern "thiscall" fn(usize, *mut [f32; 3], u32) -> *const [f32; 3] =
        std::mem::transmute(0x96b830usize);
    let mut point = [f32::NAN; 3];
    get(mesh, &mut point, bone);
    if !finite(&point) || !finite(&origin) {
        return None;
    }
    let query: Ray = std::mem::transmute(0x5abc80usize);
    let mut hit = Hit([0; 0x48]);
    // Same VISIBILITY mask/material as D9ED0C, not a bullet/damage query.
    // Ignore the drone hierarchy; reaching the intended pawn is not a wall.
    let clear = query(
        world,
        hit.0.as_mut_ptr(),
        body,
        &point,
        &origin,
        0x300400,
        0x2ad9e8c,
        &[0.; 3],
    );
    Some(clear != 0 || u32::from_le_bytes(hit.0[4..8].try_into().ok()?) as usize == target)
}
unsafe extern "thiscall" fn sight(body: usize, target: usize) -> u8 {
    sight_with(body, target, |bone, origin| {
        ray_visible(body, target, bone, origin)
    })
}
unsafe fn sight_with(
    body: usize,
    target: usize,
    ray: impl FnOnce(u32, [f32; 3]) -> Option<bool>,
) -> u8 {
    let Some(id) = identity(body) else {
        forget(body);
        return 0;
    };
    if word(target, 0) != Some(0x02ad3234) {
        return 0;
    }
    let (Some(point), Some(pos), Some(target_source), Some(threshold)) = (
        floats(target, 0x9c),
        floats(body, 0x9c),
        source(target),
        float(target, 0xbc4),
    ) else {
        return 0;
    };
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return 0;
    }
    let (view, index) = {
        let mut map = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(saved) = map.get_mut(&body).filter(|s| s.id == id) else {
            diag(4, "parked_reject_missing_view");
            return 0;
        };
        if !saved.view.contains(point, pos) {
            saved.samples.remove(&target);
            return 0;
        }
        if !saved.samples.contains_key(&target) && saved.samples.len() >= 32 {
            return 0;
        }
        let samples = saved.samples.entry(target).or_default();
        if samples.source != target_source {
            *samples = Samples {
                source: target_source,
                ..Default::default()
            };
        }
        (saved.view, samples.next)
    };
    let visible = ray(BONES[index], view.origin_at(pos)).unwrap_or(false);
    let mut map = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(saved) = map.get_mut(&body).filter(|s| s.id == id) else {
        return 0;
    };
    let Some(samples) = saved
        .samples
        .get_mut(&target)
        .filter(|s| s.source == target_source)
    else {
        return 0;
    };
    samples.clear[index] = visible;
    samples.next = (index + 1) % 8;
    let fraction = samples.clear.iter().filter(|v| **v).count() as f32 * 0.125;
    let result = fraction >= threshold;
    if LOGS.fetch_add(1, Ordering::Relaxed) < 24 {
        tracing::info!("SCBL_MOD_DRONE_VIEW parked=true camera=drone sample={} fraction={} threshold={} eligible={}",index,fraction,threshold,result);
    }
    result as u8
}
// Byte-for-byte native predicate except camera-dependent gates. No world code
// is patched in place beyond the entry detour; native controlled scans unchanged.
fn window(raw: &[u8], at: usize, bridge: usize) -> Option<Vec<u8>> {
    if raw.len() != 620
        || format!("{:x}", Sha256::digest(raw)) != FILTER_SHA
        || at == 0
        || bridge == 0
        || at.checked_add(620)? > u32::MAX as usize
        || bridge > u32::MAX as usize
    {
        return None;
    }
    let mut out = raw.to_vec();
    for (o, dest) in [
        (0x24, 0x8b8370usize),
        (0x1fc, 0xad85c0),
        (0x21d, 0xc94fc0),
        (0x228, 0xc94fc0),
        (0x22f, 0x8c4ed0),
        (0x241, 0x52e7f0),
    ] {
        if out[o] != 0xe8 {
            return None;
        }
        let old = i32::from_le_bytes(out[o + 1..o + 5].try_into().ok()?);
        if (FILTER + o + 5).wrapping_add_signed(old as isize) != dest {
            return None;
        }
        out[o + 1..o + 5].copy_from_slice(
            &(dest as u32)
                .wrapping_sub((at + o + 5) as u32)
                .to_le_bytes(),
        );
    }
    // C16: the old clone retained 8BC900, which additionally checks 94D360's
    // LOCAL render timestamp. After switching back to the merc, it rejected
    // a spy in the parked drone's frustum after two unrendered frames.
    // Keep 8BC900's target+5C hidden flag; sight_with/ray_visible validate the
    // target, current drone view and mesh instead. Do not forge frame stamps.
    // TEST byte [ebx+5C],1; JNZ native early-return +11; then fall through.
    out[0x3d..0x48].fill(0x90);
    out[0x3d..0x43].copy_from_slice(&[0xf6, 0x43, 0x5c, 0x01, 0x75, 0xce]);
    out[0x48..0x64].fill(0x90); // player's rolling visibility is not drone visibility
    out[0xf5..0x1f9].fill(0x90); // retain PUSH EDI at +F4 and native immunity at +1F9
    let mut gate = vec![0xff, 0x75, 8, 0x8b, 0xce, 0xe8]; // push target; mov ecx,esi; call sight
    gate.extend(
        (bridge as u32)
            .wrapping_sub((at + 0xf5 + 10) as u32)
            .to_le_bytes(),
    );
    gate.extend([0x84, 0xc0, 0x0f, 0x84]);
    gate.extend(((0x256i32) - (0xf5 + 18) as i32).to_le_bytes());
    gate.push(0xe9);
    gate.extend(((0x1f9i32) - (0xf5 + 23) as i32).to_le_bytes());
    out[0xf5..0xf5 + gate.len()].copy_from_slice(&gate);
    Some(out)
}
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, protect: u32) -> *mut c_void;
    fn VirtualProtect(p: *mut c_void, n: usize, protect: u32, old: *mut u32) -> i32;
    fn VirtualFree(p: *mut c_void, n: usize, t: u32) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
}
pub(super) fn ready() -> bool {
    READY.load(Ordering::Acquire)
}
pub(super) unsafe fn init() {
    if ready() {
        return;
    }
    for (p, n, s) in [
        (FILTER, 620, FILTER_SHA),
        (
            0x8bc900,
            30,
            "e9ffd7b9f69455d6e787d4908884f3505e09d4ef6924854ea542580b13f448cb",
        ),
        (
            0x94d360,
            43,
            "c2d3efa5099454671547223535d962118438334ba0951a2b63acbb60c90d187c",
        ),
        (
            0xe68b30,
            123,
            "f502ea7b9ec5932cc7cda7ac1c2f1add95c6c772a527f08f507ed734500c52ce",
        ),
        (
            0x563770,
            498,
            "0b672d3f0ad19cd7652e1f2e53371f7831c5f2d45e03e9c4680e1fde23930b7e",
        ),
        (
            0x5abc80,
            272,
            "4bd10b6d5e63bfc577a32f7678a3a00bbe23ea44fdfa69811ccb3d4f1397aff0",
        ),
        (
            0x96b830,
            167,
            "efd58e6dd206219147128bf689eb340b8bdaba5df173bd7d30bc34b578643d95",
        ),
    ] {
        if !signature(p, n, s) {
            tracing::error!("SCBL_MOD_DRONE_VIEW enabled=false reason=signature");
            return;
        }
    }
    let bank = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4) as usize;
    if bank == 0 {
        return;
    }
    let raw = std::slice::from_raw_parts(FILTER as *const u8, 620);
    let Some(code) = window(raw, bank, sight as *const () as usize) else {
        VirtualFree(bank as _, 0, 0x8000);
        return;
    };
    std::ptr::copy_nonoverlapping(code.as_ptr(), bank as *mut u8, code.len());
    let mut old = 0;
    if VirtualProtect(bank as _, 4096, 0x20, &mut old) == 0
        || FlushInstructionCache(GetCurrentProcess(), bank as _, 4096) == 0
    {
        VirtualFree(bank as _, 0, 0x8000);
        return;
    }
    BANK.store(bank, Ordering::Release);
    if Candidate
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, usize) -> u8>(FILTER),
            candidate,
        )
        .is_err()
        || Station
            .initialize(
                std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize)>(0xe68b30),
                station,
            )
            .is_err()
        || Station.enable().is_err()
        || Candidate.enable().is_err()
    {
        let _ = Candidate.disable();
        let _ = Station.disable();
        BANK.store(0, Ordering::Release);
        VirtualFree(bank as _, 0, 0x8000);
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_DRONE_VIEW enabled=true revision=58 candidate=16 parked_camera=last_drone_view visibility=drone_native_bones local_render_recency=ignored_parked_only controlled=native tags_rpc=native");
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Candidate.disable();
    let _ = Station.disable();
    VIEWS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    // Entry hooks disabled first; code stays allocated to avoid an in-flight free.
}
#[cfg(test)]
#[path = "drone_view_tests.rs"]
mod tests;
