//! One shared, allocation-free policy used by the rules DLL and host fallback.
use super::rule_contract::{AtsInput, NameplateInput};

pub fn nameplate(input: NameplateInput) -> u32 {
    if input.bms_equipped > 1 || input.intel_marked > 1 {
        return 0;
    }
    match (input.viewer_role, input.target_role) {
        (2, 1) => input.bms_equipped | input.intel_marked,
        (1, 2) => input.intel_marked,
        _ => 0,
    }
}

pub fn ats_range(input: AtsInput) -> f32 {
    if !input.native_range.is_finite() || input.native_range < 0.0 {
        return 0.0;
    }
    if input.silent_shoes == 1 {
        input.native_range * 0.5
    } else {
        input.native_range
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn name_health_permissions_are_independent_of_reticle_and_pants() {
        for viewer in 0..=3 {
            for target in 0..=3 {
                for bms in 0..=1 {
                    for intel in 0..=1 {
                        let expected = match (viewer, target) {
                            (2, 1) => bms | intel,
                            (1, 2) => intel,
                            _ => 0,
                        };
                        assert_eq!(
                            nameplate(NameplateInput {
                                viewer_role: viewer,
                                target_role: target,
                                bms_equipped: bms,
                                intel_marked: intel
                            }),
                            expected
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn ats_applies_half_distance_once_to_this_target_only() {
        for radius in [0.0, 1.0, 20.0, 1000.0] {
            assert_eq!(
                ats_range(AtsInput {
                    native_range: radius,
                    silent_shoes: 1
                }),
                radius / 2.0
            );
            assert_eq!(
                ats_range(AtsInput {
                    native_range: radius,
                    silent_shoes: 0
                }),
                radius
            );
            assert_eq!(
                ats_range(AtsInput {
                    native_range: radius,
                    silent_shoes: 99
                }),
                radius
            );
        }
    }
    #[test]
    fn invalid_values_never_create_detection_or_name_permission() {
        for radius in [f32::NAN, f32::INFINITY, -1.0] {
            assert_eq!(
                ats_range(AtsInput {
                    native_range: radius,
                    silent_shoes: 1
                }),
                0.0
            );
        }
        assert_eq!(
            nameplate(NameplateInput {
                viewer_role: 2,
                target_role: 1,
                bms_equipped: 2,
                intel_marked: 0
            }),
            0
        );
    }
}
