use super::*;
use std::collections::HashMap;

#[test]
fn global_dark_style_survives_system_theme_changes() {
    for startup_theme in [egui::Theme::Light, egui::Theme::Dark] {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(
            egui::RawInput {
                system_theme: Some(startup_theme),
                ..Default::default()
            },
            |_| {},
        );
        configure_style(&ctx);
        let expected = ctx.global_style();
        for system_theme in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
            let _ = ctx.run_ui(
                egui::RawInput {
                    system_theme: Some(system_theme),
                    ..Default::default()
                },
                |ui| {
                    assert_eq!(ctx.theme(), egui::Theme::Dark);
                    assert_eq!(ui.style().spacing.scroll, expected.spacing.scroll);
                    assert_eq!(ui.visuals(), &expected.visuals);
                },
            );
        }
    }
}

pub(super) fn assert_horizontal_editor_scrollbar_tracks_overflow(
    mut render: impl FnMut(&mut egui::Ui, &mut String),
) {
    for width in [300.0, 300.5, 301.2] {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        configure_style(&ctx);
        for (case, contents, expected) in [
            ("empty", String::new(), false),
            ("short", "short line".to_owned(), false),
            ("short multiline", "short line\n".repeat(20), false),
            ("long", "long-line-".repeat(100), true),
            ("short again", "short line".to_owned(), false),
        ] {
            let mut contents = contents;
            let mut output = None;
            for frame in 0..5 {
                output = Some(ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(380.0, 394.0),
                        )),
                        time: Some(ctx.input(|input| input.time) + 0.5),
                        ..Default::default()
                    },
                    |ui| {
                        ui.set_width(width);
                        render(ui, &mut contents);
                        assert!(
                            ui.min_rect().width() <= width + 1.0,
                            "{case}, frame {frame}"
                        );
                    },
                ));
            }
            let output = output.unwrap();
            let scrollbar = output.shapes.iter().any(|shape| match &shape.shape {
                egui::Shape::Rect(rect) => {
                    rect.rect.width() > 8.0
                        && rect.rect.height() <= 6.0
                        && rect.fill != Color32::TRANSPARENT
                }
                _ => false,
            });
            assert_eq!(
                scrollbar, expected,
                "horizontal scrollbar: {case}, width {width}"
            );
        }
    }
}

/// Small software mesh renderer for review artifacts. It renders egui's actual
/// tessellated output and texture atlas, without opening or changing user apps.
pub(super) fn rasterize(
    ctx: &egui::Context,
    output: egui::FullOutput,
    textures: &HashMap<egui::TextureId, egui::ColorImage>,
) -> resvg::tiny_skia::Pixmap {
    let scale = 2.0;
    let width = (layout::WINDOW_WIDTH * scale) as usize;
    let height = (layout::WINDOW_HEIGHT * scale) as usize;
    let mut pixels = vec![0u8; width * height * 4];
    for clipped in ctx.tessellate(output.shapes, 1.0) {
        let egui::epaint::Primitive::Mesh(mesh) = clipped.primitive else {
            continue;
        };
        let Some(texture) = textures.get(&mesh.texture_id) else {
            continue;
        };
        for indices in mesh.indices.as_chunks::<3>().0 {
            let v = [
                mesh.vertices[indices[0] as usize],
                mesh.vertices[indices[1] as usize],
                mesh.vertices[indices[2] as usize],
            ];
            let p = v.map(|v| v.pos * scale);
            let determinant =
                (p[1].y - p[2].y) * (p[0].x - p[2].x) + (p[2].x - p[1].x) * (p[0].y - p[2].y);
            if determinant.abs() < 1e-5 {
                continue;
            }
            let x0 = p
                .iter()
                .map(|p| p.x)
                .fold(f32::INFINITY, f32::min)
                .max(clipped.clip_rect.left() * scale)
                .max(0.0) as usize;
            let y0 = p
                .iter()
                .map(|p| p.y)
                .fold(f32::INFINITY, f32::min)
                .max(clipped.clip_rect.top() * scale)
                .max(0.0) as usize;
            let x1 = p
                .iter()
                .map(|p| p.x)
                .fold(0.0, f32::max)
                .min(clipped.clip_rect.right() * scale)
                .ceil()
                .min(width as f32) as usize;
            let y1 = p
                .iter()
                .map(|p| p.y)
                .fold(0.0, f32::max)
                .min(clipped.clip_rect.bottom() * scale)
                .ceil()
                .min(height as f32) as usize;
            for y in y0..y1 {
                for x in x0..x1 {
                    let px = x as f32 + 0.5;
                    let py = y as f32 + 0.5;
                    let a = ((p[1].y - p[2].y) * (px - p[2].x) + (p[2].x - p[1].x) * (py - p[2].y))
                        / determinant;
                    let b = ((p[2].y - p[0].y) * (px - p[2].x) + (p[0].x - p[2].x) * (py - p[2].y))
                        / determinant;
                    let c = 1.0 - a - b;
                    if a < 0.0 || b < 0.0 || c < 0.0 {
                        continue;
                    }
                    let uv = v[0].uv.to_vec2() * a + v[1].uv.to_vec2() * b + v[2].uv.to_vec2() * c;
                    let sample = texture[(
                        (uv.x * texture.width() as f32).clamp(0.0, (texture.width() - 1) as f32)
                            as usize,
                        (uv.y * texture.height() as f32).clamp(0.0, (texture.height() - 1) as f32)
                            as usize,
                    )]
                        .to_array();
                    let mut color = [0.0; 4];
                    for i in 0..4 {
                        color[i] = (f32::from(v[0].color.to_array()[i]) * a
                            + f32::from(v[1].color.to_array()[i]) * b
                            + f32::from(v[2].color.to_array()[i]) * c)
                            * f32::from(sample[i])
                            / 255.0;
                    }
                    let offset = (y * width + x) * 4;
                    let alpha = color[3] / 255.0;
                    for i in 0..4 {
                        pixels[offset + i] = (color[i]
                            + f32::from(pixels[offset + i]) * (1.0 - alpha))
                            .clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }
    resvg::tiny_skia::Pixmap::from_vec(
        pixels,
        resvg::tiny_skia::IntSize::from_wh(width as u32, height as u32).unwrap(),
    )
    .unwrap()
}
