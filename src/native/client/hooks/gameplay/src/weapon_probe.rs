use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::sync::OnceLock;

use retour::static_detour;
use tracing::error;
use tracing::info;

// Independent input rules; accepted obstruction and native reticle decisions
// remain unchanged. Keeping the module separate avoids another mixed policy.
#[path = "combat_input.rs"]
mod combat_input;
#[path = "spy_slide_cover.rs"]
mod spy_slide_cover;

// Independent enemy name/health policy; accepted weapon and movement logic unchanged.
#[path = "enemy_identification.rs"]
pub(crate) mod enemy_identification;

// R58: keep the verified implementation dormant; never install its world,
// aiming or damage-dispatch hooks. Native dot/identification/input stay active.
const OBSTRUCTION_FIX_ENABLED: bool = false;

const DX11_WEAPON_TRACE_DISPATCH: usize = 0x00E2_B940;
// Refreshes the current weapon attachment position and returns weapon_state+0x220.
// The retail routine resolves the configured weapon bone through 0x006B5D20,
// then applies the weapon-specific muzzle offset. The same position is used by
// the projectile and firing-effect paths.
const DX11_WEAPON_MUZZLE_POSITION: usize = 0x00DF_FDA0;
// Retail DX11 only. D81180 takes ten pointer-sized arguments and returns
// with ret 0x28. Its world query at D81670 supplies (end,start).
const DX11_AIM_RAYCASTS: usize = 0x00D8_1180;
// HudReticles::select: thiscall(hud, kind), ret 4. Slot 10 is the existing
// Unavailable_white resource, constructed at 1AA4AF1..1AA4B85.
const DX11_RETICLE_SELECT: usize = 0x01A5_AA40;
const DX11_CURRENT_WEAPON: usize = 0x00D6_E610;
const UNAVAILABLE_RETICLE: i32 = 10;
const DOT_RETICLE: u32 = 3;
const DX11_PLAYER_PAWN_CLASS: usize = 0x0339_E7C8;
// Proven retail HUD ABI: update(this, float) ret4; visibility(this, u8) ret4.
// Dot resource is hud+2b0 (kind3), Dot_white is color1. Scope the visibility
// override to the native update stack, never an arbitrary menu Flash object.
const DX11_RETICLE_UPDATE: usize = 0x01A6_8550;
const DX11_RETICLE_VISIBILITY: usize = 0x01A4_16C0;
const DX11_RETICLE_COLOR: usize = 0x01A5_1A60;
// HudReticles::update calls D683E0 at 1A68899 before resolving the selected
// component, its color/position and visibility. thiscall(this)->u8, ret (no
// arguments). Observe its original result; never change the gameplay result.
const DX11_NATIVE_RETICLE_VISIBILITY: usize = 0x00D6_83E0;
const AIM_MAX_AGE_MS: u64 = 100;
// Synchronous, filtered world query used throughout the retail game. Unlike the
// lower-level world raycast, this wrapper removes the supplied actor hierarchy
// from the hit list and restores the engine's temporary query context.
const DX11_FILTERED_RAYCAST: usize = 0x005A_BC80;
// The weapon trace uses the sibling submit wrapper at 0x009243A0. That
// wrapper adds 0x04000000 to the caller's 0x09400004 mask, producing the
// 0x0D400004 value observed later on the deferred raycast worker thread.
// Kept ONLY for bounded diagnostic comparison with r9, never for a gameplay
// veto. Scenery and bullet collision are not interchangeable at openings.
const LEGACY_SCENERY_QUERY_MASK: usize = 0x0030_0400;
const OBSTRUCTION_QUERY_MATERIAL: usize = 0x029A_2AD0;
// Native synchronous bullet query at E51800..E51843: actual trace endpoints,
// mask 08400004, material 29A2AD0, extent (state+16c, 0, state+16c).
// This is a read-only hit prediction, not a call to the damage routine. The
// deferred worker may still differ in timing; real-game validation is required.
const SHOT_QUERY_MASK: usize = 0x0840_0004;
const DX11_ZERO_VECTOR: usize = 0x0337_F25C;
const QUERY_CONTEXT_MAX_AGE_MS: u64 = 2_500;
const MAX_QUERY_CONTEXTS: usize = 64;
// Explicit development build opt-in; ordinary builds remain silent. No worker,
// extra native hook, live pointer logging, or DLL hot-unload is introduced.
const MAX_DIAGNOSTIC_RECORDS: u32 = 512;
static DIAGNOSTIC_RECORDS: AtomicU32 = AtomicU32::new(0);
// Retail UESEQJobMPIsPawnSpy / UESEQJobMPIsPawnMerc read these exact bits
// from the multiplayer pawn gameplay flags at +0x374.
const PAWN_GAMEPLAY_FLAGS_OFFSET: usize = 0x374;
const PAWN_IS_SPY: u32 = 1 << 18;
const PAWN_IS_MERC: u32 = 1 << 19;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct FVector {
    x: f32,
    y: f32,
    z: f32,
}

#[derive(Clone, Copy, Debug)]
struct QueryContext {
    timestamp_ms: u64,
    query_owner: usize,
    query_filter: usize,
}

#[derive(Clone, Copy, Debug)]
struct ObstructionQueryResult {
    blocked: bool,
    hit_position: Option<FVector>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContextStatus {
    NotApplicable,
    Missing,
    Expired,
    Unavailable,
    Ready,
}

#[derive(Clone, Copy, Debug)]
struct ContextSnapshot {
    status: ContextStatus,
    age_ms: Option<u64>,
    ready: Option<(QueryContext, u64)>,
}

impl ContextSnapshot {
    fn absent(status: ContextStatus) -> Self {
        Self {
            status,
            age_ms: None,
            ready: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PassthroughReason {
    PawnUnavailable,
    ActorMismatch,
    NotSpy,
    ContextMissing,
    ContextExpired,
    ContextUnavailable,
    InvalidQuery,
    UnconfirmedLocalAim,
    NoShotMercHit,
    BothRaysBlocked,
    CameraBlocked,
    MuzzleClear,
}

fn passthrough_reason(
    pawn_present: bool,
    actor_matches: bool,
    spy: bool,
    context: ContextStatus,
    camera: Option<bool>,
    muzzle: Option<bool>,
) -> PassthroughReason {
    if !pawn_present {
        return PassthroughReason::PawnUnavailable;
    }
    if !actor_matches {
        return PassthroughReason::ActorMismatch;
    }
    if !spy {
        return PassthroughReason::NotSpy;
    }
    match context {
        ContextStatus::Missing => return PassthroughReason::ContextMissing,
        ContextStatus::Expired => return PassthroughReason::ContextExpired,
        ContextStatus::Unavailable => return PassthroughReason::ContextUnavailable,
        ContextStatus::NotApplicable => return PassthroughReason::InvalidQuery,
        ContextStatus::Ready => {}
    }
    match (camera, muzzle) {
        (Some(true), Some(true)) => PassthroughReason::BothRaysBlocked,
        (Some(true), Some(false)) => PassthroughReason::CameraBlocked,
        (Some(false), Some(false)) => PassthroughReason::MuzzleClear,
        _ => PassthroughReason::InvalidQuery,
    }
}

#[derive(Clone, Copy, Debug)]
enum TraceAction {
    NullTrace,
    Passthrough(PassthroughReason),
    SuppressedObstruction,
}

#[derive(Debug)]
struct TraceEvidence {
    context: ContextStatus,
    context_age_ms: Option<u64>,
    camera_blocked: Option<bool>,
    muzzle_blocked: Option<bool>,
    camera_ray_length: Option<f32>,
    muzzle_ray_length: Option<f32>,
    camera_hit_distance: Option<f32>,
    muzzle_hit_distance: Option<f32>,
    legacy_muzzle_blocked: Option<bool>,
    legacy_muzzle_hit_distance: Option<f32>,
    confirmed_target_distance: Option<f32>,
    local_aim_confirmed: bool,
    center_aim_has_merc: bool,
    shot_extent: Option<f32>,
    sampled_left_button_down: bool,
}

fn diagnostic_opt_in(value: Option<&str>) -> bool {
    value == Some("1")
}

fn diagnostics_enabled() -> bool {
    diagnostic_opt_in(option_env!("SCBL_WEAPON_DIAGNOSTICS"))
}

fn take_diagnostic_slot(counter: &AtomicU32, limit: u32) -> Option<u32> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < limit).then(|| n + 1)
        })
        .ok()
        .map(|n| n + 1)
}

fn finite_distance(origin: Option<FVector>, target: Option<FVector>) -> Option<f32> {
    let distance = distance_squared(origin?, target?).sqrt();
    distance.is_finite().then_some(distance)
}

fn emit_trace(action: TraceAction, evidence: Option<TraceEvidence>) {
    if !diagnostics_enabled() {
        return;
    }
    let Some(sequence) = take_diagnostic_slot(&DIAGNOSTIC_RECORDS, MAX_DIAGNOSTIC_RECORDS) else {
        return;
    };
    let reason = match action {
        TraceAction::Passthrough(reason) => Some(reason),
        _ => None,
    };
    // All measurements are scalar distances in engine units, not coordinates,
    // player identities, raw addresses, or engine objects. Evidence describes
    // this dispatch only; unobserved native firing branches remain unproven.
    if let Some(evidence) = evidence {
        info!("SCBL_WEAPON_OBSTRUCTION_TRACE sequence={sequence} action={action:?} reason={reason:?} context={:?} context_age_ms={:?} camera_blocked={:?} muzzle_blocked={:?} camera_ray_length={:?} muzzle_ray_length={:?} camera_hit_distance={:?} muzzle_hit_distance={:?} confirmed_target_distance={:?} local_aim_confirmed={} center_aim_has_merc={} shot_extent={:?} target_source=native-shot-probe muzzle_profile=native-bullet legacy_muzzle_blocked={:?} legacy_muzzle_hit_distance={:?} sampled_left_button_down={}", evidence.context, evidence.context_age_ms, evidence.camera_blocked, evidence.muzzle_blocked, evidence.camera_ray_length, evidence.muzzle_ray_length, evidence.camera_hit_distance, evidence.muzzle_hit_distance, evidence.confirmed_target_distance, evidence.local_aim_confirmed, evidence.center_aim_has_merc, evidence.shot_extent, evidence.legacy_muzzle_blocked, evidence.legacy_muzzle_hit_distance, evidence.sampled_left_button_down);
    } else {
        info!("SCBL_WEAPON_OBSTRUCTION_TRACE sequence={sequence} action={action:?} reason={reason:?} evidence=unavailable");
    }
    if sequence == MAX_DIAGNOSTIC_RECORDS {
        info!("SCBL_WEAPON_OBSTRUCTION_TRACE exhausted=true limit={MAX_DIAGNOSTIC_RECORDS} gameplay_unchanged=true");
    }
}

static QUERY_CONTEXTS: OnceLock<Mutex<HashMap<usize, QueryContext>>> = OnceLock::new();

#[derive(Clone, Copy)]
struct AimSample {
    pawn: usize,
    weapon: usize,
    context: QueryContext,
    origin: FVector,
    target: FVector,
    merc_target: Option<FVector>,
}

#[derive(Clone, Copy)]
struct ReticleFrame {
    hud: usize,
    aiming: bool,
    foreground: bool,
    pawn: usize,
    requested: Option<i32>,
    selected: i32,
    fallback_dot: bool,
    mod_x: bool,
}

