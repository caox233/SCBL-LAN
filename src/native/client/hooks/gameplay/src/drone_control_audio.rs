//! C22: switch retained owner drones from operator loops to native spatial loops.
//! The MP exit normally falls/deactivates the body (B0F3B0/B0EE80), which stops
//! +B74/+B8C/+BB8. A voluntary park intentionally skips those lifecycle calls.
//! Intercept ONLY ADEReconDrone's PlaySound virtual slot; never global audio.
//! Retail names were read from live drones on 2026-10-02. Only the owner-side
//! retained body gets a managed spatial loop; remote/native 3D calls, one-shots,
//! attenuation and gameplay remain unchanged. Stop the managed loop on reentry/end.
#[path = "drone_spy_audio.rs"]
mod spy_gain;
use crate::equipment_native::{float, readable, signature, word};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
const VT: u32 = 0x02a88f54;
const PLAY: usize = 0x00544000;
const SLOT: usize = VT as usize + 0x368;
type Play = unsafe extern "thiscall" fn(usize, usize, u32, f32, u32, u32) -> u32;
const FIELDS: [(usize, usize, &str); 3] = [
    (0xb70, 0xb74, "Loop"),
    (0xb88, 0xb8c, "DamagedLoop"),
    (0xbb4, 0xbb8, "OutOfRange"),
];
static READY: AtomicBool = AtomicBool::new(false);
static BANK: Mutex<Bank> = Mutex::new(Bank {
    records: BTreeMap::new(),
});
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    owner: u32,
    source: [u32; 2],
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Request {
    event: usize,
    slot: u32,
    scalar: f32,
    a: u32,
    b: u32,
}
#[derive(Clone)]
struct WorldEdge {
    start: bool,
    token: std::sync::Arc<()>,
}
struct Record {
    id: Identity,
    parked: bool,
    desired: [Option<Request>; 3],
    world: [bool; 2],
    world_tokens: [Option<std::sync::Arc<()>>; 2],
    world_retry_at: f32,
    world_observed_time: Option<f32>,
}
impl Record {
    fn new(id: Identity, desired: [Option<Request>; 3]) -> Self {
        Self {
            id,
            parked: false,
            desired,
            world: [false; 2],
            world_tokens: [None, None],
            world_retry_at: 0.,
            world_observed_time: None,
        }
    }
}
#[derive(Default)]
struct Bank {
    records: BTreeMap<usize, Record>,
}
impl Bank {
    // Native ControlStart has already called body.Activate(true). Its initial
    // Loop_Player can precede stable actor/network identity and evade play().
    // Remember that proven native start at the stable control boundary; do NOT
    // submit a second Play event or overwrite known damage/range/stop history.
    fn remember_native_start(&mut self, body: usize, id: Identity, normal: Request) -> bool {
        if self.records.get(&body).is_some_and(|r| r.id == id) {
            return false;
        }
        if !self.records.contains_key(&body) && self.records.len() >= 32 {
            return false;
        }
        self.records.insert(
            body,
            Record::new(id, [Some(normal), None, None]),
        );
        true
    }
    // true means forward the entire native call and its return value unchanged.
    fn event(&mut self, body: usize, id: Identity, kind: usize, start: bool, r: Request) -> bool {
        if !self.records.contains_key(&body) && self.records.len() >= 32 {
            return true;
        }
        let record = self.records.entry(body).or_insert(Record::new(id, [None; 3]));
        if record.id != id {
            *record = Record::new(id, [None; 3]);
        }
        record.desired[kind] = start.then_some(r);
        !(start && record.parked)
    }
    fn park(&mut self, body: usize, id: Identity) -> bool {
        // C20 incorrectly required a captured start before permitting Stops.
        // A committed, validated park must stop canonical Player loops even if
        // the early Play event was not observed. Stops do not require playback
        // history in native B0F3B0/B0EE80 either. Never invent a restart here.
        if !self.records.contains_key(&body) && self.records.len() >= 32 {
            return false;
        }
        let r = self.records.entry(body).or_insert(Record::new(id, [None; 3]));
        if r.id != id {
            *r = Record::new(id, [None; 3]);
        }
        if r.parked {
            return false;
        }
        r.parked = true;
        true
    }
    // Edges only: never restart a spatial loop every frame or on repeated park.
    // OutOfRange is a controller warning, not a world sound.
    fn world_edges(&mut self, body: usize, id: Identity, enable: bool) -> [Option<WorldEdge>; 2] {
        let Some(r) = self.records.get_mut(&body).filter(|r| r.id == id) else {
            return [None, None];
        };
        std::array::from_fn(|i| {
            let wanted = enable && r.parked && r.desired[i].is_some();
            if r.world[i] == wanted {
                None
            } else {
                r.world[i] = wanted;
                let token = std::sync::Arc::new(());
                r.world_tokens[i] = Some(token.clone());
                Some(WorldEdge { start: wanted, token })
            }
        })
    }
    // The native submission runs outside the lock and can reenter the lifecycle.
    // Arc identity remains unique while the callback holds its ticket, even if
    // the map is cleared and the same body/source is immediately reused.
    fn world_current(&self, body: usize, id: Identity, kind: usize, edge: &WorldEdge) -> bool {
        self.records.get(&body).is_some_and(|r| {
            r.id == id && kind < 2 && r.world[kind] == edge.start
                && r.world_tokens[kind].as_ref()
                    .is_some_and(|token| std::sync::Arc::ptr_eq(token, &edge.token))
                && (!edge.start || (r.parked && r.desired[kind].is_some()))
        })
    }
    fn world_rejected(&mut self, body: usize, id: Identity, kind: usize, edge: &WorldEdge) {
        if self.world_current(body, id, kind, edge) {
            let r = self.records.get_mut(&body).unwrap();
            r.world[kind] = !edge.start;
            r.world_tokens[kind] = None;
        }
    }
    fn world_retry_due(&mut self, body: usize, id: Identity, now: f32) -> bool {
        let Some(r) = self
            .records
            .get_mut(&body)
            .filter(|r| r.id == id && r.parked)
        else {
            return false;
        };
        if !now.is_finite() {
            return false;
        }
        if r.world_observed_time.is_some_and(|last| now < last) {
            r.world_retry_at = now;
        }
        r.world_observed_time = Some(now);
        if now < r.world_retry_at
            || !(0..2).any(|i| r.desired[i].is_some() != r.world[i])
        {
            return false;
        }
        r.world_retry_at = now + 0.5;
        true
    }
    fn resume(&mut self, body: usize, id: Identity) -> [Option<Request>; 3] {
        let Some(r) = self
            .records
            .get_mut(&body)
            .filter(|r| r.id == id && r.parked)
        else {
            return [None; 3];
        };
        r.parked = false;
        r.desired
    }
}
unsafe fn source_identity(body: usize) -> Option<Identity> {
    if word(body, 0)? != VT || (word(body, 0x4c)? >> 8) & 255 != 1 {
        return None;
    }
    let owner = word(body, 0x9fc)?;
    if word(body, 0x8c)? != owner || word(owner as usize, 0)? != 0x2ad2abc {
        return None;
    }
    let node = word(body, 0x2c0)? as usize;
    let source = [word(node, 0x70)?, word(node, 0x74)?];
    (source != [0, 0]).then_some(Identity { owner, source })
}
unsafe fn identity(body: usize) -> Option<Identity> {
    // Initial native Play may precede state2/owner binding; C21's stable
    // ControlStart observation remains necessary and is unchanged.
    if !matches!(word(body, 0x910)? & 255, 0..=2)
        || word(body, 0x5dc)? as i32 <= 0
        || word(body, 0x4b0)? & 0x80000000 != 0
    {
        return None;
    }
    source_identity(body)
}
unsafe fn named(event: usize, expected: &str) -> bool {
    if word(event, 0) != Some(0x29d5cb4) {
        return false;
    }
    let Some(id) = word(event, 0x20).filter(|id| id >> 19 == 0) else {
        return false;
    };
    let Some(table) = word(0x333cd60, 0) else {
        return false;
    };
    let Some(entry) = word(table as usize, (id as usize & 0x3ffff) * 4) else {
        return false;
    };
    let p = entry as usize + 8;
    readable(p, expected.len() + 1)
        && std::slice::from_raw_parts(p as *const u8, expected.len()) == expected.as_bytes()
        && *(p as *const u8).add(expected.len()) == 0
}
unsafe fn classify(body: usize, event: usize) -> Option<(usize, bool)> {
    for (i, &(start, stop, suffix)) in FIELDS.iter().enumerate() {
        for (offset, active, verb) in [(start, true, "Play"), (stop, false, "Stop")] {
            if word(body, offset) == Some(event as u32)
                && named(
                    event,
                    &format!("{verb}_SFX_ADV_Gadget_Drone_{suffix}_Player"),
                )
            {
                return Some((i, active));
            }
        }
    }
    None
}
unsafe fn original(body: usize, r: Request) -> u32 {
    let play: Play = std::mem::transmute(PLAY);
    spy_gain::with_request(body, r.event, r.slot, || play(body, r.event, r.slot, r.scalar, r.a, r.b))
}
const WORLD_FIELDS: [(usize, usize, &str); 2] =
    [(0xb94, 0xb98, "Loop"), (0xbac, 0xbb0, "DamagedLoop")];
