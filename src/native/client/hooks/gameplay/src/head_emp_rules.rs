//! Pure EMP equipment policy; no game pointers or native callbacks.
pub(crate) fn protected_phase(status: f32, visual: f32, first: f32, threshold: f32) -> f32 {
    if [status, visual, first, threshold]
        .iter()
        .any(|v| !v.is_finite())
        || status <= 0.0
        || visual <= 0.0
        || !(0.05..=3.0).contains(&first)
        || first > threshold
    {
        return 0.0;
    }
    first.min(status).min(visual)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retain_native_first_phase_only() {
        assert_eq!(protected_phase(6.0, 6.0, 1.0, 3.0), 1.0);
    }
    #[test]
    fn never_lengthen_short_emp() {
        assert_eq!(protected_phase(0.2, 4.0, 1.0, 3.0), 0.2);
    }
    #[test]
    fn never_enter_secondary_phase() {
        assert_eq!(protected_phase(8.0, 8.0, 4.0, 3.0), 0.0);
    }
    #[test]
    fn unknown_or_invalid_parameters_keep_native() {
        for v in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            assert_eq!(protected_phase(v, 6.0, 1.0, 3.0), 0.0);
            assert_eq!(protected_phase(6.0, v, 1.0, 3.0), 0.0);
            assert_eq!(protected_phase(6.0, 6.0, v, 3.0), 0.0);
        }
    }
    #[test]
    fn native_expiry_precedes_second_visual_phase() {
        let mut status = protected_phase(6.0, 6.0, 1.0, 3.0);
        let mut visual = status;
        let mut secondary = 0;
        let mut ability = status;
        for _ in 0..61 {
            let was_above = status > 3.0;
            status -= 1.0 / 60.0;
            visual -= 1.0 / 60.0;
            ability -= 1.0 / 60.0;
            if was_above && status <= 3.0 {
                secondary += 1;
            }
        }
        assert!(status <= 0.0 && visual <= 0.0 && ability <= 0.0);
        assert_eq!(secondary, 0);
    }
}
