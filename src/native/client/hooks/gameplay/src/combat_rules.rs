//! Stateless hot-reload policies; native ownership and side effects stay in host.
// Retail spider gloves use +30 percent for traversal (observed equipment
// MPArmorGlovesIntelScout+78; current retail EFFCFB converts percent to speed).
// C61 user-selected paired lethal takedown bonus: +20%; traversal unchanged.
// Playback 1.20 means duration /1.20 (~16.67% shorter), not 20% less time.
// This policy never alters native melee reach or the paired-state whitelist.
pub fn melee_speed(equipped_spy: bool) -> f32 {
    if equipped_spy {
        1.20
    } else {
        1.0
    }
}
/// Zero means the original EMP path. HP is native integer HP. Round a half
/// point upwards for odd maxima, so a full-health drone still takes two EMPs.
pub fn drone_emp_damage(current: u32, maximum: u32) -> u32 {
    if maximum < 2 || maximum > i32::MAX as u32 || current > maximum || current <= maximum / 2 {
        0
    } else {
        maximum.div_ceil(2)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn equipped_gloves_change_playback_not_reach() {
        assert_eq!(melee_speed(true), 1.20);
        assert_eq!(melee_speed(false), 1.0);
    }
    #[test]
    fn emp_full_above_half_equal_half_below_half_and_invalid() {
        for (hp, max, want) in [
            (100, 100, 50),
            (51, 100, 50),
            (50, 100, 0),
            (49, 100, 0),
            (0, 100, 0),
            (101, 100, 0),
            (1, 1, 0),
            (0, 0, 0),
            (u32::MAX, u32::MAX, 0),
            (99, 99, 50),
            (50, 99, 50),
            (49, 99, 0),
        ] {
            assert_eq!(drone_emp_damage(hp, max), want, "{hp}/{max}");
        }
    }
    #[test]
    fn two_emp_and_native_bullet_damage_between_hits() {
        let hp = 100 - drone_emp_damage(100, 100);
        assert_eq!(hp, 50);
        assert_eq!(drone_emp_damage(hp, 100), 0);
        assert_eq!(drone_emp_damage(70, 100), 50);
        assert_eq!(drone_emp_damage(20, 100), 0);
    }
}

#[cfg(test)]
mod balance_tests {
    use super::*;
    #[test]
    fn take_down_duration_bonus_is_deliberately_less_than_native_spider_bonus() {
        let speed = melee_speed(true);
        let duration_ratio = 1.0 / speed;
        assert!((duration_ratio - 0.8333333).abs() < 0.000001);
        assert!(duration_ratio > 1.0 / 1.30);
        assert!(((speed - 1.0) - 0.20).abs() < 0.000001);
        println!("C61_MELEE_BALANCE equipped=true speed={speed:.2} duration_ratio={duration_ratio:.6} native_reach=unchanged");
    }
}
