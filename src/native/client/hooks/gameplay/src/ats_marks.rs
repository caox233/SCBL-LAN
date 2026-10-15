//! R58 last-shot policy. No native hook or transport is enabled by this module.
//! The adapter must supply validated native team/weapon/range facts and a stable
//! shot identity. In particular, a HUD tick is NOT a new shot. Coordinates are
//! copied once, never obtained from the actor while rendering a retained mark.
#![allow(dead_code)]
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Spy,
    Merc,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Weapon {
    SuppressedPrimary,
    Pistol, // includes the MOD MAC-11 pistol; not inferred from its old asset type
    ShockCrossbow,
    Unsuppressed,
    Unknown,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Shot {
    pub scene: u64,
    pub source: u64,
    pub serial: u64,
    pub game_time_ms: u64,
    pub team: u32,
    pub role: Role,
    pub weapon: Weapon,
    pub position: [f32; 3],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Listener {
    pub scene: u64,
    pub team: u32,
    pub role: Role,
    pub ats_operational: bool,
    pub position: [f32; 3],
    // The untouched native FOOTSTEP range, before the silent-boots modifier.
    // No fixed guessed metre value is embedded in the policy.
    pub native_footstep_range: f32,
    // Native tuning +530 multiplied by this actual ATS detector armor +FC.
    pub native_duration_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Mark {
    pub source: u64,
    pub serial: u64,
    pub position: [f32; 3],
    pub shot_time_ms: u64,
    pub expires_ms: u64,
}

pub(crate) struct Marks {
    scene: u64,
    team: u32,
    items: BTreeMap<u64, Mark>,
}

fn finite_position(v: [f32; 3]) -> bool {
    v.iter().all(|x| x.is_finite())
}

fn detected(shot: &Shot, listener: &Listener, team: u32) -> bool {
    if listener.scene != shot.scene
        || listener.team != team
        || listener.team == shot.team
        || listener.role != Role::Merc
        || !listener.ats_operational
        || !listener.native_footstep_range.is_finite()
        || listener.native_footstep_range <= 0.0
        || listener.native_duration_ms == 0
        || !finite_position(listener.position)
    {
        return false;
    }
    // f64 avoids overflowing valid f32 coordinates while comparing distances.
    let squared: f64 = shot
        .position
        .iter()
        .zip(listener.position)
        .map(|(a, b)| (*a as f64 - b as f64).powi(2))
        .sum();
    squared <= (listener.native_footstep_range as f64).powi(2)
}

impl Marks {
    pub(crate) fn new(scene: u64, team: u32) -> Self {
        Self {
            scene,
            team,
            items: BTreeMap::new(),
        }
    }

    pub(crate) fn begin_scene(&mut self, scene: u64, team: u32) {
        self.scene = scene;
        self.team = team;
        self.items.clear();
    }

    pub(crate) fn observe(&mut self, shot: Shot, listeners: &[Listener], now_ms: u64) -> bool {
        self.items.retain(|_, mark| now_ms < mark.expires_ms);
        if shot.scene != self.scene
            || shot.source == 0
            || shot.role != Role::Spy
            || shot.weapon != Weapon::SuppressedPrimary
            || shot.team == self.team
            || !finite_position(shot.position)
            || now_ms < shot.game_time_ms
        {
            return false;
        }
        // Each eligible detector contributes its native duration. Keep the mark
        // while at least one real detection remains valid; never extend it on
        // rendering or on a duplicate callback serial.
        let expires = listeners
            .iter()
            .filter(|l| detected(&shot, l, self.team))
            .filter_map(|l| shot.game_time_ms.checked_add(l.native_duration_ms))
            .filter(|&expiry| now_ms < expiry)
            .max();
        let Some(expires_ms) = expires else {
            return false;
        };
        if self.items.get(&shot.source).is_some_and(|previous| {
            previous.serial == shot.serial || previous.shot_time_ms > shot.game_time_ms
        }) {
            return false;
        }
        self.items.insert(
            shot.source,
            Mark {
                source: shot.source,
                serial: shot.serial,
                position: shot.position,
                shot_time_ms: shot.game_time_ms,
                expires_ms,
            },
        );
        true
    }

    // All merc teammates may view the snapshot, including ones without ATS.
    // No name/health permission or actor highlight is returned by this API.
    pub(crate) fn visible(&self, scene: u64, team: u32, role: Role, now_ms: u64) -> Vec<Mark> {
        if scene != self.scene || team != self.team || role != Role::Merc {
            return Vec::new();
        }
        self.items
            .values()
            .filter(|m| now_ms >= m.shot_time_ms && now_ms < m.expires_ms)
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shot() -> Shot {
        Shot {
            scene: 1,
            source: 2,
            serial: 3,
            game_time_ms: 100,
            team: 10,
            role: Role::Spy,
            weapon: Weapon::SuppressedPrimary,
            position: [90.0, 0.0, 0.0],
        }
    }
    fn listener() -> Listener {
        Listener {
            scene: 1,
            team: 20,
            role: Role::Merc,
            ats_operational: true,
            position: [0.0; 3],
            native_footstep_range: 100.0,
            native_duration_ms: 5000,
        }
    }
    #[test]
    fn fixed_snapshot_expires_at_five_seconds_not_last_render() {
        let mut marks = Marks::new(1, 20);
        let mut s = shot();
        assert!(marks.observe(s, &[listener()], 100));
        s.position[0] = 40.0;
        assert_eq!(marks.visible(1, 20, Role::Merc, 5099)[0].position[0], 90.0);
        assert!(!marks.observe(s, &[listener()], 5099)); // same shot, not a new event
        assert!(marks.visible(1, 20, Role::Merc, 5100).is_empty());
    }
    #[test]
    fn new_shot_replaces_position_and_uses_shot_time_not_arrival_time() {
        let mut marks = Marks::new(1, 20);
        assert!(marks.observe(shot(), &[listener()], 100));
        let s = Shot {
            serial: 4,
            game_time_ms: 200,
            position: [80.0, 0.0, 0.0],
            ..shot()
        };
        assert!(marks.observe(s, &[listener()], 1000));
        let m = marks.visible(1, 20, Role::Merc, 1000)[0];
        assert_eq!((m.position[0], m.expires_ms), (80.0, 5200));
        assert!(!marks.observe(shot(), &[listener()], 1100));
    }
    #[test]
    fn pistols_mac11_shock_and_unsuppressed_do_not_enter_new_policy() {
        for weapon in [
            Weapon::Pistol,
            Weapon::ShockCrossbow,
            Weapon::Unsuppressed,
            Weapon::Unknown,
        ] {
            let mut marks = Marks::new(1, 20);
            assert!(!marks.observe(Shot { weapon, ..shot() }, &[listener()], 100));
        }
    }
    #[test]
    fn radius_uses_validated_native_footstep_distance_including_exact_edge() {
        let mut marks = Marks::new(1, 20);
        for (distance, expected) in [(90.0, true), (100.0, true), (100.1, false)] {
            marks.begin_scene(1, 20);
            assert_eq!(
                marks.observe(
                    Shot {
                        position: [distance, 0.0, 0.0],
                        ..shot()
                    },
                    &[listener()],
                    100
                ),
                expected
            );
        }
    }
    #[test]
    fn listener_must_be_eligible_ally_merc_with_operational_ats() {
        for l in [
            Listener {
                scene: 2,
                ..listener()
            },
            Listener {
                team: 10,
                ..listener()
            },
            Listener {
                role: Role::Spy,
                ..listener()
            },
            Listener {
                ats_operational: false,
                ..listener()
            },
            Listener {
                native_footstep_range: f32::NAN,
                ..listener()
            },
            Listener {
                native_footstep_range: 0.0,
                ..listener()
            },
        ] {
            assert!(!Marks::new(1, 20).observe(shot(), &[l], 100));
        }
    }
    #[test]
    fn any_number_of_ats_listeners_yields_one_shared_merc_mark() {
        let listeners = vec![listener(); 17];
        let mut marks = Marks::new(1, 20);
        assert!(marks.observe(shot(), &listeners, 100));
        assert!(!marks.observe(shot(), &listeners, 200));
        assert_eq!(marks.visible(1, 20, Role::Merc, 200).len(), 1);
        assert!(marks.visible(1, 10, Role::Spy, 200).is_empty());
        assert!(marks.visible(1, 30, Role::Merc, 200).is_empty());
        assert!(marks.visible(2, 20, Role::Merc, 200).is_empty());
        marks.begin_scene(2, 20);
        assert!(marks.visible(2, 20, Role::Merc, 200).is_empty());
        assert!(!marks.observe(shot(), &listeners, 200));
    }
    #[test]
    fn unknown_corrupt_expired_or_future_facts_fail_closed() {
        let mut marks = Marks::new(1, 20);
        for s in [
            Shot {
                source: 0,
                ..shot()
            },
            Shot {
                role: Role::Unknown,
                ..shot()
            },
            Shot {
                position: [f32::NAN; 3],
                ..shot()
            },
            Shot {
                game_time_ms: 101,
                ..shot()
            },
        ] {
            assert!(!marks.observe(s, &[listener()], 100));
        }
        assert!(!marks.observe(shot(), &[listener()], 5100));
        assert!(!marks.observe(shot(), &[], 100));
    }
}
