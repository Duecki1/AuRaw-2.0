use super::*;

pub(super) fn show_loading_thumbnail(
    ui: &mut Ui,
    app: &CalibRawApp,
    available: egui::Vec2,
) -> bool {
    let (Some(texture), Some([width, height])) = (
        app.develop_ui.loading_thumbnail.texture.as_ref(),
        app.develop_ui.loading_thumbnail.texture_size,
    ) else {
        return false;
    };
    if available.x <= 0.0 || available.y <= 0.0 || width == 0 || height == 0 {
        return false;
    }

    let (outer_rect, _) = ui.allocate_exact_size(available, Sense::hover());
    let image_size = fitted_image_size(outer_rect.size(), width as f32 / height as f32);
    let image_rect = Rect::from_center_size(outer_rect.center(), image_size);
    ui.painter().image(
        texture.id(),
        image_rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );

    if outer_rect.width() >= 104.0 && outer_rect.height() >= 48.0 {
        let badge_width = 132.0_f32.min(outer_rect.width() - 16.0);
        let badge_rect = Rect::from_center_size(outer_rect.center(), egui::vec2(badge_width, 32.0));
        ui.painter()
            .rect_filled(badge_rect, 16.0, Color32::from_black_alpha(190));
        ui.painter().text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Loading RAW…",
            egui::FontId::proportional(13.0),
            Color32::WHITE,
        );
    }
    true
}

pub(super) fn paint_textured_geometry_quad(
    ui: &Ui,
    texture_id: egui::TextureId,
    clip_rect: Rect,
    positions: [Pos2; 4],
    texture_uv: Rect,
) {
    let mut mesh = Mesh::with_texture(texture_id);
    let uvs = [
        texture_uv.left_top(),
        texture_uv.right_top(),
        texture_uv.right_bottom(),
        texture_uv.left_bottom(),
    ];
    for (pos, uv) in positions.into_iter().zip(uvs) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos,
            uv,
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    ui.painter_at(clip_rect).add(Shape::mesh(mesh));
}

pub(super) fn paint_textured_combined_geometry_mesh(
    ui: &Ui,
    texture_id: egui::TextureId,
    clip_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    texture_uv: Rect,
    source_uv: [f32; 4],
    crop_workspace: bool,
) {
    if lens_geometry.is_none() {
        let positions = source_uv_corners(source_uv).map(|point| {
            if crop_workspace {
                crop_workspace_source_to_screen(
                    clip_rect,
                    geometry,
                    source_width,
                    source_height,
                    point,
                )
            } else {
                final_geometry_source_to_screen(
                    clip_rect,
                    geometry,
                    source_width,
                    source_height,
                    point,
                )
            }
        });
        paint_textured_geometry_quad(ui, texture_id, clip_rect, positions, texture_uv);
        return;
    }

    let span_u = (source_uv[2] - source_uv[0]).abs().max(1e-6);
    let span_v = (source_uv[3] - source_uv[1]).abs().max(1e-6);
    let grid_x = ((source_width.max(1) as f32 * span_u / 96.0).ceil() as usize).clamp(16, 96);
    let grid_y = ((source_height.max(1) as f32 * span_v / 96.0).ceil() as usize).clamp(16, 96);
    let lens_geometry = lens_geometry.expect("lens geometry checked above");
    let mut mesh = Mesh::with_texture(texture_id);
    mesh.vertices.reserve((grid_x + 1) * (grid_y + 1));
    mesh.indices.reserve(grid_x * grid_y * 6);
    for gy in 0..=grid_y {
        let ty = gy as f32 / grid_y as f32;
        let raw_v = source_uv[1] + (source_uv[3] - source_uv[1]) * ty;
        let texture_v = texture_uv.top() + (texture_uv.bottom() - texture_uv.top()) * ty;
        for gx in 0..=grid_x {
            let tx = gx as f32 / grid_x as f32;
            let raw_u = source_uv[0] + (source_uv[2] - source_uv[0]) * tx;
            let texture_u = texture_uv.left() + (texture_uv.right() - texture_uv.left()) * tx;
            let corrected_uv = native_source_to_corrected_uv(
                lens_geometry,
                source_width,
                source_height,
                [raw_u, raw_v],
            );
            let pos = if crop_workspace {
                crop_workspace_source_to_screen(
                    clip_rect,
                    geometry,
                    source_width,
                    source_height,
                    corrected_uv,
                )
            } else {
                final_geometry_source_to_screen(
                    clip_rect,
                    geometry,
                    source_width,
                    source_height,
                    corrected_uv,
                )
            };
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv: Pos2::new(texture_u, texture_v),
                color: Color32::WHITE,
            });
        }
    }
    let stride = grid_x + 1;
    for gy in 0..grid_y {
        for gx in 0..grid_x {
            let a = (gy * stride + gx) as u32;
            let b = a + 1;
            let c = a + stride as u32;
            let d = c + 1;
            mesh.indices.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }
    ui.painter_at(clip_rect).add(Shape::mesh(mesh));
}

pub(super) fn source_uv_corners(source_uv: [f32; 4]) -> [[f32; 2]; 4] {
    [
        [source_uv[0], source_uv[1]],
        [source_uv[2], source_uv[1]],
        [source_uv[2], source_uv[3]],
        [source_uv[0], source_uv[3]],
    ]
}

pub(super) fn paint_final_geometry_texture(
    ui: &Ui,
    texture_id: egui::TextureId,
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    texture_uv: Rect,
    source_uv: [f32; 4],
) {
    if source_uv == [0.0, 0.0, 1.0, 1.0] {
        ui.painter_at(image_rect)
            .rect_filled(image_rect, 0.0, Color32::BLACK);
    }
    paint_textured_combined_geometry_mesh(
        ui,
        texture_id,
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        texture_uv,
        source_uv,
        false,
    );
}

pub(super) fn paint_final_geometry_overlay_texture(
    ui: &Ui,
    texture_id: egui::TextureId,
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    texture_uv: Rect,
    source_uv: [f32; 4],
) {
    if lens_geometry.is_none() {
        let positions = source_uv_corners(source_uv).map(|point| {
            final_geometry_source_to_screen(
                image_rect,
                geometry,
                source_width,
                source_height,
                point,
            )
        });
        paint_textured_geometry_quad(ui, texture_id, image_rect, positions, texture_uv);
        return;
    }
    paint_textured_combined_geometry_mesh(
        ui,
        texture_id,
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        texture_uv,
        source_uv,
        false,
    );
}

pub(super) fn paint_crop_workspace_texture(
    ui: &Ui,
    texture_id: egui::TextureId,
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    texture_uv: Rect,
    source_uv: [f32; 4],
) {
    if source_uv == [0.0, 0.0, 1.0, 1.0] {
        ui.painter_at(image_rect)
            .rect_filled(image_rect, 0.0, ui.visuals().panel_fill);
    }
    paint_textured_combined_geometry_mesh(
        ui,
        texture_id,
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        texture_uv,
        source_uv,
        true,
    );
}
