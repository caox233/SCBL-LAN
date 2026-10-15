//! Optional POD-only drawing extension; existing HostApiV1/PluginConfigV1 unchanged.
pub const HUD_EXPORT: &[u8] = b"scbl_mod_hud_snapshot_v1\0";
pub const HUD_ABI: u32 = 1;
pub const HUD_MAX_RECTS: usize = 8;
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct HudRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub rgba: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HudFrame {
    pub abi: u32,
    pub size: u32,
    pub count: u32,
    pub reserved: u32,
    pub rects: [HudRect; HUD_MAX_RECTS],
}
impl Default for HudFrame {
    fn default() -> Self {
        Self {
            abi: HUD_ABI,
            size: core::mem::size_of::<Self>() as u32,
            count: 0,
            reserved: 0,
            rects: [HudRect::default(); HUD_MAX_RECTS],
        }
    }
}
pub type HudSnapshotFn = unsafe extern "system" fn(*mut HudFrame, u32) -> i32;
impl HudFrame {
    pub fn valid(&self) -> bool {
        self.abi == HUD_ABI
            && self.size == core::mem::size_of::<Self>() as u32
            && self.reserved == 0
            && self.count <= HUD_MAX_RECTS as u32
            && self.rects[..self.count.min(HUD_MAX_RECTS as u32) as usize]
                .iter()
                .all(|r| {
                    [r.x, r.y, r.w, r.h]
                        .iter()
                        .all(|x| x.is_finite() && *x >= 0.0 && *x <= 1.0)
                        && r.x + r.w <= 1.0
                        && r.y + r.h <= 1.0
                })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_extension_rejects_bad_layout_and_nonfinite_geometry() {
        let mut f = HudFrame::default();
        assert!(f.valid());
        assert_eq!(core::mem::size_of::<HudRect>(), 20);
        assert_eq!(core::mem::size_of::<HudFrame>(), 176);
        f.count = 9;
        assert!(!f.valid());
        f.count = 1;
        f.rects[0].x = f32::NAN;
        assert!(!f.valid());
        f.rects[0] = HudRect {
            x: 0.9,
            w: 0.2,
            ..HudRect::default()
        };
        assert!(!f.valid());
        f.rects[0] = HudRect::default();
        f.abi = 2;
        assert!(!f.valid());
    }
}
