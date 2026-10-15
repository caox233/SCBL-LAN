//! C28 read-only native ATS shot facts. No network packet or synthetic game
//! detection is emitted here. D76240 supplies real native shot boundaries.
//! Pointers are consumed synchronously; retained marks contain copied positions.
//! The separate native view owns UI; this module retains only copied facts.
use crate::ats_marks::{Listener, Mark, Marks, Role, Shot, Weapon};
use crate::equipment_native::{float, readable, signature, word};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Mutex,
};
const SPY: u32 = 0x2ad3234;
const MERC: u32 = 0x2ad2abc;
const ATS: u32 = 0x2a95c5c;
const ARMOR: u32 = 0x2ad42ac;
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
static REJECT_LOGS: AtomicU32 = AtomicU32::new(0);
static STATE: Mutex<Option<State>> = Mutex::new(None);
struct State {
    world: usize,
    info: usize,
    local: usize,
    team: u32,
    last: u64,
    serial: u64,
    marks: Marks,
}
#[derive(Clone, Debug)]
struct Person {
    controller: usize,
    pawn: usize,
    team: u32,
}
#[derive(Clone, Debug)]
struct Facts {
    world: usize,
    info: usize,
    local: usize,
    team: u32,
    shot: Shot,
    listeners: Vec<Listener>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Frame {
    pub world: usize,
    pub info: usize,
    pub local: usize,
    pub team: u32,
    pub now: u64,
    pub position: [f32; 3],
    pub distance_delay_ms: u64,
    pub visible: bool,
}
unsafe fn frame_from(info: usize) -> Option<Frame> {
    let world = ptr(info, 0x144)?;
    let list = roster(info)?;
    let local_pc = crate::local_player::controller(world)?;
    let local = list.iter().find(|r| r.controller == local_pc)?;
    if live_role(local.pawn, world)? != Role::Merc {
        return None;
    }
    Some(Frame {
        world,
        info,
        local: local.pawn,
        team: local.team,
        now: milliseconds(float(world, 0x5a0)?)?,
        position: position(local.pawn)?,
        distance_delay_ms: milliseconds(float(info, 0x534)?)?,
        visible: true,
    })
}
pub(crate) unsafe fn frame() -> Option<Frame> {
    if !READY.load(Ordering::Acquire) {
        return None;
    }
    frame_from(ptr(0x339e5a0, 0)?)
}
/// Opposing native roster identities for detector results. Object +20 is the exact
/// 44D890 key used by the BMS entry resolver, not a guessed pawn pointer.
pub(crate) unsafe fn detector_players(f: &Frame) -> Option<Vec<(u32, u64, Role)>> {
    let current = frame()?;
    detector_players_from(f, &current)
}
unsafe fn detector_players_from(f: &Frame, current: &Frame) -> Option<Vec<(u32, u64, Role)>> {
    if (f.world, f.info, f.local, f.team)
        != (current.world, current.info, current.local, current.team)
    {
        return None;
    }
    let mut out = Vec::new();
    for r in roster(f.info)? {
        if r.team == f.team {
            continue;
        }
        let Some(role) = live_role(r.pawn, f.world) else {
            continue;
        };
        if word(r.pawn, 0xc50) != Some(0) {
            continue;
        }
        let Some(object) = word(r.pawn, 0x20).filter(|v| *v != 0) else {
            continue;
        };
        let Some(peer) = ptr(r.pawn, 0x2c0) else {
            continue;
        };
        let (Some(low), Some(high)) = (word(peer, 0x70), word(peer, 0x74)) else {
            continue;
        };
        let key = u64::from(low) | (u64::from(high) << 32);
        if key == 0 || out.iter().any(|&(o, k, _)| o == object || k == key) {
            return None;
        }
        out.push((object, key, role));
    }
    Some(out)
}
unsafe fn ptr(p: usize, o: usize) -> Option<usize> {
    let x = word(p, o)? as usize;
    (x >= 0x10000).then_some(x)
}
unsafe fn byte(p: usize, o: usize) -> Option<u8> {
    let x = p.checked_add(o)?;
    readable(x, 1).then(|| (x as *const u8).read())
}
unsafe fn vector(p: usize, o: usize, max: usize) -> Option<Vec<usize>> {
    let n = word(p, o + 4)? as usize;
    let cap = word(p, o + 8)? as usize;
    if n > max || n > cap || cap > max * 4 {
        return None;
    }
    if n == 0 {
        return Some(Vec::new());
    }
    let d = ptr(p, o)?;
    if !readable(d, n * 4) {
        return None;
    }
    (0..n).map(|i| word(d, i * 4).map(|v| v as usize)).collect()
}
unsafe fn position(p: usize) -> Option<[f32; 3]> {
    let v = [float(p, 0x9c)?, float(p, 0xa0)?, float(p, 0xa4)?];
    v.iter().all(|x| x.is_finite()).then_some(v)
}
fn milliseconds(seconds: f32) -> Option<u64> {
    let n = seconds as f64 * 1000.;
    (n.is_finite() && n >= 0. && n < (u64::MAX as f64)).then_some(n.round() as u64)
}
unsafe fn live_role(p: usize, world: usize) -> Option<Role> {
    if ptr(p, 0x144)? != world
        || word(p, 0x60)? & (1 << 11) != 0
        || word(p, 0x4b0)? & 0x80000000 != 0
    {
        return None;
    }
    match (word(p, 0)?, word(p, 0x374)? & 0xc0000) {
        (SPY, 0x40000) => Some(Role::Spy),
        (MERC, 0x80000) => Some(Role::Merc),
        _ => None,
    }
}
unsafe fn roster(info: usize) -> Option<Vec<Person>> {
    if word(info, 0x330)? & 0x10 == 0 {
        return None;
    }
    let records = vector(info, 0x448, 32)?;
    let mut people = Vec::new();
    for r in records {
        if r == 0 {
            continue;
        }
        let Some(pc) = ptr(r, 0x48) else {
            continue;
        };
        let Some(pawn) = ptr(pc, 0x520) else {
            continue;
        };
        let team = word(r, 0x2c)?;
        if team == u32::MAX {
            continue;
        }
        // The native roster's team record must agree with its controller.
        let vt = ptr(pc, 0)?;
        if word(vt, 0x430) != Some(0xe5c960) || word(pc, 0x6dc) != Some(team) {
            continue;
        }
        if people
            .iter()
            .any(|x: &Person| x.controller == pc || x.pawn == pawn)
        {
            return None;
        }
        people.push(Person {
            controller: pc,
            pawn,
            team,
        });
    }
    Some(people)
}
// Only copied, live roster facts; no detector inference and no stored pointers.
#[cfg(any(test, feature = "goggle-feedback"))]
pub(crate) unsafe fn feedback_people(info: usize) -> Option<Vec<(usize, usize, u32, Role)>> {
    let world = ptr(info, 0x144)?;
    let mut out = Vec::new();
    for p in roster(info)? {
        if word(p.pawn, 0xc50) != Some(0) {
            continue;
        }
        let Some(role) = live_role(p.pawn, world) else {
            continue;
        };
        out.push((p.controller, p.pawn, p.team, role));
    }
    Some(out)
}
// D5FB10: inventory +38 buckets, +28 bucket ID, +2C items, first item.
// Only the exact ATS class and its native operational bit are accepted.
unsafe fn ats_gear(pawn: usize) -> Option<usize> {
    let inventory = ptr(pawn, 0x1410)?;
    let buckets = vector(inventory, 0x38, 64)?;
    for b in buckets {
        if b != 0 && byte(b, 0x28) == Some(0x29) {
            let entries = vector(b, 0x2c, 8)?;
            let gear = *entries.first()?;
            return (word(gear, 0) == Some(ATS) && word(gear, 0x744)? & 1 != 0).then_some(gear);
        }
    }
    None
}
// E08E90 -> DFFFD0. Match the native inherent-suppression bit OR any
// installed suppressor (native property bit / suppressor FName); no catalog
// guess, no static "silent weapon" list.
unsafe fn suppressed(weapon: usize, suppressor_name: u32) -> Option<bool> {
    // Replicated attachment instances can exist before the firing-logic
    // object. The native E08E90 getter returns false immediately in that case.
    // Accept a positive installed-suppressor fact without fabricating a logic
    // object; absence of an attachment is still unknown while logic is absent.
    let logic = word(weapon, 0x748)? as usize;
    let logic_known = if logic == 0 {
        false
    } else {
        if logic < 0x10000 || ptr(logic, 0x28)? != weapon {
            return None;
        }
        let props = ptr(logic, 0x2c)?;
        if word(props, 0x2c)? & 0x100 != 0 {
            return Some(true);
        }
        true
    };
    for attachment in vector(weapon, 0x764, 32)? {
        if attachment == 0 {
            continue;
        }
        let Some(prop) = ptr(attachment, 0x700) else {
            continue;
        };
        if byte(prop, 0x48)? & 4 != 0 || word(prop, 0x2c)? == suppressor_name {
            return Some(true);
        }
    }
    logic_known.then_some(false)
}

/// Synchronous facts for the native ATS call-site correction only. Incomplete
/// or foreign identities leave the original classifier result unchanged.
pub(crate) unsafe fn suppression_for_ats(weapon: usize, info: usize, suppressor_name: u32) -> Option<bool> {
    if suppressor_name == 0 {
        return None;
    }
    let f = frame_from(info)?;
    let pawn = ptr(weapon, 0x8c)?;
    if ptr(pawn, 0x4b8)? != weapon || live_role(pawn, f.world)? != Role::Spy {
        return None;
    }
    let people = roster(info)?;
    let source = people.iter().find(|person| person.pawn == pawn)?;
    if source.team == f.team {
        return None;
    }
    let props = ptr(weapon, 0x744)?;
    if !matches!(byte(props, 0x3b)?, 1 | 2) {
        return None;
    }
    suppressed(weapon, suppressor_name)
}
unsafe fn weapon_kind(pawn: usize, suppressor_name: u32) -> Option<Weapon> {
    let weapon = ptr(pawn, 0x4b8)?;
    let props = ptr(weapon, 0x744)?;
    let family = byte(props, 0x38)?;
    if family == 0x21 {
        return Some(Weapon::ShockCrossbow);
    }
    // DD51F0 uses the CURRENT archetype slot. MAC-11 is already reclassified
    // by its established pre-inventory adapter, so it remains a pistol exemption.
    match byte(props, 0x3b)? {
        2 => return Some(Weapon::Pistol),
        1 => {}
        _ => return None,
    }
    // B105A0 reads the firing pawn's current weapon, then DD51F0 identifies
    // the runtime slot. Remote proxies need not populate owner-local inventory
    // +50/+54; using those slots here rejects otherwise valid replicated shots.
    Some(if suppressed(weapon, suppressor_name)? {
        Weapon::SuppressedPrimary
    } else {
        Weapon::Unsuppressed
    })
}
unsafe fn collect(pawn: usize, info: usize, suppressor_name: u32) -> Option<Facts> {
    if suppressor_name == 0 {
        return None;
    }
    let world = ptr(pawn, 0x144)?;
    if live_role(pawn, world)? != Role::Spy {
        return None;
    }
    let list = roster(info)?;
    let source = list.iter().find(|r| r.pawn == pawn)?;
    let local_pc = crate::local_player::controller(world)?;
    let local = list.iter().find(|r| r.controller == local_pc)?;
    if live_role(local.pawn, world)? != Role::Merc || local.team == source.team {
        return None;
    }
    let time = milliseconds(float(world, 0x5a0)?)?;
    let range = float(info, 0x544)?;
    let base = float(info, 0x530)?;
    if !range.is_finite() || range <= 0. || !base.is_finite() || base <= 0. {
        return None;
    }
    let mut listeners = Vec::new();
    for p in &list {
        if p.team != local.team
            || live_role(p.pawn, world) != Some(Role::Merc)
            || ats_gear(p.pawn).is_none()
        {
            continue;
        }
        let Some(armor) = ptr(p.pawn, 0xcf4) else {
            continue;
        };
        if word(armor, 0) != Some(ARMOR) {
            continue;
        }
        let Some(mult) = float(armor, 0xfc).filter(|v| v.is_finite() && *v > 0.) else {
            continue;
        };
        let Some(duration) = milliseconds(base * mult).filter(|v| *v > 0) else {
            continue;
        };
        let Some(at) = position(p.pawn) else {
            continue;
        };
        listeners.push(Listener {
            scene: world as u64,
            team: p.team,
            role: Role::Merc,
            ats_operational: true,
            position: at,
            native_footstep_range: range.min(2000.0),
            native_duration_ms: duration,
        });
    }
    Some(Facts {
        world,
        info,
        local: local.pawn,
        team: local.team,
        shot: Shot {
            scene: world as u64,
            source: pawn as u64,
            serial: 0,
            game_time_ms: time,
            team: source.team,
            role: Role::Spy,
            weapon: weapon_kind(pawn, suppressor_name)?,
            position: position(pawn)?,
        },
        listeners,
    })
}
impl State {
    fn feed(&mut self, mut f: Facts) -> bool {
        let now = f.shot.game_time_ms;
        if (self.world, self.info, self.local) != (f.world, f.info, f.local)
            || self.team != f.team
            || now < self.last
        {
            self.marks.begin_scene(f.world as u64, f.team);
            self.world = f.world;
            self.info = f.info;
            self.local = f.local;
            self.team = f.team;
            self.serial = 0;
        }
        // Shot serial is advanced only by the native source callback, never a HUD tick.
        self.serial = self.serial.wrapping_add(1);
        self.last = now;
        f.shot.serial = self.serial;
        self.marks.observe(f.shot, &f.listeners, now)
    }
}
pub(crate) unsafe fn observe_source(pawn: usize) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let facts = ptr(0x339e5a0, 0)
        .and_then(|info| word(0x33a039c, 0).and_then(|id| collect(pawn, info, id)));
    let Some(f) = facts else {
        if word(pawn, 0) == Some(SPY) && REJECT_LOGS.fetch_add(1, Ordering::Relaxed) < 16 {
            let info = ptr(0x339e5a0, 0);
            let world = ptr(pawn, 0x144);
            let local = world.and_then(|w| crate::local_player::controller(w));
            let local_role = local
                .and_then(|pc| ptr(pc, 0x520))
                .and_then(|p| live_role(p, world?));
            let kind = word(0x33a039c, 0).and_then(|id| weapon_kind(pawn, id));
            tracing::info!("SCBL_R58_ATS_SNAPSHOT revision=C55 facts_complete=false accepted=false roster={} local_role={:?} source_role={:?} weapon={:?}",
                info.and_then(|i|roster(i)).map_or(0,|r|r.len()),local_role,world.and_then(|w|live_role(pawn,w)),kind);
        }
        return;
    };
    if f.shot.weapon != Weapon::SuppressedPrimary {
        return;
    }
    if let Ok(mut lock) = STATE.lock() {
        let s = lock.get_or_insert_with(|| State {
            world: f.world,
            info: f.info,
            local: f.local,
            team: f.team,
            last: f.shot.game_time_ms,
            serial: 0,
            marks: Marks::new(f.world as u64, f.team),
        });
        let listener_count = f.listeners.len();
        let accepted = s.feed(f);
        if LOGS.fetch_add(1, Ordering::Relaxed) < 32 {
            tracing::info!("SCBL_R58_ATS_SNAPSHOT accepted={accepted} native_listener_count={listener_count} fixed_position=true source_only=true");
        }
    }
}
// Does not dereference any retained game actor. The view supplies fresh
// validated local role/scene facts, never inferring them from old marks.
pub(crate) fn snapshot(
    world: usize,
    info: usize,
    local: usize,
    team: u32,
    role: Role,
    now: u64,
) -> Vec<Mark> {
    let Ok(mut lock) = STATE.lock() else {
        return Vec::new();
    };
    let Some(s) = lock.as_mut() else {
        return Vec::new();
    };
    if (s.world, s.info, s.local) != (world, info, local)
        || s.team != team || role != Role::Merc || now < s.last
    {
        // Rejecting only this read lets old marks revive after a context returns
        // or a reset clock catches up, even without another shot.
        *lock = None;
        return Vec::new();
    }
    // Track observed time, not just the last shot. Never extend mark expiry.
    s.last = now;
    s.marks.visible(world as u64, team, role, now)
}
fn invalidate_samples() {
    if let Ok(mut lock) = STATE.lock() {
        *lock = None;
    }
}
pub(crate) fn snapshot_frame(frame: Option<&Frame>) -> Vec<Mark> {
    match frame {
        Some(f) => snapshot(f.world, f.info, f.local, f.team, Role::Merc, f.now),
        None => {
            // No valid local frame is a lifetime boundary; mere HUD hiding is not.
            invalidate_samples();
            Vec::new()
        }
    }
}
pub(crate) unsafe fn init() -> bool {
    let ok = SIGNATURES.iter().all(|&(a, n, h)| signature(a, n, h));
    READY.store(ok, Ordering::Release);
    ok
}
pub(crate) fn deinit() {
    READY.store(false, Ordering::Release);
    invalidate_samples();
}
const SIGNATURES: &[(usize, usize, &str)] = &[
    (
        0xd5fb10,
        80,
        "f811eb09bb06acbbb543a16d22036c4417326f660525449237ee534bd2854d9b",
    ),
    (
        0xe08e90,
        18,
        "3f27ba87598b5f3653f956d322e3be51bd81a514f2d04811bcdd2d3a97c008df",
    ),
    (
        0xdfffd0,
        99,
        "a1da17de7f25336c79f2151f0ac3ca2f1734fbbd832caaa1e9cfa2e2d4ecaa8c",
    ),
    (
        0xdd51d0,
        11,
        "d5d7dd5cb4c18ee6fa10149baf6af2862300e0a12ae5789706f2d67989feac80",
    ),
    (
        0xdd51f0,
        11,
        "bf36c2d5e429e5aa3a33217454dd0e8a762c87e53c85a05906a649c0e460a3b9",
    ),
    (
        0xe5c960,
        7,
        "34a6bc6fe248655335aaaa845b9246e9b2ffab6e80679c5eb9f9f305c05bb527",
    ),
    (
        0xe8ca30,
        25,
        "b2eab6727fe4e2b25959bb68eb1c9b8c3ba835a09d34c075b8ff86cbfd12a015",
    ),
];
#[cfg(test)]
#[path = "ats_native_source_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ats_lifetime_tests.rs"]
mod lifetime_tests;
