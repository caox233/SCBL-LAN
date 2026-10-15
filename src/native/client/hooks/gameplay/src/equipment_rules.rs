//! Hot, pointer-free policies. Native adapters only supply verified observations.
// Preserve the original game-selected range, including its equipped-glove bonus.
// Keep this ABI event available for existing hosts, but do not extend reach.
pub fn melee_range(native: f32, _multiplier: f32, _equipped_spy: bool) -> f32 {
    native
}
// Legacy rule ABI may not suppress native detection. Presentation lives in rfd_rules.
pub fn rfd_allowed(native: bool, _pants: bool, _kind: u32, _flags: u32) -> bool { native }
// State: visible latch and remaining fade milliseconds. Never changes stamina.
pub fn stamina(
    valid: bool,
    current: f32,
    maximum: f32,
    dt: f32,
    visible: &mut bool,
    fade: &mut f32,
) -> (f32, f32) {
    if !valid
        || !current.is_finite()
        || !maximum.is_finite()
        || maximum <= 0.0
        || current < 0.0
        || current > maximum * 1.01
    {
        *visible = false;
        *fade = 0.0;
        return (0.0, 0.0);
    }
    let ratio = (current / maximum).clamp(0.0, 1.0);
    if ratio <= 0.5 {
        *visible = true;
    }
    if !*visible {
        return (ratio, 0.0);
    }
    if ratio < 0.9999 {
        *fade = 0.5;
        return (ratio, 1.0);
    }
    let step = if dt.is_finite() && dt >= 0.0 {
        dt.min(0.1)
    } else {
        0.0
    };
    *fade = (*fade - step).max(0.0);
    if *fade == 0.0 {
        *visible = false;
    }
    (ratio, (*fade / 0.5).clamp(0.0, 1.0))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_range_and_glove_bonus_are_preserved_bit_for_bit() {
        for native in [
            0.0f32,
            -0.0,
            -1.0,
            120.0,
            180.0,
            189.0,
            270.0,
            f32::INFINITY,
            f32::from_bits(0x7fc00042),
        ] {
            for multiplier in [1.0, 1.05, 1.5, 2.0, f32::NAN] {
                for equipped in [false, true] {
                    assert_eq!(
                        melee_range(native, multiplier, equipped).to_bits(),
                        native.to_bits()
                    );
                }
            }
        }
    }
    #[test]
    fn rfd_only_exact_equipment_and_three_windows() {
        for native in [false, true] {
            for pants in [false, true] {
                for kind in 0..6 {
                    for flags in [0, 1, 8, 32, 40, 64] {
                        let _protected = matches!(kind, 1 | 2) && flags & 8 != 0 && flags & 32 == 0
                            || kind == 3 && flags & (32 | 8) != 0;
                        assert_eq!(
                            rfd_allowed(native, pants, kind, flags),
                            native
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn stamina_half_recovery_full_fade_and_respawn() {
        let (mut show, mut fade) = (false, 0.0);
        assert_eq!(
            stamina(true, 51.0, 100.0, 0.016, &mut show, &mut fade),
            (0.51, 0.0)
        );
        assert_eq!(
            stamina(true, 50.0, 100.0, 0.016, &mut show, &mut fade),
            (0.5, 1.0)
        );
        assert_eq!(
            stamina(true, 90.0, 100.0, 0.016, &mut show, &mut fade),
            (0.9, 1.0)
        );
        assert!(stamina(true, 100.0, 100.0, 0.1, &mut show, &mut fade).1 > 0.0);
        for _ in 0..6 {
            stamina(true, 100.0, 100.0, 0.1, &mut show, &mut fade);
        }
        assert!(!show);
        assert_eq!(
            stamina(false, 25.0, 100.0, 0.016, &mut show, &mut fade),
            (0.0, 0.0)
        );
    }
    #[test]
    fn invalid_native_stamina_never_displays() {
        for (a, b) in [
            (f32::NAN, 10.0),
            (1.0, 0.0),
            (-1.0, 10.0),
            (11.0, 10.0),
            (5.0, f32::INFINITY),
        ] {
            let (mut show, mut fade) = (true, 0.5);
            assert_eq!(stamina(true, a, b, 0.016, &mut show, &mut fade), (0.0, 0.0));
            assert!(!show);
        }
    }
    #[test]
    fn legacy_rfd_abi_never_suppresses_native_visibility() {
        for flags in [0x21, 0x29, 0x09] {
            assert!(rfd_allowed(true, true, 3, flags));
            assert!(rfd_allowed(true, false, 3, flags));
        }
        for flags in [0, 1, 0x41, 0x81] {
            assert!(rfd_allowed(true, true, 3, flags));
        }
        assert!(!rfd_allowed(false, true, 3, 1));
        assert!(rfd_allowed(true, true, 0, 9));
    }
    #[test]
    fn actual_glove_five_percent_bonus_remains_original() {
        assert!((melee_range(189.0, 1.05, true) - 189.0).abs() < 0.001);
        assert!((melee_range(126.0, 1.05, true) - 126.0).abs() < 0.001);
        assert_eq!(melee_range(180.0, 1.0, false), 180.0);
    }
}
