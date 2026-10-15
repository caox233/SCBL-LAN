//! Development-only ABI observation for the two remaining R58 native adapters.
//! No synthetic shot, mark, audio, volume override or network message is emitted.
//! No saved actor pointer is dereferenced. Bounds are per process, not per frame.
use crate::equipment_native::{float, signature, word};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
static READY: AtomicBool = AtomicBool::new(false);
static FIRE_LOGS: AtomicU32 = AtomicU32::new(0);
static EVENT_LOGS: AtomicU32 = AtomicU32::new(0);
static AUDIO_LOGS: AtomicU32 = AtomicU32::new(0);
static_detour! {
    static Fire: unsafe extern "thiscall" fn(usize, usize);
    static Event: unsafe extern "thiscall" fn(usize, usize, usize) -> u8;
    static Audio: unsafe extern "thiscall" fn(usize, usize, usize, u32, usize, f32, usize) -> u32;
}
fn reserve(counter: &AtomicU32) -> bool {
    counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        (n < 96).then(|| n + 1)
    }).is_ok()
}
fn permitted(counter: &AtomicU32) -> bool {
    READY.load(Ordering::Acquire) && reserve(counter)
}
#[derive(Debug, PartialEq)]
struct ShotFacts {
    role_flags: Option<u32>,
    network_role: Option<u32>,
    weapon_present: bool,
    scene_time: Option<f32>,
}
unsafe fn shot_facts(pawn: usize) -> ShotFacts {
    ShotFacts {
        role_flags: word(pawn, 0x374).map(|v| v & 0xc0000),
        network_role: word(pawn, 0x4c).map(|v| (v >> 8) & 255),
        weapon_present: word(pawn, 0x4b8).is_some_and(|v| v != 0),
        scene_time: word(pawn, 0x144)
            .and_then(|p| float(p as usize, 0x5a0))
            .filter(|v| v.is_finite()),
    }
}
fn before<R>(observe: impl FnOnce(), native: impl FnOnce() -> R) -> R {
    observe();
    native()
}
fn fire(gear: usize, pawn: usize) {
    unsafe {
        before(
            || {
                if word(gear, 0) == Some(0x2a95c5c) && permitted(&FIRE_LOGS) {
                    tracing::info!("SCBL_R58_ATS_OBSERVE event=native_fire active={:?} shot={:?} readonly=true", word(gear,0x744).map(|v|v&1),shot_facts(pawn));
                }
            },
            || Fire.call(gear, pawn),
        );
    }
}
fn event(hud: usize, context: usize, data: usize) -> u8 {
    unsafe {
        before(
            || {
                if word(hud, 0) == Some(0x2c3e85c)
                    && word(data, 8).is_some_and(|v| v <= 1)
                    && permitted(&EVENT_LOGS)
                {
                    let source = word(data, 4).unwrap_or(0) as usize;
                    tracing::info!("SCBL_R58_ATS_OBSERVE event=native_hud active={:?} kind={:?} oneshot={:?} hud_active={:?} shot={:?} readonly=true",word(data,0),word(data,8),word(data,12),word(hud,0x2f8).map(|v|v&255),shot_facts(source));
                }
            },
            || Event.call(hud, context, data),
        )
    }
}
fn audio(
    system: usize,
    actor: usize,
    sound: usize,
    slot: u32,
    context: usize,
    parameter: f32,
    extra: usize,
) -> u32 {
    unsafe {
        before(
            || {
                if word(actor, 0) == Some(0x2a88f54) && permitted(&AUDIO_LOGS) {
                    let field = [
                        0xb6c, 0xb70, 0xb74, 0xb80, 0xb8c, 0xb90, 0xb94, 0xb98, 0xba4, 0xbb0, 0xbb8,
                    ]
                    .into_iter()
                    .find(|o| word(actor, *o) == Some(sound as u32));
                    // The float argument is deliberately NOT labelled volume: its
                    // exact DARE meaning is still under review. Do not multiply it.
                    tracing::info!("SCBL_R58_DRONE_AUDIO_OBSERVE field={field:?} state={:?} slot={slot} argument_bits={} context_present={} extra_present={} event_flags={:?} readonly=true",word(actor,0x910).map(|v|v&255),parameter.to_bits(),context!=0,extra!=0,word(sound,0x40));
                }
            },
            || Audio.call(system, actor, sound, slot, context, parameter, extra),
        )
    }
}
pub(crate) unsafe fn init(world: usize) {
    if world != 0x589860
        || ![
            (
                0xb105a0,
                110,
                "d49bd282485b2feffe6a76c7932f22d0d8c5e2721ce2c81d953c15a0fc9fc81a",
            ),
            (
                0x1a86ce0,
                619,
                "21abc09eb3b54fc52a83b829107845a6f0ed57318fd6e6f35de60a1171210b77",
            ),
            (
                0xa96d20,
                1136,
                "71e1cd21383ceb734a30e988d2e8a8181b3e0876ff346eb544425532b40003bc",
            ),
        ]
        .iter()
        .all(|&(a, n, h)| signature(a, n, h))
    {
        tracing::warn!("SCBL_R58_NATIVE_OBSERVE enabled=false reason=signature");
        return;
    }
    if Fire.initialize(std::mem::transmute::<usize,unsafe extern "thiscall" fn(usize,usize)>(0xb105a0),fire).is_err()
        || Event.initialize(std::mem::transmute::<usize,unsafe extern "thiscall" fn(usize,usize,usize)->u8>(0x1a86ce0),event).is_err()
        || Audio.initialize(std::mem::transmute::<usize,unsafe extern "thiscall" fn(usize,usize,usize,u32,usize,f32,usize)->u32>(0xa96d20),audio).is_err()
        || Fire.enable().is_err() || Event.enable().is_err() || Audio.enable().is_err() {
        deinit();tracing::warn!("SCBL_R58_NATIVE_OBSERVE enabled=false reason=hook");return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_R58_NATIVE_OBSERVE enabled=true readonly=true limit_each=96 shared_marks=false audio_boost=false");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Audio.disable();
    let _ = Event.disable();
    let _ = Fire.disable();
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_stops_mutating_after_96_reservations() {
        let counter = AtomicU32::new(0);
        assert_eq!((0..200).filter(|_| reserve(&counter)).count(), 96);
        assert_eq!(counter.load(Ordering::Relaxed), 96);
    }
    #[test]
    fn overflow_cannot_reopen_an_exhausted_quota() {
        let counter = AtomicU32::new(u32::MAX);
        assert!(!reserve(&counter));
        assert!(!reserve(&counter));
        assert_eq!(counter.load(Ordering::Relaxed), u32::MAX);
    }
    #[test]
    fn concurrent_reservations_keep_one_process_budget() {
        let counter = std::sync::Arc::new(AtomicU32::new(0));
        let workers = (0..8).map(|_| {
            let counter = counter.clone();
            std::thread::spawn(move || (0..30).filter(|_| reserve(&counter)).count())
        }).collect::<Vec<_>>();
        let accepted: usize = workers.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(accepted, 96);
        assert_eq!(counter.load(Ordering::Relaxed), 96);
    }
    #[test]
    fn snapshot_reads_only_validated_fields_and_preserves_source() {
        let mut pawn = vec![0u32; 0x4c0 / 4];
        let mut level = vec![0u32; 0x5a4 / 4];
        pawn[0x374 / 4] = 0x40000;
        pawn[0x4c / 4] = 3 << 8;
        pawn[0x144 / 4] = level.as_mut_ptr() as u32;
        level[0x5a0 / 4] = 12.25f32.to_bits();
        let bytes = (pawn.clone(), level.clone());
        let facts = unsafe { shot_facts(pawn.as_ptr() as usize) };
        assert_eq!(
            facts,
            ShotFacts {
                role_flags: Some(0x40000),
                network_role: Some(3),
                weapon_present: false,
                scene_time: Some(12.25)
            }
        );
        assert_eq!((pawn, level), bytes);
        assert_eq!(unsafe { shot_facts(0) }.scene_time, None);
    }
    #[test]
    fn native_result_and_call_count_are_unchanged() {
        use std::cell::Cell;
        let n = Cell::new(0);
        let observations = Cell::new(0);
        let bits = 0xffc12345u32;
        let result = before(
            || observations.set(observations.get() + 1),
            || {
                n.set(n.get() + 1);
                bits
            },
        );
        assert_eq!((result, n.get(), observations.get()), (bits, 1, 1));
    }
}