thread_local! {
    // Thread-local snapshots: never query the scene on a different thread or
    // retain native hit-list pointers. The native aim routine must finish
    // consuming its scratch list before our HUD-only query is allowed.
    static AIM_CAPTURE: Cell<Option<Option<AimSample>>> = const { Cell::new(None) };
    static AIM_SAMPLES: RefCell<HashMap<usize, AimSample>> = RefCell::new(HashMap::new());
    static ACTIVE_RETICLE_FRAME: Cell<ReticleFrame> = const { Cell::new(ReticleFrame {
        hud: 0, aiming: false, foreground: false, pawn: 0, requested: None,
        selected: -1, fallback_dot: false, mod_x: false,
    }) };
    // Identity only, never dereferenced. HUD refresh identifies the locally
    // controlled pawn; remote replicated firing must remain entirely native.
    static LOCAL_PAWN_OBSERVATION: Cell<Option<(usize, u64)>> = const { Cell::new(None) };
    static LAST_RETICLE_RESULT: Cell<Option<(bool, &'static str)>> = const { Cell::new(None) };
    static LAST_DOT_DECISION: Cell<Option<(u8, i32, i32)>> = const { Cell::new(None) };
}
static RETICLE_RECORDS: AtomicU32 = AtomicU32::new(0);
static DOT_RECORDS: AtomicU32 = AtomicU32::new(0);
static DOT_STATE_RECORDS: AtomicU32 = AtomicU32::new(0);

static_detour! {
    static WorldRaycastHook: unsafe extern "thiscall" fn(
        *mut c_void,
        usize,
        *const FVector,
        *const FVector,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
    ) -> *mut c_void;

    static WeaponTraceDispatchHook: unsafe extern "thiscall" fn(
        *mut c_void,
        *mut u8,
    );
    static AimRaycastsHook: unsafe extern "thiscall" fn(
        *mut c_void, usize, usize, usize, usize, usize,
        usize, usize, usize, usize, usize,
    );
    static ReticleSelectHook: unsafe extern "thiscall" fn(*mut c_void, i32);
    static ReticleUpdateHook: unsafe extern "thiscall" fn(*mut c_void, f32);
    static ReticleVisibilityHook: unsafe extern "thiscall" fn(*mut c_void, u8);
    static NativeReticleVisibilityHook: unsafe extern "thiscall" fn(*mut c_void) -> u8;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetTickCount64() -> u64;
    fn GetCurrentProcessId() -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn GetAsyncKeyState(key: i32) -> i16;
    fn GetForegroundWindow() -> *mut c_void;
    fn GetWindowThreadProcessId(window: *mut c_void, process: *mut u32) -> u32;
}

fn query_contexts() -> &'static Mutex<HashMap<usize, QueryContext>> {
    QUERY_CONTEXTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn read_vector(vector: *const FVector) -> Option<FVector> {
    if vector.is_null() || !vector.is_aligned() {
        return None;
    }
    // These are the same start/end pointers dereferenced unconditionally by the
    // original retail routine. Reading them before forwarding does not broaden
    // the set of addresses touched by the game.
    Some(unsafe { vector.read() })
}

fn read_pointer(base: *const u8, offset: usize) -> Option<*const u8> {
    if base.is_null() {
        return None;
    }
    let value = unsafe { base.add(offset).cast::<usize>().read_unaligned() } as *const u8;
    (!value.is_null()).then_some(value)
}

fn read_vector_at(base: *const u8, offset: usize) -> Option<FVector> {
    if base.is_null() {
        return None;
    }
    read_vector(unsafe { base.add(offset).cast::<FVector>() })
}

fn read_u32_at(base: *const u8, offset: usize) -> Option<u32> {
    if base.is_null() {
        return None;
    }
    Some(unsafe { base.add(offset).cast::<u32>().read_unaligned() })
}

fn usable_vector(value: FVector) -> bool {
    value.x.is_finite() && value.y.is_finite() && value.z.is_finite()
}

fn distance_squared(left: FVector, right: FVector) -> f32 {
    let x = left.x - right.x;
    let y = left.y - right.y;
    let z = left.z - right.z;
    x * x + y * y + z * z
}

fn pawn_roles(flags: Option<u32>) -> (bool, bool) {
    let flags = flags.unwrap_or_default();
    (flags & PAWN_IS_SPY != 0, flags & PAWN_IS_MERC != 0)
}

fn remember_query_context(actor: usize, context: QueryContext) {
    if actor == 0 || context.query_owner == 0 || context.query_filter == 0 {
        return;
    }
    if let Ok(mut contexts) = query_contexts().lock() {
        if !contexts.contains_key(&actor) && contexts.len() >= MAX_QUERY_CONTEXTS {
            if let Some(oldest) = contexts
                .iter()
                .min_by_key(|(_, value)| value.timestamp_ms)
                .map(|(actor, _)| *actor)
            {
                contexts.remove(&oldest);
            }
        }
        contexts.insert(actor, context);
    }
}

fn context_snapshot(context: Option<QueryContext>, now_ms: u64) -> ContextSnapshot {
    let Some(context) = context else {
        return ContextSnapshot::absent(ContextStatus::Missing);
    };
    let age_ms = now_ms.saturating_sub(context.timestamp_ms);
    let ready = (age_ms <= QUERY_CONTEXT_MAX_AGE_MS).then_some((context, age_ms));
    ContextSnapshot {
        status: if ready.is_some() {
            ContextStatus::Ready
        } else {
            ContextStatus::Expired
        },
        age_ms: Some(age_ms),
        ready,
    }
}

fn query_context_snapshot(actor: usize, now_ms: u64) -> ContextSnapshot {
    let Ok(contexts) = query_contexts().lock() else {
        return ContextSnapshot::absent(ContextStatus::Unavailable);
    };
    context_snapshot(contexts.get(&actor).copied(), now_ms)
}

#[cfg(test)]
fn query_context_for(actor: usize, now_ms: u64) -> Option<(QueryContext, u64)> {
    query_context_snapshot(actor, now_ms).ready
}

// Historical predicate retained only for regression comparisons with r6/r7.
#[cfg(test)]
fn should_apply_spy_obstruction_fix(
    spy_weapon_trace: bool,
    camera_query: Option<ObstructionQueryResult>,
    muzzle_query: Option<ObstructionQueryResult>,
) -> bool {
    spy_weapon_trace
        && camera_query.is_some_and(|result| !result.blocked)
        && muzzle_query.is_some_and(|result| result.blocked)
}

fn should_block_confirmed_shot(
    spy_weapon_trace: bool,
    confirmed_target: Option<FVector>,
    muzzle_query: Option<ObstructionQueryResult>,
) -> bool {
    // The native shot query already reached a merc. Recheck the finite muzzle
    // segment with bullet collision, not the old scenery profile. Camera
    // blockage never exempts a real muzzle wall; a native-passable gap is clear.
    // No native target, invalid query, or unobstructed muzzle passes through.
    spy_weapon_trace
        && confirmed_target.is_some_and(usable_vector)
        && muzzle_query
            .is_some_and(|result| result.blocked && result.hit_position.is_some_and(usable_vector))
}

fn read_muzzle_position(weapon_state: *mut c_void) -> Option<FVector> {
    if weapon_state.is_null() {
        return None;
    }
    let getter: unsafe extern "thiscall" fn(*mut c_void) -> *const FVector =
        unsafe { std::mem::transmute(DX11_WEAPON_MUZZLE_POSITION) };
    let value = read_vector(unsafe { getter(weapon_state) })?;
    usable_vector(value).then_some(value)
}

// Native FCheckResult copies 18 dwords. Byte arrays alone have alignment1,
// which can make read_vector reject a valid hit under a different stack layout.
#[repr(C, align(16))]
struct NativeHitBuffer {
    bytes: [u8; 0x48],
}

type FilteredRaycast = unsafe extern "thiscall" fn(
    *mut c_void,
    *mut u8,
    usize,
    *const FVector,
    *const FVector,
    usize,
    usize,
    usize,
) -> u8;

fn query_obstruction(
    context: QueryContext,
    origin: FVector,
    target: FVector,
) -> Option<ObstructionQueryResult> {
    let query: FilteredRaycast = unsafe { std::mem::transmute(DX11_FILTERED_RAYCAST) };
    query_obstruction_with(query, context, origin, target, hit_is_multiplayer_pawn)
}

fn query_obstruction_with(
    query: FilteredRaycast,
    context: QueryContext,
    origin: FVector,
    target: FVector,
    is_player: impl FnOnce(*const u8) -> bool,
) -> Option<ObstructionQueryResult> {
    // Preserve a point muzzle segment (zero extent); do not artificially widen
    // it across a narrow opening. Only collision selection changes to the
    // native synchronous bullet profile proven at E51814.
    query_obstruction_profile_with(query, context, origin, target, SHOT_QUERY_MASK, is_player)
}

fn query_obstruction_profile_with(
    query: FilteredRaycast,
    context: QueryContext,
    origin: FVector,
    target: FVector,
    mask: usize,
    is_player: impl FnOnce(*const u8) -> bool,
) -> Option<ObstructionQueryResult> {
    if context.query_owner == 0
        || context.query_filter == 0
        || !usable_vector(origin)
        || !usable_vector(target)
        || distance_squared(origin, target) < 1.0
    {
        return None;
    }

    let mut hit = NativeHitBuffer { bytes: [0; 0x48] };
    let clear = unsafe {
        query(
            context.query_owner as *mut c_void,
            hit.bytes.as_mut_ptr(),
            context.query_filter,
            // 0x005ABC80 preserves (end,start) down to 0x006CA4E0,
            // which computes hit = start + (end - start) * fraction.
            &target,
            &origin,
            mask,
            OBSTRUCTION_QUERY_MATERIAL,
            DX11_ZERO_VECTOR,
        )
    };
    // A ray reaching a player is a possible hit, not an opaque muzzle wall.
    // Never turn the intended victim itself into an obstruction/X prompt.
    let player_hit = clear == 0 && read_pointer(hit.bytes.as_ptr(), 4).is_some_and(is_player);
    let blocked = clear == 0 && !player_hit;
    let hit_position = blocked
        .then(|| read_vector_at(hit.bytes.as_ptr(), 8))
        .flatten()
        .filter(|value| usable_vector(*value));
    Some(ObstructionQueryResult {
        blocked,
        hit_position,
    })
}

fn diagnostic_legacy_muzzle_query(
    context: QueryContext,
    muzzle: FVector,
    target: FVector,
) -> Option<ObstructionQueryResult> {
    if !diagnostics_enabled()
        || DIAGNOSTIC_RECORDS.load(Ordering::Relaxed) >= MAX_DIAGNOSTIC_RECORDS
    {
        return None;
    }
    let end = target_before_camera_hit(muzzle, target, target)?;
    let query: FilteredRaycast = unsafe { std::mem::transmute(DX11_FILTERED_RAYCAST) };
    validated_segment_query(
        &mut |from, to| {
            query_obstruction_profile_with(
                query,
                context,
                from,
                to,
                LEGACY_SCENERY_QUERY_MASK,
                hit_is_multiplayer_pawn,
            )
        },
        muzzle,
        end,
    )
}

fn shot_merc_target(
    context: QueryContext,
    origin: FVector,
    end: FVector,
    extent: f32,
) -> Option<FVector> {
    let query: FilteredRaycast = unsafe { std::mem::transmute(DX11_FILTERED_RAYCAST) };
    shot_merc_target_with(query, context, origin, end, extent, primary_merc_target)
}

fn shot_merc_target_with(
    query: FilteredRaycast,
    context: QueryContext,
    origin: FVector,
    end: FVector,
    extent: f32,
    resolve_merc: impl FnOnce(*const u8) -> Option<FVector>,
) -> Option<FVector> {
    if context.query_owner == 0
        || context.query_filter == 0
        || !usable_vector(origin)
        || !usable_vector(end)
        || distance_squared(origin, end) < 1.0
        || !extent.is_finite()
        || extent < 0.0
    {
        return None;
    }
    let sweep = FVector {
        x: extent,
        y: 0.0,
        z: extent,
    };
    let mut hit = NativeHitBuffer { bytes: [0; 0x48] };
    let clear = unsafe {
        query(
            context.query_owner as *mut c_void,
            hit.bytes.as_mut_ptr(),
            context.query_filter,
            &end,
            &origin,
            SHOT_QUERY_MASK,
            OBSTRUCTION_QUERY_MATERIAL,
            &sweep as *const FVector as usize,
        )
    };
    if clear != 0 {
        return None;
    }
    // First accepted hit only. Never select a merc behind the first obstacle,
    // reuse HUD center aim, or grow the native weapon's hit volume.
    resolve_merc(hit.bytes.as_ptr())
        .filter(|point| hit_within_shot_sweep(origin, end, *point, sweep))
}

fn hit_within_shot_sweep(origin: FVector, end: FVector, hit: FVector, extent: FVector) -> bool {
    if !usable_vector(origin)
        || !usable_vector(end)
        || !usable_vector(hit)
        || !usable_vector(extent)
    {
        return false;
    }
    let length2 = distance_squared(origin, end);
    if !length2.is_finite() || length2 < 1.0 {
        return false;
    }
    let along = ((hit.x - origin.x) * (end.x - origin.x)
        + (hit.y - origin.y) * (end.y - origin.y)
        + (hit.z - origin.z) * (end.z - origin.z))
        / length2;
    let projected = FVector {
        x: origin.x + along * (end.x - origin.x),
        y: origin.y + along * (end.y - origin.y),
        z: origin.z + along * (end.z - origin.z),
    };
    // A swept hit need not be collinear with the crosshair. This bound only
    // validates the native result; it does not create a hit or enlarge a ray.
    along.is_finite()
        && (0.0..=1.0).contains(&along)
        && distance_squared(hit, projected)
            <= distance_squared(
                extent,
                FVector {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
            ) + 0.01
}

fn weapon_trace_dispatch(weapon_state: *mut c_void, trace: *mut u8) {
    let now_ms = unsafe { GetTickCount64() };
    if trace.is_null() {
        unsafe { WeaponTraceDispatchHook.call(weapon_state, trace) };
        emit_trace(TraceAction::NullTrace, None);
        return;
    }

    let state = weapon_state.cast::<u8>();
    let owner = read_pointer(state, 0x28);
    let pawn = owner.and_then(|value| read_pointer(value, 0x8c));
    let trace_near = read_vector_at(trace, 4);
    let trace_far = read_vector_at(trace, 0x10);
    let trace_actor = read_pointer(trace, 0)
        .map(|value| value as usize)
        .unwrap_or_default();
    let pawn_flags = pawn
        .filter(|value| *value as usize == trace_actor)
        .and_then(|value| read_u32_at(value, PAWN_GAMEPLAY_FLAGS_OFFSET));
    let (is_spy, is_merc) = pawn_roles(pawn_flags);
    let spy_weapon_trace = is_spy && !is_merc;

    // The replicated dispatch also runs on the receiving machine. Its muzzle
    // animation and trace endpoints are not authoritative local aiming data.
    // Fresh local, same-weapon aim supplies identity/context, NOT the shot hit.
    // Center aim and bullet spread/sweep have different hit coverage.
    let local_aim = AIM_SAMPLES
        .with(|samples| samples.borrow().get(&trace_actor).copied())
        .filter(|sample| {
            confirmed_local_sample(
                *sample,
                LOCAL_PAWN_OBSERVATION.with(|slot| slot.get()),
                trace_actor,
                owner.map(|value| value as usize).unwrap_or_default(),
                now_ms,
            )
        });
    let context_snapshot = if spy_weapon_trace && local_aim.is_some() {
        context_snapshot(local_aim.map(|sample| sample.context), now_ms)
    } else {
        ContextSnapshot::absent(ContextStatus::NotApplicable)
    };
    let query_context = context_snapshot.ready;
    let shot_extent = query_context
        .and_then(|_| read_u32_at(state, 0x16c))
        .map(f32::from_bits)
        .filter(|value| value.is_finite() && *value >= 0.0);
    let target = match (query_context, trace_near, trace_far, shot_extent) {
        (Some((context, _)), Some(from), Some(to), Some(extent)) => {
            shot_merc_target(context, from, to, extent)
        }
        _ => None,
    };
    let muzzle = target.and_then(|_| read_muzzle_position(weapon_state));
    let queries = match (query_context, trace_near, muzzle, target) {
        (Some((context, _)), Some(camera), Some(muzzle), Some(target)) => sightline_queries_with(
            |from, to| query_obstruction(context, from, to),
            camera,
            muzzle,
            target,
        ),
        _ => None,
    };
    let camera_query = queries.map(|(camera, _, _)| camera);
    let muzzle_query = queries.map(|(_, muzzle, _)| muzzle);
    let effective_target = queries.map(|(_, _, target)| target);
    let blocked = should_block_confirmed_shot(spy_weapon_trace, effective_target, muzzle_query);
    let obstruction_hit = muzzle_query.and_then(|result| result.hit_position);

    // Scalar comparison from the SAME dispatch and finite muzzle endpoints.
    // No feedback into blocking, HUD state, or native damage. The old profile
    // is never queried every frame; diagnostic builds cap this at 512 shots.
    let legacy_muzzle_query = match (query_context, muzzle, effective_target) {
        (Some((context, _)), Some(muzzle), Some(target)) => {
            diagnostic_legacy_muzzle_query(context, muzzle, target)
        }
        _ => None,
    };

    let diagnostic = diagnostics_enabled().then(|| TraceEvidence {
        context: context_snapshot.status,
        context_age_ms: context_snapshot.age_ms,
        camera_blocked: camera_query.map(|result| result.blocked),
        muzzle_blocked: muzzle_query.map(|result| result.blocked),
        camera_ray_length: finite_distance(trace_near, effective_target),
        muzzle_ray_length: finite_distance(muzzle, effective_target),
        camera_hit_distance: finite_distance(
            trace_near,
            camera_query.and_then(|result| result.hit_position),
        ),
        muzzle_hit_distance: finite_distance(muzzle, obstruction_hit),
        legacy_muzzle_blocked: legacy_muzzle_query.map(|result| result.blocked),
        legacy_muzzle_hit_distance: finite_distance(
            muzzle,
            legacy_muzzle_query.and_then(|result| result.hit_position),
        ),
        confirmed_target_distance: finite_distance(trace_near, target),
        local_aim_confirmed: local_aim.is_some(),
        center_aim_has_merc: local_aim.is_some_and(|sample| sample.merc_target.is_some()),
        shot_extent,
        // Observation only: an async mouse sample is not proof of game input.
        sampled_left_button_down: unsafe { GetAsyncKeyState(1) } as u16 & 0x8000 != 0,
    });

    // Do not submit any native damage ray for a confirmed obstruction.
    // Shortening a native camera ray still enters its penetration/deferred
    // damage machinery. Parent firing/ammo/animation remain native.
    if !dispatch_unobstructed(blocked, || unsafe {
        WeaponTraceDispatchHook.call(weapon_state, trace)
    }) {
        emit_trace(TraceAction::SuppressedObstruction, diagnostic);
        return;
    }
    if diagnostics_enabled() {
        let reason = passthrough_reason(
            pawn.is_some(),
            pawn.is_some_and(|value| value as usize == trace_actor),
            spy_weapon_trace,
            context_snapshot.status,
            camera_query.map(|result| result.blocked),
            muzzle_query.map(|result| result.blocked),
        );
        let reason = if spy_weapon_trace && local_aim.is_none() {
            PassthroughReason::UnconfirmedLocalAim
        } else if spy_weapon_trace && query_context.is_some() && target.is_none() {
            PassthroughReason::NoShotMercHit
        } else {
            reason
        };
        emit_trace(TraceAction::Passthrough(reason), diagnostic);
    }
}

fn dispatch_unobstructed(blocked: bool, submit: impl FnOnce()) -> bool {
    if blocked {
        return false;
    }
    submit();
    true
}

fn sightline_queries_with(
    mut query: impl FnMut(FVector, FVector) -> Option<ObstructionQueryResult>,
    camera: FVector,
    muzzle: FVector,
    target: FVector,
) -> Option<(ObstructionQueryResult, ObstructionQueryResult, FVector)> {
    // A finite native merc hit is the endpoint, NOT weapon maximum range.
    // HUD predicts a native center bullet; firing uses its actual shot hit.
    // End each segment just before that surface so neither the victim nor
    // scenery behind it can cancel a valid hit. Never shorten again after a
    // query. Both segments use bullet collision, never the scenery profile.
    let camera_end = target_before_camera_hit(camera, target, target)?;
    let muzzle_end = target_before_camera_hit(muzzle, target, target)?;
    let camera_query = validated_segment_query(&mut query, camera, camera_end)?;
    let muzzle_query = validated_segment_query(&mut query, muzzle, muzzle_end)?;
    Some((camera_query, muzzle_query, target))
}

fn validated_segment_query(
    query: &mut impl FnMut(FVector, FVector) -> Option<ObstructionQueryResult>,
    origin: FVector,
    end: FVector,
) -> Option<ObstructionQueryResult> {
    let result = query(origin, end)?;
    if result.blocked {
        let hit = result.hit_position?;
        if !point_on_segment(origin, end, hit) {
            return None;
        }
    }
    Some(result)
}

fn point_on_segment(origin: FVector, end: FVector, point: FVector) -> bool {
    if !usable_vector(origin) || !usable_vector(end) || !usable_vector(point) {
        return false;
    }
    let length2 = distance_squared(origin, end);
    if !length2.is_finite() || length2 < 1.0 {
        return false;
    }
    let dx = end.x - origin.x;
    let dy = end.y - origin.y;
    let dz = end.z - origin.z;
    let along = ((point.x - origin.x) * dx + (point.y - origin.y) * dy + (point.z - origin.z) * dz)
        / length2;
    let projected = FVector {
        x: origin.x + along * dx,
        y: origin.y + along * dy,
        z: origin.z + along * dz,
    };
    along.is_finite() && (0.0..=1.0).contains(&along) && distance_squared(point, projected) <= 0.01
}

fn confirmed_local_sample(
    sample: AimSample,
    observed: Option<(usize, u64)>,
    pawn: usize,
    weapon: usize,
    now_ms: u64,
) -> bool {
    let Some((local_pawn, observed_at)) = observed else {
        return false;
    };
    pawn != 0
        && pawn == local_pawn
        && now_ms >= observed_at
        && now_ms - observed_at <= AIM_MAX_AGE_MS
        && aim_sample_is_current(sample, pawn, weapon, now_ms)
}

fn class_derives_from(mut class: *const u8, expected: *const u8) -> bool {
    if expected.is_null() {
        return false;
    }
    // UObject::IsA (43F4D0) walks UClass+28. Bound corrupted/cyclic input.
    for _ in 0..64 {
        if class.is_null() {
            return false;
        }
        if class == expected {
            return true;
        }
        class = read_pointer(class, 0x28).unwrap_or(std::ptr::null());
    }
    false
}

fn hit_is_multiplayer_pawn(actor: *const u8) -> bool {
    let Some(class) = read_pointer(actor, 0x24) else {
        return false;
    };
    let expected = unsafe { (DX11_PLAYER_PAWN_CLASS as *const *const u8).read() };
    if !class_derives_from(class, expected) {
        return false;
    }
    matches!(
        pawn_roles(read_u32_at(actor, PAWN_GAMEPLAY_FLAGS_OFFSET)),
        (true, false) | (false, true)
    )
}

fn primary_merc_target(result: *const u8) -> Option<FVector> {
    let expected = unsafe { (DX11_PLAYER_PAWN_CLASS as *const *const u8).read() };
    primary_merc_target_for_class(result, expected)
}

fn primary_merc_target_for_class(result: *const u8, expected: *const u8) -> Option<FVector> {
    let actor = read_pointer(result, 4)?;
    let class = read_pointer(actor, 0x24)?;
    if !class_derives_from(class, expected)
        || pawn_roles(read_u32_at(actor, PAWN_GAMEPLAY_FLAGS_OFFSET)) != (false, true)
    {
        return None;
    }
    read_vector_at(result, 8).filter(|hit| usable_vector(*hit))
}

fn reticle_update(hud: *mut c_void, dt: f32) {
    // Read the existing native local-pawn chain; the dot/RMB UI below is unchanged.
    let local_pawn = read_pointer(hud.cast(), 0x25c).and_then(|owner| read_pointer(owner, 0x310));
    LOCAL_PAWN_OBSERVATION
        .with(|slot| slot.set(local_pawn.map(|pawn| (pawn as usize, unsafe { GetTickCount64() }))));
    let mut pid = 0;
    let foreground = unsafe {
        let window = GetForegroundWindow();
        !window.is_null()
            && GetWindowThreadProcessId(window, &mut pid) != 0
            && pid == GetCurrentProcessId()
    };
    let aiming = foreground && (unsafe { GetAsyncKeyState(2) } as u16 & 0x8000 != 0);
    let frame = ReticleFrame {
        hud: hud as usize,
        aiming,
        foreground,
        pawn: local_pawn.map(|pawn| pawn as usize).unwrap_or_default(),
        requested: None,
        selected: -1,
        fallback_dot: false,
        mod_x: false,
    };
    let previous = ACTIVE_RETICLE_FRAME.with(|slot| slot.replace(frame));
    unsafe { ReticleUpdateHook.call(hud, dt) };
    crate::equipment_native::observe_stamina(hud as usize, dt, foreground);
    ACTIVE_RETICLE_FRAME.with(|slot| slot.set(previous));
}

fn spy_hud(hud: *const u8) -> bool {
    let pawn = read_pointer(hud, 0x25c).and_then(|owner| read_pointer(owner, 0x310));
    let roles = pawn_roles(pawn.and_then(|pawn| read_u32_at(pawn, PAWN_GAMEPLAY_FLAGS_OFFSET)));
    roles == (true, false)
}

fn idle_reticle_kind(
    requested: i32,
    multiplayer: bool,
    frame_active: bool,
    native_visible: Option<bool>,
) -> i32 {
    if multiplayer
        && frame_active
        && native_visible == Some(false)
        && matches!(requested, 0 | 3 | 4 | 5 | 6 | 7 | 9)
    {
        DOT_RETICLE as i32
    } else {
        requested
    }
}

fn should_show_idle_dot(
    in_update: bool,
    multiplayer: bool,
    selected: Option<u32>,
    same_component: bool,
    fallback_dot: bool,
) -> bool {
    in_update && multiplayer && fallback_dot && selected == Some(DOT_RETICLE) && same_component
}

fn native_reticle_visibility(this: *mut c_void) -> u8 {
    let visible = unsafe { NativeReticleVisibilityHook.call(this) };
    let mut frame = ACTIVE_RETICLE_FRAME.with(|slot| slot.get());
    // The original function itself reads this+61c -> context+24 as its pawn.
    // Match only the current HUD's local pawn; no remote/menu/global override.
    if frame.hud == 0 || !frame.foreground || frame.pawn == 0 || !ReticleSelectHook.is_enabled() {
        return visible;
    }
    let owner = read_pointer(this.cast(), 0x61c).and_then(|context| read_pointer(context, 0x24));
    if owner.map(|pawn| pawn as usize) != Some(frame.pawn) || !spy_hud(frame.hud as *const u8) {
        return visible;
    }
    let Some(requested) = frame.requested else {
        return visible;
    };
    if diagnostics_enabled() {
        // Observe only the already-validated local Spy/native HUD stack. Do not
        // call a scene query or synthesize shots to investigate a camera offset.
        unsafe {
            crate::aim_observation::observe(this as usize, visible, requested, GetTickCount64());
        }
    }
    frame.fallback_dot =
        visible == 0 && idle_reticle_kind(requested, true, true, Some(false)) == DOT_RETICLE as i32;
    let selected = if frame.fallback_dot {
        DOT_RETICLE as i32
    } else {
        frame.selected
    };
    // Run before the native update obtains its component (1A6897D). The game
    // then positions/updates the correct component in this SAME frame, so no
    // delayed right-button sample or previous-frame visibility is involved.
    if read_u32_at(frame.hud as *const u8, 0x2d4) != Some(selected as u32) {
        unsafe { ReticleSelectHook.call(frame.hud as *mut c_void, selected) };
    }
    ACTIVE_RETICLE_FRAME.with(|slot| slot.set(frame));
    let changed = LAST_DOT_DECISION.with(|slot| {
        slot.replace(Some((visible, requested, selected))) != Some((visible, requested, selected))
    });
    if diagnostics_enabled() && changed && take_diagnostic_slot(&DOT_STATE_RECORDS, 128).is_some() {
        info!(
            "SCBL_MULTIPLAYER_IDLE_DOT_STATE native_visible={visible} fallback={} native_kind={requested} final_kind={selected} visibility_source=native-reticle revision=11",
            frame.fallback_dot
        );
    }
    visible
}

fn force_white_reticle(
    active_spy_frame: bool,
    selected: Option<u32>,
    same_selected_component: bool,
    fallback_dot: bool,
    mod_x: bool,
) -> bool {
    active_spy_frame
        && same_selected_component
        && ((fallback_dot && selected == Some(DOT_RETICLE))
            || (mod_x && selected == Some(UNAVAILABLE_RETICLE as u32)))
}

fn reticle_visibility(component: *mut c_void, visible: u8) {
    let frame = ACTIVE_RETICLE_FRAME.with(|slot| slot.get());
    let hud = frame.hud as *const u8;
    let multiplayer = spy_hud(hud);
    let selected = read_u32_at(hud, 0x2d4);
    let same = read_pointer(hud, 0x2b0) == Some(component.cast());
    let dot_owned = !hud.is_null() && multiplayer && same;
    let active_spy = frame.foreground && !hud.is_null() && multiplayer;
    let idle_dot = should_show_idle_dot(
        frame.foreground && !hud.is_null(),
        multiplayer,
        selected,
        same,
        frame.fallback_dot,
    );
    let same_selected = selected
        .filter(|kind| *kind < 12)
        .and_then(|kind| read_pointer(hud, 0x2a4 + kind as usize * 4))
        == Some(component.cast());
    if force_white_reticle(
        active_spy,
        selected,
        same_selected,
        frame.fallback_dot,
        frame.mod_x,
    ) {
        let color: unsafe extern "thiscall" fn(*mut c_void, i32) =
            unsafe { std::mem::transmute(DX11_RETICLE_COLOR) };
        // Native update applies target red/blue at 1A6898C BEFORE this call.
        // Reapply color1 (the resource's default white) only to our dot/X.
        unsafe { color(component, 1) };
    }
    // Visibility handoff uses this frame's native result, not right-button
    // state. A stale dot cannot survive another selected reticle.
    let visible = if idle_dot {
        1
    } else if dot_owned && (!frame.foreground || selected != Some(DOT_RETICLE)) {
        0
    } else {
        visible
    };
    unsafe { ReticleVisibilityHook.call(component, visible) };
    if idle_dot && diagnostics_enabled() && take_diagnostic_slot(&DOT_RECORDS, 8).is_some() {
        info!(
            "SCBL_MULTIPLAYER_IDLE_DOT active=true source=native-dot color=white visibility_source=native-reticle resource_present={} revision=11",
            read_pointer(component.cast(), 0).is_some()
        );
    }
}

fn current_weapon(pawn: *const u8) -> Option<*const u8> {
    // Native HUD calls this getter too. It unconditionally uses pawn+93c;
    // reject absent weapon managers before entering the retail function.
    read_pointer(pawn, 0x93c)?;
    let getter: unsafe extern "thiscall" fn(*const u8) -> *const u8 =
        unsafe { std::mem::transmute(DX11_CURRENT_WEAPON) };
    let weapon = unsafe { getter(pawn) };
    (!weapon.is_null()).then_some(weapon)
}

#[allow(clippy::too_many_arguments)]
fn aim_raycasts(
    this: *mut c_void,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
    a7: usize,
    a8: usize,
    a9: usize,
    a10: usize,
) {
    let previous = AIM_CAPTURE.with(|slot| slot.replace(Some(None)));
    unsafe { AimRaycastsHook.call(this, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10) };
    let captured = AIM_CAPTURE.with(|slot| slot.replace(previous)).flatten();
    if let Some(mut sample) = captured {
        let pawn = sample.pawn as *const u8;
        if pawn_roles(read_u32_at(pawn, PAWN_GAMEPLAY_FLAGS_OFFSET)) != (true, false) {
            return;
        }
        // The native primary aim result owns actor+4 and hit-position+8.
        // Copy the result before its caller reuses the buffer; never retain
        // an actor/hit-list pointer. Verify UObject class before reading flags.
        sample.merc_target = primary_merc_target(a1 as *const u8);
        sample.weapon = current_weapon(pawn).map(|p| p as usize).unwrap_or_default();
        AIM_SAMPLES.with(|samples| {
            let mut samples = samples.borrow_mut();
            if samples.len() >= MAX_QUERY_CONTEXTS && !samples.contains_key(&sample.pawn) {
                samples.clear();
            }
            samples.insert(sample.pawn, sample);
        });
    }
}

fn aim_sample_is_current(sample: AimSample, pawn: usize, weapon: usize, now_ms: u64) -> bool {
    sample.pawn == pawn
        && weapon != 0
        && sample.weapon == weapon
        && now_ms >= sample.context.timestamp_ms
        && now_ms - sample.context.timestamp_ms <= AIM_MAX_AGE_MS
}

fn reticle_kind_for_obstruction(requested: i32, blocked: bool) -> i32 {
    // Only gun kinds emitted by the retail weapon selector. Preserve gadget,
    // hidden/special, and native Unavailable states without overriding them.
    if blocked && matches!(requested, 0 | 4 | 5 | 6 | 7 | 9) {
        UNAVAILABLE_RETICLE
    } else {
        requested
    }
}

fn target_before_camera_hit(origin: FVector, target: FVector, hit: FVector) -> Option<FVector> {
    if !usable_vector(origin) || !usable_vector(target) || !usable_vector(hit) {
        return None;
    }
    let ray_length = distance_squared(origin, target).sqrt();
    let hit_length = distance_squared(origin, hit).sqrt();
    if !ray_length.is_finite()
        || !hit_length.is_finite()
        || hit_length < 4.0
        || hit_length > ray_length + 0.1
    {
        return None;
    }
    let dot = (hit.x - origin.x) * (target.x - origin.x)
        + (hit.y - origin.y) * (target.y - origin.y)
        + (hit.z - origin.z) * (target.z - origin.z);
    if dot <= 0.0 || dot / (ray_length * hit_length) < 0.999 {
        return None;
    }
    let scale = (hit_length - 2.0) / hit_length;
    Some(FVector {
        x: origin.x + (hit.x - origin.x) * scale,
        y: origin.y + (hit.y - origin.y) * scale,
        z: origin.z + (hit.z - origin.z) * scale,
    })
}

fn aim_obstruction(hud: *mut c_void) -> (bool, &'static str) {
    if AIM_CAPTURE.with(|slot| slot.get().is_some()) {
        return (false, "native-query-active");
    }
    // Same local-pawn chain as HudReticles::update -> 1A52310. Cached remote
    // actors are never dereferenced by this presentation path.
    let Some(pawn) = read_pointer(hud.cast(), 0x25c).and_then(|owner| read_pointer(owner, 0x310))
    else {
        return (false, "no-local-pawn");
    };
    if pawn_roles(read_u32_at(pawn, PAWN_GAMEPLAY_FLAGS_OFFSET)) != (true, false) {
        return (false, "not-spy");
    }
    let Some(sample) = AIM_SAMPLES.with(|samples| samples.borrow().get(&(pawn as usize)).copied())
    else {
        return (false, "no-aim-sample");
    };
    let Some(weapon) = current_weapon(pawn) else {
        return (false, "no-weapon");
    };
    if !aim_sample_is_current(sample, pawn as usize, weapon as usize, unsafe {
        GetTickCount64()
    }) {
        return (false, "stale-or-switched");
    }
    // E07B30 obtains exactly this state to call DFFDA0. Require the original
    // owner chain to match before asking for the current muzzle position.
    let Some(state) = read_pointer(weapon, 0x748) else {
        return (false, "no-weapon-state");
    };
    if read_pointer(state, 0x28) != Some(weapon) || read_pointer(weapon, 0x8c) != Some(pawn) {
        return (false, "owner-mismatch");
    }
    let Some(muzzle) = read_muzzle_position(state.cast_mut().cast()) else {
        return (false, "no-muzzle");
    };
    let Some(extent) = read_u32_at(state, 0x16c)
        .map(f32::from_bits)
        .filter(|value| value.is_finite() && *value >= 0.0)
    else {
        return (false, "invalid-shot-extent");
    };
    // HUD primary aim and native bullet collision use different masks. Predict
    // the current center shot using the same read-only profile as firing; never
    // call WeaponTraceDispatch or cache a previous shot merely to show the X.
    // Spread is still determined by the real shot, not this visual prediction.
    let merc_hit = shot_merc_target(sample.context, sample.origin, sample.target, extent);
    aim_obstruction_for_target_with(
        |from, to| query_obstruction(sample.context, from, to),
        sample.origin,
        muzzle,
        merc_hit,
    )
}

fn aim_obstruction_for_target_with(
    query: impl FnMut(FVector, FVector) -> Option<ObstructionQueryResult>,
    camera: FVector,
    muzzle: FVector,
    merc_hit: Option<FVector>,
) -> (bool, &'static str) {
    let Some(merc_hit) = merc_hit else {
        return (false, "not-aiming-at-merc");
    };
    let Some((_, muzzle_query, target)) = sightline_queries_with(query, camera, muzzle, merc_hit)
    else {
        return (false, "invalid-sightline");
    };
    let blocked = should_block_confirmed_shot(true, Some(target), Some(muzzle_query));
    (
        blocked,
        if blocked {
            "merc-muzzle-blocked"
        } else {
            "muzzle-clear"
        },
    )
}

fn reticle_select(hud: *mut c_void, requested: i32) {
    let frame = ACTIVE_RETICLE_FRAME.with(|slot| slot.get());
    let active = frame.hud == hud as usize && frame.foreground;
    let (blocked, reason) = if !OBSTRUCTION_FIX_ENABLED {
        (false, "feature-disabled-native-shooting")
    } else if !active || !frame.aiming {
        (false, "right-button-not-aiming")
    } else if reticle_kind_for_obstruction(requested, true) != requested {
        aim_obstruction(hud)
    } else {
        (false, "native-special")
    };
    let selected = reticle_kind_for_obstruction(requested, blocked);
    if frame.hud == hud as usize {
        ACTIVE_RETICLE_FRAME.with(|slot| {
            slot.set(ReticleFrame {
                requested: Some(requested),
                selected,
                fallback_dot: false,
                mod_x: blocked
                    && selected == UNAVAILABLE_RETICLE
                    && requested != UNAVAILABLE_RETICLE,
                ..frame
            })
        });
    }
    unsafe { ReticleSelectHook.call(hud, selected) };
    let changed = LAST_RETICLE_RESULT
        .with(|last| last.replace(Some((blocked, reason))) != Some((blocked, reason)));
    if diagnostics_enabled() && changed && take_diagnostic_slot(&RETICLE_RECORDS, 512).is_some() {
        info!(
            "SCBL_WEAPON_OBSTRUCTION_CROSSHAIR active={blocked} reason={reason} requested={requested} selected={selected} right_button={} source=native-unavailable",
            frame.aiming
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn world_raycast(
    owner: *mut c_void,
    arg1: usize,
    start: *const FVector,
    end: *const FVector,
    arg4: usize,
    arg5: usize,
    arg6: usize,
    arg7: usize,
    arg8: usize,
    arg9: usize,
    arg10: usize,
) -> *mut c_void {
    let now_ms = unsafe { GetTickCount64() };
    let result = unsafe {
        WorldRaycastHook.call(
            owner, arg1, start, end, arg4, arg5, arg6, arg7, arg8, arg9, arg10,
        )
    };

    if !owner.is_null() && arg7 == OBSTRUCTION_QUERY_MATERIAL && arg8 != 0 {
        // Copy only inputs while inside the proven native aiming function.
        // The world API's misleading old Rust argument names are retained at
        // its ABI boundary: first vector is END, second vector is START.
        AIM_CAPTURE.with(|slot| {
            if slot.get().is_some() {
                if let (Some(origin), Some(target)) = (read_vector(end), read_vector(start)) {
                    slot.set(Some(Some(AimSample {
                        pawn: arg8,
                        weapon: 0,
                        context: QueryContext {
                            timestamp_ms: now_ms,
                            query_owner: owner as usize,
                            query_filter: arg8,
                        },
                        origin,
                        target,
                        merc_target: None,
                    })));
                }
            }
        });
        remember_query_context(
            arg8,
            QueryContext {
                timestamp_ms: now_ms,
                query_owner: owner as usize,
                query_filter: arg8,
            },
        );
    }

    result
}

// Install only after drone_control has validated the shared native queue bytes.
pub(crate) unsafe fn init_slide_cover() {
    spy_slide_cover::init();
}

pub unsafe fn init(world_raycast_address: usize) {
    if world_raycast_address == 0 {
        error!("SCBL_WEAPON_OBSTRUCTION_FIX unavailable for this executable");
        return;
    }
    let address = world_raycast_address;
    if address != 0x0058_9860 {
        error!("SCBL_WEAPON_OBSTRUCTION_FIX unsupported world_raycast={address:08X}");
        return;
    }

    combat_input::init();
    enemy_identification::init();

    if OBSTRUCTION_FIX_ENABLED {
        let target = std::mem::transmute(address);
        if let Err(err) = WorldRaycastHook
            .initialize(target, world_raycast)
            .and_then(|hook| hook.enable())
        {
            error!("SCBL_WEAPON_OBSTRUCTION_FIX failed stage=world-raycast address={address:08X} error={err:?}");
            return;
        }

        let dispatch_target = std::mem::transmute(DX11_WEAPON_TRACE_DISPATCH);
        if let Err(err) = WeaponTraceDispatchHook
            .initialize(dispatch_target, weapon_trace_dispatch)
            .and_then(|hook| hook.enable())
        {
            error!("SCBL_WEAPON_OBSTRUCTION_FIX failed stage=weapon-dispatch address={DX11_WEAPON_TRACE_DISPATCH:08X} error={err:?}");
            let _ = WorldRaycastHook.disable();
            return;
        }

        // This binding is verified only for the retail DX11 image. If either
        // prologue differs, retain the shot fix but do not install UI hooks.
        let aim_signature = std::slice::from_raw_parts(DX11_AIM_RAYCASTS as *const u8, 6);
        let reticle_signature = std::slice::from_raw_parts(DX11_RETICLE_SELECT as *const u8, 6);
        if aim_signature == [0x55, 0x8b, 0xec, 0x83, 0xec, 0x58]
            && reticle_signature == [0x55, 0x8b, 0xec, 0x83, 0xec, 0x10]
        {
            let aim_result = AimRaycastsHook
                .initialize(std::mem::transmute(DX11_AIM_RAYCASTS), aim_raycasts)
                .and_then(|hook| hook.enable());
            match aim_result {
                Ok(()) => {
                    if let Err(err) = ReticleSelectHook
                        .initialize(std::mem::transmute(DX11_RETICLE_SELECT), reticle_select)
                        .and_then(|hook| hook.enable())
                    {
                        let _ = AimRaycastsHook.disable();
                        error!("SCBL_WEAPON_OBSTRUCTION_CROSSHAIR enabled=false stage=reticle-hook error={err:?}");
                    } else {
                        info!("SCBL_WEAPON_OBSTRUCTION_CROSSHAIR enabled=true native_kind=10 live_aim=true cue=false target_source=native-shot-prediction muzzle_profile=native-bullet revision=11");
                    }
                }
                Err(err) => error!(
                    "SCBL_WEAPON_OBSTRUCTION_CROSSHAIR enabled=false stage=aim-hook error={err:?}"
                ),
            }
        } else {
            error!("SCBL_WEAPON_OBSTRUCTION_CROSSHAIR enabled=false stage=signature-mismatch");
        }
    } else {
        // Reticle selection is still required by the native-visibility white dot.
        // It must not depend on the retired obstruction/aim hooks.
        if std::slice::from_raw_parts(DX11_RETICLE_SELECT as *const u8, 6)
            == [0x55, 0x8b, 0xec, 0x83, 0xec, 0x10]
        {
            if let Err(err) = ReticleSelectHook
                .initialize(std::mem::transmute(DX11_RETICLE_SELECT), reticle_select)
                .and_then(|hook| hook.enable())
            {
                error!(
                    "SCBL_MULTIPLAYER_IDLE_DOT enabled=false stage=reticle-selection error={err:?}"
                );
            }
        }
        info!("SCBL_WEAPON_OBSTRUCTION_FIX enabled=false revision=58 damage=native custom_x=false retained_source=true");
    }

    let update_signature = std::slice::from_raw_parts(DX11_RETICLE_UPDATE as *const u8, 9);
    let visibility_signature = std::slice::from_raw_parts(DX11_RETICLE_VISIBILITY as *const u8, 7);
    let native_visibility_signature =
        std::slice::from_raw_parts(DX11_NATIVE_RETICLE_VISIBILITY as *const u8, 10);
    let color_signature = std::slice::from_raw_parts(DX11_RETICLE_COLOR as *const u8, 6);
    if update_signature == [0x55, 0x8b, 0xec, 0x81, 0xec, 0xa4, 0, 0, 0]
        && visibility_signature == [0x55, 0x8b, 0xec, 0x51, 0x89, 0x4d, 0xfc]
        && native_visibility_signature == [0x55, 0x8b, 0xec, 0x51, 0x8b, 0x89, 0x1c, 0x06, 0, 0]
        && color_signature == [0x55, 0x8b, 0xec, 0x83, 0xec, 0x1c]
    {
        match ReticleVisibilityHook
            .initialize(
                std::mem::transmute(DX11_RETICLE_VISIBILITY),
                reticle_visibility,
            )
            .and_then(|hook| hook.enable())
        {
            Ok(()) => {
                let result = NativeReticleVisibilityHook
                    .initialize(
                        std::mem::transmute(DX11_NATIVE_RETICLE_VISIBILITY),
                        native_reticle_visibility,
                    )
                    .and_then(|hook| hook.enable())
                    .and_then(|_| {
                        ReticleUpdateHook
                            .initialize(std::mem::transmute(DX11_RETICLE_UPDATE), reticle_update)
                    })
                    .and_then(|hook| hook.enable());
                if let Err(err) = result {
                    if NativeReticleVisibilityHook.is_enabled() {
                        let _ = NativeReticleVisibilityHook.disable();
                    }
                    let _ = ReticleVisibilityHook.disable();
                    error!("SCBL_MULTIPLAYER_IDLE_DOT enabled=false stage=native-visibility-or-update error={err:?}");
                } else {
                    info!("SCBL_MULTIPLAYER_IDLE_DOT enabled=true native_kind=3 spy_only=true visibility_source=native-reticle right_button_switch=false x_color=white revision=11");
                }
            }
            Err(err) => {
                error!("SCBL_MULTIPLAYER_IDLE_DOT enabled=false stage=visibility error={err:?}")
            }
        }
    } else {
        error!("SCBL_MULTIPLAYER_IDLE_DOT enabled=false stage=signature-mismatch");
    }

    info!("SCBL_WEAPON_OBSTRUCTION_FIX enabled={OBSTRUCTION_FIX_ENABLED} world_raycast={address:08X} weapon_dispatch={DX11_WEAPON_TRACE_DISPATCH:08X} query_context_max_age_ms={QUERY_CONTEXT_MAX_AGE_MS}");
    info!(
        "SCBL_WEAPON_OBSTRUCTION_TRACE_CONFIG enabled={} limit={MAX_DIAGNOSTIC_RECORDS} coverage=weapon-dispatch-only alternate-native-path=unobserved behavior=confirmed-shot-muzzle-block camera-blocked-exemption=false muzzle_profile=native-bullet firing_revision=10 revision=11",
        diagnostics_enabled()
    );
}

pub(crate) unsafe fn deinit_slide_cover() {
    spy_slide_cover::deinit();
}

pub unsafe fn deinit() {
    enemy_identification::deinit();
    combat_input::deinit();
    if ReticleUpdateHook.is_enabled() {
        let _ = ReticleUpdateHook.disable();
    }
    if NativeReticleVisibilityHook.is_enabled() {
        let _ = NativeReticleVisibilityHook.disable();
    }
    if ReticleVisibilityHook.is_enabled() {
        let _ = ReticleVisibilityHook.disable();
    }
    if ReticleSelectHook.is_enabled() {
        let _ = ReticleSelectHook.disable();
    }
    if AimRaycastsHook.is_enabled() {
        let _ = AimRaycastsHook.disable();
    }
    if let Ok(mut contexts) = query_contexts().lock() {
        contexts.clear();
    }
    if WeaponTraceDispatchHook.is_enabled() {
        let _ = WeaponTraceDispatchHook.disable();
    }
    if WorldRaycastHook.is_enabled() {
        let _ = WorldRaycastHook.disable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r58_obstruction_is_dormant_without_disabling_native_dot() {
        assert!(!OBSTRUCTION_FIX_ENABLED);
        assert_eq!(idle_reticle_kind(4, true, true, Some(false)), 3);
        assert_eq!(idle_reticle_kind(4, true, true, Some(true)), 4);
        assert_eq!(idle_reticle_kind(10, true, true, Some(true)), 10);
        assert!(force_white_reticle(true, Some(3), true, true, false));
        assert!(!force_white_reticle(true, Some(10), true, false, false));
    }

    // Synthetic collision fixture: the old scenery profile sees a closed
    // hull; the native bullet profile can pass the opening. filter=2 models
    // a truly solid wall under BOTH profiles, not a real actor identity.
    unsafe extern "thiscall" fn gap_profile_fixture(
        _owner: *mut c_void,
        hit: *mut u8,
        filter: usize,
        end: *const FVector,
        start: *const FVector,
        mask: usize,
        _material: usize,
        _extent: usize,
    ) -> u8 {
        if mask == SHOT_QUERY_MASK && filter == 1 {
            return 1;
        }
        let a = unsafe { start.read() };
        let b = unsafe { end.read() };
        unsafe {
            hit.add(8).cast::<FVector>().write_unaligned(vector(
                a.x + (b.x - a.x) * 0.25,
                a.y + (b.y - a.y) * 0.25,
                a.z + (b.z - a.z) * 0.25,
            ))
        };
        0
    }

    #[test]
    fn native_bullet_gap_is_not_vetoed_by_scenery_profile() {
        let context = QueryContext {
            timestamp_ms: 0,
            query_owner: 1,
            query_filter: 1,
        };
        let camera = vector(0.0, 0.0, 0.0);
        let muzzle = vector(0.0, 20.0, 0.0);
        let target = vector(400.0, 0.0, 0.0);
        let query =
            |from, to| query_obstruction_with(gap_profile_fixture, context, from, to, |_| false);
        let (_, muzzle_query, endpoint) =
            sightline_queries_with(query, camera, muzzle, target).unwrap();
        let blocked = should_block_confirmed_shot(true, Some(endpoint), Some(muzzle_query));
        assert!(
            !blocked,
            "a native-passable opening must not use the scenery collision profile"
        );
        let mut calls = 0;
        assert!(dispatch_unobstructed(blocked, || calls += 1));
        assert_eq!(calls, 1);
        assert_eq!(
            aim_obstruction_for_target_with(query, camera, muzzle, Some(target)),
            (false, "muzzle-clear")
        );
    }

    #[test]
    fn native_bullet_solid_wall_still_blocks_dispatch_and_shows_x() {
        let context = QueryContext {
            timestamp_ms: 0,
            query_owner: 1,
            query_filter: 2,
        };
        let camera = vector(0.0, 0.0, 0.0);
        let muzzle = vector(0.0, 20.0, 0.0);
        let target = vector(400.0, 0.0, 0.0);
        let query =
            |from, to| query_obstruction_with(gap_profile_fixture, context, from, to, |_| false);
        let (_, muzzle_query, endpoint) =
            sightline_queries_with(query, camera, muzzle, target).unwrap();
        let blocked = should_block_confirmed_shot(true, Some(endpoint), Some(muzzle_query));
        assert!(blocked);
        assert!(!dispatch_unobstructed(blocked, || panic!(
            "solid-wall shot submitted"
        )));
        assert_eq!(
            aim_obstruction_for_target_with(query, camera, muzzle, Some(target)),
            (true, "merc-muzzle-blocked")
        );
    }

    #[test]
    fn x_prediction_shows_when_both_camera_and_muzzle_queries_hit_the_door() {
        let result = aim_obstruction_for_target_with(
            |from, _| {
                Some(ObstructionQueryResult {
                    blocked: true,
                    hit_position: Some(vector(if from.x == 0.0 { 260.0 } else { 250.0 }, 0.0, 0.0)),
                })
            },
            vector(0.0, 0.0, 0.0),
            vector(160.0, 0.0, 0.0),
            Some(vector(650.0, 0.0, 0.0)),
        );
        assert_eq!(result, (true, "merc-muzzle-blocked"));
        assert_eq!(reticle_kind_for_obstruction(4, result.0), 10);
    }

    #[test]
    fn x_prediction_requires_current_native_merc_hit_and_clears_when_moved_away() {
        let camera = vector(0.0, 0.0, 0.0);
        let muzzle = vector(160.0, 0.0, 0.0);
        let target = Some(vector(650.0, 0.0, 0.0));
        for has_wall in [true, false, true, false] {
            let result = aim_obstruction_for_target_with(
                |_, _| {
                    Some(ObstructionQueryResult {
                        blocked: has_wall,
                        hit_position: has_wall.then_some(vector(250.0, 0.0, 0.0)),
                    })
                },
                camera,
                muzzle,
                target,
            );
            assert_eq!(result.0, has_wall);
            assert_eq!(
                reticle_kind_for_obstruction(4, result.0),
                if has_wall { 10 } else { 4 }
            );
        }
        assert_eq!(
            aim_obstruction_for_target_with(
                |_, _| panic!("no merc must not query walls"),
                camera,
                muzzle,
                None
            ),
            (false, "not-aiming-at-merc")
        );
    }

    #[test]
    fn x_prediction_accepts_native_bullet_sweep_without_hud_primary_merc() {
        let camera = vector(0.0, 0.0, 0.0);
        let target = shot_merc_target_with(
            shot_sweep_fixture,
            QueryContext {
                timestamp_ms: 0,
                query_owner: 1,
                query_filter: 12,
            },
            camera,
            vector(1000.0, 0.0, 0.0),
            8.0,
            |result| read_vector_at(result, 8),
        );
        let result = aim_obstruction_for_target_with(
            |from, to| {
                Some(ObstructionQueryResult {
                    blocked: true,
                    hit_position: Some(vector(
                        from.x + (to.x - from.x) * 0.25,
                        from.y + (to.y - from.y) * 0.25,
                        from.z + (to.z - from.z) * 0.25,
                    )),
                })
            },
            camera,
            vector(160.0, 20.0, 0.0),
            target,
        );
        assert_eq!(result, (true, "merc-muzzle-blocked"));
        // This path contains queries only; no native shot, damage or event call.
    }

    #[test]
    fn x_prediction_rejects_unknown_query_and_scenery_past_the_target() {
        let camera = vector(0.0, 0.0, 0.0);
        let muzzle = vector(160.0, 0.0, 0.0);
        let target = Some(vector(650.0, 0.0, 0.0));
        assert_eq!(
            aim_obstruction_for_target_with(|_, _| None, camera, muzzle, target),
            (false, "invalid-sightline")
        );
        assert_eq!(
            aim_obstruction_for_target_with(
                |_, _| Some(ObstructionQueryResult {
                    blocked: true,
                    hit_position: Some(vector(750.0, 0.0, 0.0))
                }),
                camera,
                muzzle,
                target
            ),
            (false, "invalid-sightline")
        );
    }

    #[test]
    fn confirmed_shot_cannot_bypass_a_muzzle_wall_when_camera_is_also_blocked() {
        // Scalar distances from r7 live shots 93/95/96; not world coordinates.
        for (target_distance, camera_wall, muzzle_distance, muzzle_wall) in [
            (649.5724, 260.96365, 493.20544, 94.3603),
            (652.0125, 262.89352, 482.42868, 85.99057),
            (650.59186, 262.0467, 481.7145, 84.69618),
        ] {
            let camera = vector(0.0, 0.0, 0.0);
            let target = vector(target_distance, 0.0, 0.0);
            let muzzle = vector(target_distance - muzzle_distance, 0.0, 0.0);
            let (camera_query, muzzle_query, target) = sightline_queries_with(
                |from, _| {
                    Some(ObstructionQueryResult {
                        blocked: true,
                        hit_position: Some(vector(
                            if from.x == 0.0 {
                                camera_wall
                            } else {
                                from.x + muzzle_wall
                            },
                            0.0,
                            0.0,
                        )),
                    })
                },
                camera,
                muzzle,
                target,
            )
            .unwrap();
            // Prove the previous firing condition let these measurements pass.
            assert!(!should_apply_spy_obstruction_fix(
                true,
                Some(camera_query),
                Some(muzzle_query)
            ));
            let blocked = should_block_confirmed_shot(true, Some(target), Some(muzzle_query));
            assert!(blocked);
            assert!(!dispatch_unobstructed(blocked, || panic!(
                "both-blocked confirmed shot escaped"
            )));
        }
    }

    #[test]
    fn firing_without_a_confirmed_target_or_valid_wall_stays_native() {
        let target = Some(vector(650.0, 0.0, 0.0));
        let wall = Some(ObstructionQueryResult {
            blocked: true,
            hit_position: Some(vector(250.0, 0.0, 0.0)),
        });
        assert!(should_block_confirmed_shot(true, target, wall));
        assert!(!should_block_confirmed_shot(false, target, wall));
        assert!(!should_block_confirmed_shot(true, None, wall));
        assert!(!should_block_confirmed_shot(
            true,
            Some(vector(f32::NAN, 0.0, 0.0)),
            wall
        ));
        for query in [
            None,
            Some(ObstructionQueryResult {
                blocked: false,
                hit_position: None,
            }),
            Some(ObstructionQueryResult {
                blocked: true,
                hit_position: None,
            }),
            Some(ObstructionQueryResult {
                blocked: true,
                hit_position: Some(vector(f32::NAN, 0.0, 0.0)),
            }),
        ] {
            assert!(!should_block_confirmed_shot(true, target, query));
        }
    }

    #[test]
    fn scenery_at_or_beyond_victim_is_not_a_valid_muzzle_obstruction() {
        let camera = vector(0.0, 0.0, 0.0);
        let muzzle = vector(160.0, 0.0, 0.0);
        let target = vector(650.0, 0.0, 0.0);
        for wall in [650.0, 750.0, 2000.0] {
            let queries = sightline_queries_with(
                |from, _| {
                    Some(if from.x == 0.0 {
                        ObstructionQueryResult {
                            blocked: false,
                            hit_position: None,
                        }
                    } else {
                        ObstructionQueryResult {
                            blocked: true,
                            hit_position: Some(vector(wall, 0.0, 0.0)),
                        }
                    })
                },
                camera,
                muzzle,
                target,
            );
            assert!(queries.is_none());
            assert!(!should_block_confirmed_shot(
                true,
                Some(target),
                queries.map(|(_, muzzle, _)| muzzle)
            ));
        }
    }

    unsafe extern "thiscall" fn shot_sweep_fixture(
        owner: *mut c_void,
        hit: *mut u8,
        filter: usize,
        end: *const FVector,
        start: *const FVector,
        mask: usize,
        material: usize,
        extent: usize,
    ) -> u8 {
        assert_eq!(owner as usize, 1);
        assert_eq!(filter, 12);
        assert_eq!(mask, SHOT_QUERY_MASK);
        assert_eq!(material, OBSTRUCTION_QUERY_MATERIAL);
        let sweep = unsafe { (extent as *const FVector).read() };
        assert_eq!(sweep.y, 0.0);
        assert_eq!(sweep.x, sweep.z);
        let from = unsafe { start.read() };
        let to = unsafe { end.read() };
        assert!(to.x > from.x);
        // A shot can touch the merc's hit volume off the center line; a
        // zero-extent center query deliberately misses this fixture.
        if sweep.x < 6.0 {
            return 1;
        }
        unsafe {
            hit.add(8).cast::<FVector>().write_unaligned(vector(
                from.x + (to.x - from.x) * 0.4,
                0.0,
                6.0,
            ));
        }
        0
    }

    #[test]
    fn off_center_native_shot_volume_blocks_without_a_center_merc() {
        let context = QueryContext {
            timestamp_ms: 1000,
            query_owner: 1,
            query_filter: 12,
        };
        let camera = vector(0.0, 0.0, 0.0);
        let end = vector(1000.0, 0.0, 0.0);
        let sample = AimSample {
            pawn: 12,
            weapon: 24,
            context,
            origin: camera,
            target: end,
            merc_target: None,
        };
        assert!(confirmed_local_sample(
            sample,
            Some((12, 1000)),
            12,
            24,
            1000
        ));
        let hit = shot_merc_target_with(shot_sweep_fixture, context, camera, end, 8.0, |result| {
            read_vector_at(result, 8)
        })
        .unwrap();
        let (camera_query, muzzle_query, _) = sightline_queries_with(
            |from, to| {
                Some(if from.y == 0.0 {
                    ObstructionQueryResult {
                        blocked: false,
                        hit_position: None,
                    }
                } else {
                    ObstructionQueryResult {
                        blocked: true,
                        hit_position: Some(vector(
                            from.x + (to.x - from.x) * 0.1,
                            from.y + (to.y - from.y) * 0.1,
                            from.z + (to.z - from.z) * 0.1,
                        )),
                    }
                })
            },
            camera,
            vector(0.0, 20.0, 0.0),
            hit,
        )
        .unwrap();
        assert!(!camera_query.blocked);
        let blocked = should_block_confirmed_shot(true, Some(hit), Some(muzzle_query));
        assert!(blocked);
        assert!(!dispatch_unobstructed(blocked, || panic!(
            "off-center obstructed shot submitted"
        )));
        // No HUD result was invented; X still requires its own primary hit.
        assert!(sample.merc_target.is_none());
    }

    #[test]
    fn actual_shot_query_preserves_extent_and_does_not_invent_missing_hits() {
        let context = QueryContext {
            timestamp_ms: 0,
            query_owner: 1,
            query_filter: 12,
        };
        let camera = vector(0.0, 0.0, 0.0);
        let end = vector(1000.0, 0.0, 0.0);
        assert!(
            shot_merc_target_with(shot_sweep_fixture, context, camera, end, 0.0, |_| panic!(
                "miss resolved as hit"
            ))
            .is_none()
        );
        assert!(
            shot_merc_target_with(shot_sweep_fixture, context, camera, end, 8.0, |_| None)
                .is_none()
        );
        assert!(
            shot_merc_target_with(shot_sweep_fixture, context, camera, end, 8.0, |_| Some(
                vector(400.0, 0.0, 6.0)
            ))
            .is_some()
        );
        for extent in [-1.0, f32::NAN, f32::INFINITY] {
            assert!(shot_merc_target_with(
                shot_sweep_fixture,
                context,
                camera,
                end,
                extent,
                |_| panic!("invalid extent queried")
            )
            .is_none());
        }
    }

    #[test]
    fn shot_sweep_validation_accepts_volume_but_not_unrelated_geometry() {
        let camera = vector(0.0, 0.0, 0.0);
        let end = vector(1000.0, 0.0, 0.0);
        let extent = vector(8.0, 0.0, 8.0);
        assert!(hit_within_shot_sweep(
            camera,
            end,
            vector(400.0, 0.0, 6.0),
            extent
        ));
        for hit in [
            vector(-1.0, 0.0, 0.0),
            vector(1001.0, 0.0, 0.0),
            vector(400.0, 200.0, 0.0),
            vector(f32::NAN, 0.0, 0.0),
        ] {
            assert!(!hit_within_shot_sweep(camera, end, hit, extent));
        }
        assert!(!hit_within_shot_sweep(
            camera,
            end,
            vector(400.0, 0.0, 6.0),
            vector(0.0, 0.0, 0.0)
        ));
    }

    #[test]
    fn clear_off_center_shot_does_not_block_on_scenery_behind_victim() {
        let result = sightline_queries_with(
            |_, to| {
                assert!(to.x < 400.0);
                Some(ObstructionQueryResult {
                    blocked: false,
                    hit_position: None,
                })
            },
            vector(0.0, 0.0, 0.0),
            vector(0.0, 20.0, 0.0),
            vector(400.0, 0.0, 6.0),
        )
        .unwrap();
        let blocked = should_block_confirmed_shot(true, Some(result.2), Some(result.1));
        assert!(!blocked);
        let mut calls = 0;
        assert!(dispatch_unobstructed(blocked, || calls += 1));
        assert_eq!(calls, 1);
    }

    #[test]
    fn idle_dot_scope_rejects_merc_singleplayer_and_mixed_roles() {
        let mut pawn = [0usize; 240];
        let mut owner = [0usize; 200];
        let mut hud = [0usize; 180];
        owner[0x310 / 4] = pawn.as_ptr() as usize;
        hud[0x25c / 4] = owner.as_ptr() as usize;
        for (flags, expected) in [
            (PAWN_IS_SPY, true),
            (PAWN_IS_MERC, false),
            (0, false),
            (PAWN_IS_SPY | PAWN_IS_MERC, false),
        ] {
            pawn[PAWN_GAMEPLAY_FLAGS_OFFSET / 4] = flags as usize;
            std::hint::black_box(&pawn);
            assert_eq!(spy_hud(hud.as_ptr().cast()), expected);
        }
    }

    #[test]
    fn hitting_a_player_is_not_classified_as_a_muzzle_wall() {
        unsafe extern "thiscall" fn player_query(
            _owner: *mut c_void,
            hit: *mut u8,
            _filter: usize,
            _end: *const FVector,
            _start: *const FVector,
            _mask: usize,
            _material: usize,
            _extent: usize,
        ) -> u8 {
            unsafe {
                hit.add(4).cast::<usize>().write_unaligned(1);
                hit.add(8)
                    .cast::<FVector>()
                    .write_unaligned(vector(20.0, 0.0, 0.0));
            }
            0
        }
        let context = QueryContext {
            timestamp_ms: 0,
            query_owner: 1,
            query_filter: 2,
        };
        let from = vector(0.0, 0.0, 0.0);
        let to = vector(100.0, 0.0, 0.0);
        assert!(
            !query_obstruction_with(player_query, context, from, to, |_| true)
                .unwrap()
                .blocked
        );
        assert!(
            query_obstruction_with(player_query, context, from, to, |_| false)
                .unwrap()
                .blocked
        );
    }

    #[test]
    fn native_visibility_controls_idle_dot_and_restores_weapon_reticle() {
        assert_eq!(
            [false, false, true, true, false].map(|visible| idle_reticle_kind(
                4,
                true,
                true,
                Some(visible)
            )),
            [3, 3, 4, 4, 3]
        );
        for kind in 0..12 {
            assert_eq!(idle_reticle_kind(kind, true, true, Some(true)), kind);
            assert_eq!(idle_reticle_kind(kind, false, true, Some(false)), kind);
            assert_eq!(idle_reticle_kind(kind, true, false, Some(false)), kind);
            assert_eq!(idle_reticle_kind(kind, true, true, None), kind);
        }
        for kind in [1, 2, 8, 10, 11] {
            assert_eq!(idle_reticle_kind(kind, true, true, Some(false)), kind);
        }
        assert!(!should_show_idle_dot(true, true, Some(3), true, false));
        assert!(!should_show_idle_dot(false, true, Some(3), true, true));
        assert!(should_show_idle_dot(true, true, Some(3), true, true));
    }

    #[test]
    fn white_override_only_changes_our_dot_and_obstruction_x() {
        assert!(force_white_reticle(true, Some(10), true, false, true));
        assert!(force_white_reticle(true, Some(3), true, true, false));
        for kind in 0..12 {
            assert!(!force_white_reticle(true, Some(kind), true, false, false));
            assert!(!force_white_reticle(false, Some(kind), true, true, true));
            assert!(!force_white_reticle(true, Some(kind), false, true, true));
            if kind != 3 && kind != 10 {
                assert!(!force_white_reticle(true, Some(kind), true, true, true));
            }
        }
        assert!(!force_white_reticle(true, None, true, true, true));
    }

    #[test]
    fn delayed_native_aim_and_key_release_cannot_race_dot_handoff() {
        // (mouse state, real native visibility): input changes early/late,
        // while the decision follows only the game's displayed reticle.
        let frames = [
            (false, false),
            (true, false),
            (true, true),
            (false, true),
            (false, false),
        ];
        let selected =
            frames.map(|(_mouse, visible)| idle_reticle_kind(4, true, true, Some(visible)));
        assert_eq!(selected, [3, 3, 4, 4, 3]);
        for (kind, show) in selected.into_iter().zip([true, true, false, false, true]) {
            assert_eq!(
                should_show_idle_dot(true, true, Some(kind as u32), kind == 3, show),
                show
            );
        }
    }

    #[test]
    fn scenery_behind_merc_cannot_cancel_a_clear_shot() {
        let mut calls = Vec::new();
        let result = sightline_queries_with(
            |from, to| {
                calls.push(to.x);
                // The old maximum-range test saw unequal distant surfaces.
                Some(ObstructionQueryResult {
                    blocked: to.x >= 750.0,
                    hit_position: (to.x >= 750.0).then_some(vector(
                        if from.y == 0.0 { 800.0 } else { 750.0 },
                        from.y,
                        0.0,
                    )),
                })
            },
            vector(0.0, 0.0, 0.0),
            vector(10.0, 10.0, 0.0),
            vector(400.0, 0.0, 0.0),
        )
        .unwrap();
        assert!(calls.iter().all(|x| *x < 400.0));
        assert!(!should_apply_spy_obstruction_fix(
            true,
            Some(result.0),
            Some(result.1)
        ));
        assert!(dispatch_unobstructed(false, || {}));
    }

    #[test]
    fn near_muzzle_door_still_blocks_before_confirmed_merc() {
        for direction in [1.0, -1.0] {
            let result = sightline_queries_with(
                |from, to| {
                    Some(if from.y == 0.0 {
                        ObstructionQueryResult {
                            blocked: false,
                            hit_position: None,
                        }
                    } else {
                        ObstructionQueryResult {
                            blocked: true,
                            hit_position: Some(vector(
                                from.x + (to.x - from.x) * 0.1,
                                from.y + (to.y - from.y) * 0.1,
                                0.0,
                            )),
                        }
                    })
                },
                vector(0.0, 0.0, 0.0),
                vector(10.0 * direction, 10.0, 0.0),
                vector(400.0 * direction, 0.0, 0.0),
            )
            .unwrap();
            let blocked = should_apply_spy_obstruction_fix(true, Some(result.0), Some(result.1));
            assert!(blocked);
            assert!(!dispatch_unobstructed(blocked, || panic!(
                "blocked shot submitted"
            )));
        }
    }

    #[test]
    fn unequal_camera_and_muzzle_collisions_are_not_proof_of_blockage() {
        let hit = |x| {
            Some(ObstructionQueryResult {
                blocked: true,
                hit_position: Some(vector(x, 0.0, 0.0)),
            })
        };
        for (a, b) in [(800.0, 750.0), (650.0, 190.0), (800.0, 799.0)] {
            assert!(!should_apply_spy_obstruction_fix(true, hit(a), hit(b)));
        }
    }

    #[test]
    fn unknown_queries_do_not_cancel_damage() {
        for hit in [
            None,
            Some(vector(f32::NAN, 0.0, 0.0)),
            Some(vector(-10.0, 0.0, 0.0)),
            Some(vector(1001.0, 0.0, 0.0)),
        ] {
            assert!(sightline_queries_with(
                |_, _| Some(ObstructionQueryResult {
                    blocked: true,
                    hit_position: hit
                }),
                vector(0.0, 0.0, 0.0),
                vector(0.0, 10.0, 0.0),
                vector(1000.0, 0.0, 0.0)
            )
            .is_none());
        }
        assert!(sightline_queries_with(
            |_, _| None,
            vector(0.0, 0.0, 0.0),
            vector(0.0, 10.0, 0.0),
            vector(1000.0, 0.0, 0.0)
        )
        .is_none());
    }

    #[test]
    fn segment_validation_accepts_near_wall_but_rejects_out_of_segment_hits() {
        let from = vector(100.0, 10.0, 0.0);
        let to = vector(200.0, 10.0, 0.0);
        for x in [100.0, 100.1, 150.0, 200.0] {
            assert!(point_on_segment(from, to, vector(x, 10.0, 0.0)));
        }
        for point in [
            vector(99.0, 10.0, 0.0),
            vector(201.0, 10.0, 0.0),
            vector(150.0, 20.0, 0.0),
            vector(f32::NAN, 10.0, 0.0),
        ] {
            assert!(!point_on_segment(from, to, point));
        }
    }

    #[test]
    fn replicated_stale_or_other_weapon_aim_cannot_cancel_native_damage() {
        let sample = AimSample {
            pawn: 12,
            weapon: 24,
            context: QueryContext {
                timestamp_ms: 1000,
                query_owner: 1,
                query_filter: 12,
            },
            origin: vector(0.0, 0.0, 0.0),
            target: vector(1000.0, 0.0, 0.0),
            merc_target: Some(vector(400.0, 0.0, 0.0)),
        };
        assert!(confirmed_local_sample(
            sample,
            Some((12, 1000)),
            12,
            24,
            1100
        ));
        for (observed, pawn, weapon, now) in [
            (None, 12, 24, 1000),
            (Some((13, 1000)), 12, 24, 1000),
            (Some((12, 1000)), 12, 25, 1000),
            (Some((12, 1000)), 12, 24, 1101),
            (Some((12, 1000)), 12, 24, 999),
            (Some((12, 800)), 12, 24, 1000),
        ] {
            assert!(!confirmed_local_sample(sample, observed, pawn, weapon, now));
        }
        // HUD need not detect a merc for this local shot's own query to run.
        assert!(confirmed_local_sample(
            AimSample {
                merc_target: None,
                ..sample
            },
            Some((12, 1000)),
            12,
            24,
            1000
        ));
    }

    #[test]
    fn confirmed_obstruction_submits_zero_native_damage_calls() {
        let mut submissions = 0;
        for _ in 0..128 {
            assert!(!dispatch_unobstructed(true, || submissions += 1));
        }
        assert_eq!(submissions, 0);
        assert!(dispatch_unobstructed(false, || submissions += 1));
        assert_eq!(submissions, 1);
    }

    #[test]
    fn native_merc_gate_rejects_scenery_spies_and_unknown_classes() {
        let root = [0usize; 12];
        let mut derived = [0usize; 12];
        derived[0x28 / 4] = root.as_ptr() as usize;
        let mut actor = [0usize; 240];
        actor[0x24 / 4] = derived.as_ptr() as usize;
        actor[PAWN_GAMEPLAY_FLAGS_OFFSET / 4] = PAWN_IS_MERC as usize;
        let mut result = [0u32; 18];
        result[1] = actor.as_ptr() as usize as u32;
        result[2] = 100.0f32.to_bits();
        assert_eq!(
            primary_merc_target_for_class(result.as_ptr().cast(), root.as_ptr().cast())
                .unwrap()
                .x,
            100.0
        );
        actor[PAWN_GAMEPLAY_FLAGS_OFFSET / 4] = PAWN_IS_SPY as usize;
        assert!(
            primary_merc_target_for_class(result.as_ptr().cast(), root.as_ptr().cast()).is_none()
        );
        actor[PAWN_GAMEPLAY_FLAGS_OFFSET / 4] = PAWN_IS_MERC as usize;
        actor[0x24 / 4] = 0;
        std::hint::black_box(&actor);
        assert!(
            primary_merc_target_for_class(result.as_ptr().cast(), root.as_ptr().cast()).is_none()
        );
        assert!(primary_merc_target_for_class(result.as_ptr().cast(), std::ptr::null()).is_none());
        result[1] = 0;
        assert!(
            primary_merc_target_for_class(result.as_ptr().cast(), root.as_ptr().cast()).is_none()
        );
    }

    #[test]
    fn class_walk_is_bounded_for_cycles() {
        let expected = [0usize; 12];
        let mut cyclic = [0usize; 12];
        cyclic[0x28 / 4] = cyclic.as_ptr() as usize;
        assert!(!class_derives_from(
            cyclic.as_ptr().cast(),
            expected.as_ptr().cast()
        ));
    }

    #[test]
    fn idle_dot_only_overrides_the_selected_native_multiplayer_dot() {
        for active in [false, true] {
            for multiplayer in [false, true] {
                for same in [false, true] {
                    for kind in 0..12 {
                        assert_eq!(
                            should_show_idle_dot(active, multiplayer, Some(kind), same, true),
                            active && multiplayer && same && kind == 3
                        );
                    }
                }
            }
        }
        assert!(!should_show_idle_dot(true, true, None, true, false));
        // Aim, obstruction X, gadget, and native indestructible-light X untouched.
        for kind in [0, 1, 4, 5, 6, 7, 9, 10, 11] {
            assert!(!should_show_idle_dot(true, true, Some(kind), true, false));
        }
    }

    #[test]
    fn native_x_is_limited_to_gun_reticles_and_clears_without_a_shot() {
        for kind in 0..12 {
            assert_eq!(reticle_kind_for_obstruction(kind, false), kind);
            assert_eq!(
                reticle_kind_for_obstruction(kind, true),
                if matches!(kind, 0 | 4 | 5 | 6 | 7 | 9) {
                    10
                } else {
                    kind
                }
            );
        }
        assert_eq!(
            [false, true, true, false].map(|blocked| reticle_kind_for_obstruction(4, blocked)),
            [4, 10, 10, 4]
        );
    }

    #[test]
    fn native_x_does_not_reuse_other_pawns_weapons_or_stale_aim() {
        let sample = AimSample {
            pawn: 12,
            weapon: 24,
            context: QueryContext {
                timestamp_ms: 1000,
                query_owner: 1,
                query_filter: 12,
            },
            origin: vector(0.0, 0.0, 0.0),
            target: vector(100.0, 0.0, 0.0),
            merc_target: None,
        };
        assert!(aim_sample_is_current(sample, 12, 24, 1100));
        for (pawn, weapon, time) in [
            (13, 24, 1000),
            (12, 25, 1000),
            (12, 0, 1000),
            (12, 24, 1101),
            (12, 24, 999),
        ] {
            assert!(!aim_sample_is_current(sample, pawn, weapon, time));
        }
    }

    #[test]
    fn native_x_camera_target_stops_before_wall_in_both_directions() {
        let low = vector(0.0, 0.0, 0.0);
        let high = vector(100.0, 0.0, 0.0);
        assert_eq!(
            target_before_camera_hit(low, high, vector(60.0, 0.0, 0.0))
                .unwrap()
                .x,
            58.0
        );
        assert_eq!(
            target_before_camera_hit(high, low, vector(60.0, 0.0, 0.0))
                .unwrap()
                .x,
            62.0
        );
        for hit in [
            vector(-10.0, 0.0, 0.0),
            vector(101.0, 0.0, 0.0),
            vector(2.0, 0.0, 0.0),
            vector(30.0, 20.0, 0.0),
            vector(f32::NAN, 0.0, 0.0),
        ] {
            assert!(target_before_camera_hit(low, high, hit).is_none());
        }
    }

    #[test]
    fn native_aim_capture_restores_nested_scope_without_retaining_hit_lists() {
        AIM_CAPTURE.with(|slot| {
            assert!(slot.get().is_none());
            let previous = slot.replace(Some(None));
            let inner_previous = slot.replace(Some(None));
            assert!(slot.replace(inner_previous).is_some());
            assert!(slot.replace(previous).is_some());
            assert!(slot.get().is_none());
        });
    }

    fn vector(x: f32, y: f32, z: f32) -> FVector {
        FVector { x, y, z }
    }

    #[test]
    fn development_diagnostics_require_exact_build_opt_in() {
        assert!(diagnostic_opt_in(Some("1")));
        for value in [None, Some(""), Some("0"), Some("true"), Some(" 1")] {
            assert!(!diagnostic_opt_in(value));
        }
    }

    #[test]
    fn diagnostic_budget_is_bounded_even_under_concurrent_dispatch() {
        let count = std::sync::Arc::new(AtomicU32::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let count = count.clone();
                std::thread::spawn(move || {
                    (0..100)
                        .filter_map(|_| take_diagnostic_slot(&count, 64))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut slots: Vec<_> = threads
            .into_iter()
            .flat_map(|t| t.join().unwrap())
            .collect();
        slots.sort_unstable();
        assert_eq!(slots, (1..=64).collect::<Vec<_>>());
        assert_eq!(count.load(Ordering::Relaxed), 64);
        assert_eq!(take_diagnostic_slot(&count, 64), None);
        let max = AtomicU32::new(u32::MAX);
        assert_eq!(take_diagnostic_slot(&max, u32::MAX), None);
    }

    #[test]
    fn observation_preserves_exact_existing_context_expiration_boundary() {
        let context = QueryContext {
            timestamp_ms: 1000,
            query_owner: 1,
            query_filter: 2,
        };
        for now in [0, 999, 1000, 3499, 3500, 3501, 9000] {
            let result = context_snapshot(Some(context), now);
            let expected = now.saturating_sub(context.timestamp_ms) <= QUERY_CONTEXT_MAX_AGE_MS;
            assert_eq!(result.ready.is_some(), expected);
            assert_eq!(result.status == ContextStatus::Ready, expected);
            assert_eq!(result.age_ms, Some(now.saturating_sub(1000)));
        }
        assert_eq!(context_snapshot(None, 9000).status, ContextStatus::Missing);
    }

    #[test]
    fn telemetry_distinguishes_role_context_and_query_bypasses() {
        use PassthroughReason::*;
        assert_eq!(
            passthrough_reason(
                false,
                false,
                false,
                ContextStatus::NotApplicable,
                None,
                None
            ),
            PawnUnavailable
        );
        assert_eq!(
            passthrough_reason(true, false, false, ContextStatus::NotApplicable, None, None),
            ActorMismatch
        );
        assert_eq!(
            passthrough_reason(true, true, false, ContextStatus::NotApplicable, None, None),
            NotSpy
        );
        for (status, expected) in [
            (ContextStatus::Missing, ContextMissing),
            (ContextStatus::Expired, ContextExpired),
            (ContextStatus::Unavailable, ContextUnavailable),
        ] {
            assert_eq!(
                passthrough_reason(true, true, true, status, None, None),
                expected
            );
        }
        assert_eq!(
            passthrough_reason(true, true, true, ContextStatus::Ready, None, Some(true)),
            InvalidQuery
        );
        assert_eq!(
            passthrough_reason(
                true,
                true,
                true,
                ContextStatus::Ready,
                Some(true),
                Some(true)
            ),
            BothRaysBlocked
        );
        assert_eq!(
            passthrough_reason(
                true,
                true,
                true,
                ContextStatus::Ready,
                Some(true),
                Some(false)
            ),
            CameraBlocked
        );
        assert_eq!(
            passthrough_reason(
                true,
                true,
                true,
                ContextStatus::Ready,
                Some(false),
                Some(false)
            ),
            MuzzleClear
        );
    }

    #[test]
    fn all_ray_decisions_retain_the_existing_gameplay_truth_table() {
        for spy in [false, true] {
            for camera in [None, Some(false), Some(true)] {
                for muzzle in [None, Some(false), Some(true)] {
                    let query = |blocked| ObstructionQueryResult {
                        blocked,
                        hit_position: None,
                    };
                    let before = spy && camera == Some(false) && muzzle == Some(true);
                    assert_eq!(
                        should_apply_spy_obstruction_fix(spy, camera.map(query), muzzle.map(query)),
                        before
                    );
                }
            }
        }
    }

    #[test]
    fn diagnostic_distances_do_not_emit_non_finite_values() {
        assert_eq!(
            finite_distance(Some(vector(0.0, 0.0, 0.0)), Some(vector(3.0, 4.0, 0.0))),
            Some(5.0)
        );
        assert_eq!(finite_distance(None, Some(vector(0.0, 0.0, 0.0))), None);
        assert_eq!(
            finite_distance(
                Some(vector(f32::NAN, 0.0, 0.0)),
                Some(vector(0.0, 0.0, 0.0))
            ),
            None
        );
    }

    #[test]
    fn context_is_matched_by_actor_and_expires() {
        let actor = 0x1234;
        remember_query_context(
            actor,
            QueryContext {
                timestamp_ms: 1_000,
                query_owner: 1,
                query_filter: actor,
            },
        );
        assert!(query_context_for(actor, 1_100).is_some());
        assert!(query_context_for(actor + 1, 1_100).is_none());
        assert!(query_context_for(actor, 4_000).is_none());
    }

    #[test]
    fn only_camera_clear_and_muzzle_blocked_spy_shots_are_fixed() {
        let clear = Some(ObstructionQueryResult {
            blocked: false,
            hit_position: None,
        });
        let blocked = Some(ObstructionQueryResult {
            blocked: true,
            hit_position: None,
        });

        assert!(should_apply_spy_obstruction_fix(true, clear, blocked));
        assert!(!should_apply_spy_obstruction_fix(true, blocked, blocked));
        assert!(!should_apply_spy_obstruction_fix(true, clear, clear));
        assert!(!should_apply_spy_obstruction_fix(false, clear, blocked));
        assert!(!should_apply_spy_obstruction_fix(true, None, blocked));
        assert!(!should_apply_spy_obstruction_fix(true, clear, None));
        assert_eq!(
            distance_squared(vector(0.0, 0.0, 0.0), vector(3.0, 4.0, 0.0)),
            25.0
        );
    }

    #[test]
    fn retail_pawn_role_bits_scope_the_fix_to_spies() {
        assert_eq!(pawn_roles(Some(PAWN_IS_SPY)), (true, false));
        assert_eq!(pawn_roles(Some(PAWN_IS_MERC)), (false, true));
        assert_eq!(pawn_roles(Some(0)), (false, false));
        assert_eq!(pawn_roles(None), (false, false));
    }

    // Native 0x6CA4E0 builds hit = second + (first - second) * fraction.
    // This fixture checks the real FFI call boundary, not variable names.
    unsafe extern "thiscall" fn quarter_hit_query(
        _owner: *mut c_void,
        hit: *mut u8,
        _filter: usize,
        end: *const FVector,
        start: *const FVector,
        _mask: usize,
        _material: usize,
        _extent: usize,
    ) -> u8 {
        let start = unsafe { start.read_unaligned() };
        let end = unsafe { end.read_unaligned() };
        let point = FVector {
            x: start.x + (end.x - start.x) * 0.25,
            y: start.y + (end.y - start.y) * 0.25,
            z: start.z + (end.z - start.z) * 0.25,
        };
        unsafe { hit.add(8).cast::<FVector>().write_unaligned(point) };
        0
    }

    #[test]
    fn filtered_query_native_abi_travels_from_origin_to_target() {
        assert_eq!(std::mem::align_of::<NativeHitBuffer>(), 16);
        let context = QueryContext {
            timestamp_ms: 0,
            query_owner: 1,
            query_filter: 2,
        };
        for (origin, target) in [
            (vector(0.0, 2.0, 4.0), vector(100.0, 42.0, 24.0)),
            (vector(100.0, 42.0, 24.0), vector(0.0, 2.0, 4.0)),
        ] {
            let result =
                query_obstruction_with(quarter_hit_query, context, origin, target, |_| false)
                    .unwrap();
            assert!(result.blocked);
            let expected = vector(
                origin.x + (target.x - origin.x) * 0.25,
                origin.y + (target.y - origin.y) * 0.25,
                origin.z + (target.z - origin.z) * 0.25,
            );
            assert_eq!(
                distance_squared(result.hit_position.unwrap(), expected),
                0.0
            );
        }
    }
}
