//! Draw the existing optional gameplay POD snapshot; no native pawn reads here.
use hooks_plugin_api::hud::HudFrame;

// ImGui permits one active process-wide Context. Hold through Context::drop,
// including source-extracted render fixtures that run beside these tests.
#[cfg(test)]
pub(crate) static TEST_CONTEXT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn render(ui: &imgui::Ui) {
    if let Some(frame) = crate::plugin_loader::hud_snapshot() {
        draw_frame(ui, &frame);
    }
}

fn draw_frame(ui: &imgui::Ui, frame: &HudFrame) {
    let [width, height] = ui.io().display_size;
    if !frame.valid() || !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return;
    }
    let draw = ui.get_foreground_draw_list();
    for rect in &frame.rects[..frame.count as usize] {
        let [r, g, b, a] = rect.rgba.to_le_bytes();
        if a == 0 || rect.w == 0.0 || rect.h == 0.0 { continue; }
        draw.add_rect(
            [rect.x * width, rect.y * height],
            [(rect.x + rect.w) * width, (rect.y + rect.h) * height],
            imgui::ImColor32::from_rgba(r, g, b, a),
        ).filled(true).build();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hooks_plugin_api::hud::HudRect;
    fn rendered(frame: &HudFrame) -> (i32, i32, Vec<imgui::DrawVert>) {
        let _context_lease = TEST_CONTEXT_LOCK.lock().unwrap();
        let mut ctx=imgui::Context::create();ctx.set_ini_filename(None);
        ctx.io_mut().display_size=[1920.0,1080.0];ctx.io_mut().delta_time=0.016;
        ctx.fonts().build_rgba32_texture();draw_frame(ctx.frame(),frame);
        let data=ctx.render();
        (data.total_vtx_count,data.total_idx_count,data.draw_lists().flat_map(|d|d.vtx_buffer().iter().copied()).collect())
    }
    #[test]
    fn stamina_hud_geometry_and_color_reach_real_imgui_buffers() {
        let mut frame=HudFrame::default();frame.count=1;
        frame.rects[0]=HudRect{x:0.25,y:0.5,w:0.5,h:0.01,rgba:u32::from_le_bytes([145,223,211,122])};
        let (v,i,vertices)=rendered(&frame);assert_eq!((v,i),(4,6));
        assert_eq!(vertices[0].pos,[480.0,540.0]);
        assert!(vertices.iter().all(|v|v.col==[145,223,211,122]));
    }
    #[test]
    fn stamina_hud_empty_invalid_or_transparent_data_produces_no_vertices() {
        let mut f=HudFrame::default();assert_eq!(rendered(&f).0,0);
        f.count=9;assert_eq!(rendered(&f).0,0);
        f.count=1;f.rects[0].x=f32::NAN;assert_eq!(rendered(&f).0,0);
        f.rects[0]=HudRect{x:0.25,y:0.5,w:0.5,h:0.01,rgba:0};assert_eq!(rendered(&f).0,0);
    }
}