// A96E71..A96E94 converts slot0 to nonspatial7 for this owner's captured
// flags6C (bit4000 clear). Slot1 enters the native spatial path without
// changing actor flags, assets, attenuation or volume. Native type2 tracks
// actor position, unlike the Player/type1 listener-relative channel.
const WORLD_SLOT: u32 = 1;
unsafe fn world_request(body: usize, kind: usize, start: bool) -> Option<Request> {
    let &(play, stop, suffix) = WORLD_FIELDS.get(kind)?;
    // Validate BOTH events before starting a loop we could not later stop.
    let p = word(body, play)? as usize;
    let q = word(body, stop)? as usize;
    if !named(p, &format!("Play_SFX_ADV_Gadget_Drone_{suffix}_3D"))
        || !named(q, &format!("Stop_SFX_ADV_Gadget_Drone_{suffix}_3D"))
        || word(body, 0x70)? & 0x80000000 != 0
    {
        return None;
    }
    Some(Request {
        event: if start { p } else { q },
        slot: WORLD_SLOT,
        scalar: 0.,
        a: 0,
        b: 0,
    })
}
unsafe fn sync_world(body: usize, id: Identity, enabled: bool) {
    if source_identity(body) != Some(id) {
        return;
    }
    let requests: Option<Vec<_>> = (0..2)
        .map(|i| {
            Some([
                world_request(body, i, false)?,
                world_request(body, i, true)?,
            ])
        })
        .collect();
    let Some(requests) = requests else {
        return;
    };
    let edges = BANK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .world_edges(body, id, enabled);
    for (i, edge) in edges.into_iter().enumerate() {
        if let Some(edge) = edge {
            // An earlier native call in this batch may cancel/resume/replace the
            // body. Revalidate every remaining edge, not just the initial batch.
            if source_identity(body) != Some(id) || !BANK.lock()
                .unwrap_or_else(|e| e.into_inner()).world_current(body, id, i, &edge)
            {
                continue;
            }
            let start = edge.start;
            let accepted = original(body, requests[i][start as usize]);
            if accepted == 0 {
                BANK.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .world_rejected(body, id, i, &edge);
            }
            // This is a native submission result, not proof of audible output.
            tracing::info!("SCBL_MOD_DRONE_AUDIO candidate=22 owner_world_kind={} start={} native_result={} spatial_slot=1", i, start, accepted);
        }
    }
}
// Existing ability Tick only retries a rejected managed loop at 2Hz. The
// native spatial distance gate can reject a start far away; approaching later
// must not leave the owner permanently silent. No scene scan or remote repair.
pub(super) unsafe fn tick(ability: usize) {
    if !READY.load(Ordering::Acquire) || word(ability, 0) != Some(0x2a3f14c) {
        return;
    }
    let Some(body) = word(ability, 0xa0).map(|v| v as usize) else {
        return;
    };
    let Some(id) = identity(body) else {
        return;
    };
    if word(ability, 0x6c) != Some(id.owner) || word(body, 0x910).map(|v| v & 255) != Some(1) {
        return;
    }
    let Some(now) = word(body, 0x144).and_then(|p| float(p as usize, 0x5a0)) else {
        return;
    };
    let due = BANK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .world_retry_due(body, id, now);
    if due {
        sync_world(body, id, true);
    }
}
unsafe extern "thiscall" fn play(
    body: usize,
    event: usize,
    slot: u32,
    scalar: f32,
    a: u32,
    b: u32,
) -> u32 {
    let r = Request {
        event,
        slot,
        scalar,
        a,
        b,
    };
    if !READY.load(Ordering::Acquire) {
        return original(body, r);
    }
    let Some(id) = identity(body) else {
        // A lethal hit may reach native Stops with HP0 before state3. Release
        // only our matching managed loop; never reuse a stale actor identity.
        if let Some(id) = source_identity(body) {
            sync_world(body, id, false);
        }
        BANK.lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .remove(&body);
        return original(body, r);
    };
    if let Some((kind, start)) = classify(body, event) {
        let forward = BANK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .event(body, id, kind, start, r);
        // Native normal->damaged and Stop transitions also update the parked
        // world loop. Native Player calls remain suppressed only while parked.
        sync_world(body, id, true);
        if !forward {
            return 0;
        } // same "no event submitted" result as native 544096
    }
    original(body, r)
}
pub(super) unsafe fn park(body: usize) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let Some(id) = identity(body) else {
        return;
    };
    if word(body, 0x910).map(|v| v & 255) != Some(1) {
        return;
    }
    let mut stops = Vec::new();
    for &(_, stop, suffix) in &FIELDS {
        let Some(event) = word(body, stop).map(|v| v as usize) else {
            return;
        };
        if !named(event, &format!("Stop_SFX_ADV_Gadget_Drone_{suffix}_Player")) {
            return;
        }
        stops.push(Request {
            event,
            slot: 7,
            scalar: 0.,
            a: 0,
            b: 0,
        });
    }
    if BANK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .park(body, id)
    {
        // Call original, not the interceptor: these presentation-only stops must
        // not forget the native desired loop to resume on the SAME live body.
        for r in stops {
            original(body, r);
        }
        sync_world(body, id, true);
        tracing::info!(
            "SCBL_MOD_DRONE_AUDIO candidate=22 parked_player_stop_requests=3 owner_world_transfer=true remote_world_unchanged=true"
        );
    } else {
        tracing::debug!("SCBL_MOD_DRONE_AUDIO park_stop_not_repeated_or_capacity_full=true");
    }
}
pub(super) unsafe fn resume(body: usize) {
    if !READY.load(Ordering::Acquire) || word(body, 0x910).map(|v| v & 255) != Some(2) {
        return;
    }
    let Some(id) = identity(body) else {
        return;
    };
    let Some(normal) = word(body, 0xb70)
        .map(|v| v as usize)
        .filter(|&p| named(p, "Play_SFX_ADV_Gadget_Drone_Loop_Player"))
    else {
        return;
    };
    // End our spatial copy BEFORE restoring the same drone's operator loop.
    sync_world(body, id, false);
    let (requests, remembered) = {
        let mut bank = BANK.lock().unwrap_or_else(|e| e.into_inner());
        let remembered = bank.remember_native_start(
            body,
            id,
            Request {
                event: normal,
                slot: 7,
                scalar: 0.,
                a: 0,
                b: 0,
            },
        );
        (bank.resume(body, id), remembered)
    };
    tracing::info!(
        "SCBL_MOD_DRONE_AUDIO candidate=22 control_native_start_remembered={} resume_requests={}",
        remembered,
        requests.iter().filter(|r| r.is_some()).count()
    );
    for r in requests.into_iter().flatten() {
        if classify(body, r.event).is_some_and(|(_, start)| start) {
            original(body, r);
        }
    }
}
unsafe fn replace(expected: usize, new: usize) -> bool {
    if word(SLOT, 0) != Some(expected as u32) {
        return false;
    }
    let mut old = 0;
    if super::VirtualProtect(SLOT as _, 4, 4, &mut old) == 0 {
        return false;
    }
    std::ptr::write_volatile(SLOT as *mut u32, new as u32);
    let mut discard = 0;
    if super::VirtualProtect(SLOT as _, 4, old, &mut discard) != 0 {
        return true;
    }
    std::ptr::write_volatile(SLOT as *mut u32, expected as u32);
    let _ = super::VirtualProtect(SLOT as _, 4, old, &mut discard);
    false
}
pub(super) unsafe fn init() {
    if READY.load(Ordering::Acquire) {
        return;
    }
    if !signature(
        PLAY,
        158,
        "69835d0e1888fe3358b9e523986e8082311bc28c2486909d324610a0f2766682",
    ) || !replace(PLAY, play as *const () as usize)
    {
        tracing::warn!("SCBL_MOD_DRONE_AUDIO enabled=false reason=signature_or_slot");
        return;
    }
    READY.store(true, Ordering::Release);
    let spy_gain_enabled = spy_gain::init();
    tracing::info!("SCBL_MOD_DRONE_SPY_AUDIO adapter_ready={}", spy_gain_enabled);
    tracing::info!("SCBL_MOD_DRONE_AUDIO enabled=true candidate=22 scope=drone_operator_loops");
}
pub(super) unsafe fn deinit() {
    spy_gain::deinit();
    READY.store(false, Ordering::Release);
    let owned: Vec<_> = BANK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .records
        .iter()
        .map(|(&body, r)| (body, r.id))
        .collect();
    for (body, id) in owned {
        sync_world(body, id, false);
    }
    let _ = replace(play as *const () as usize, PLAY);
    BANK.lock()
        .unwrap_or_else(|e| e.into_inner())
        .records
        .clear();
}
#[cfg(test)]
#[path = "drone_control_audio_tests.rs"]
mod tests;
