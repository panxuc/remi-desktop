//! Minimal Spine viewer, built to answer one question: does `assets/spine-asset`
//! already contain the seven animations we shipped as GIFs?
//!
//! Deliberately throwaway. It uses the official Spine 4.2 C runtime via
//! `rusty_spine`, so what you see is what a real runtime would draw -- no
//! hand-rolled skinning to distrust.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use macroquad::prelude::*;
use rusty_spine::controller::SkeletonController;
use rusty_spine::{Atlas, Physics, SkeletonJson};

/// Stashed on each atlas page so `attachment_renderer_object` can hand it back.
struct SpineTex(Texture2D);

/// Backgrounds worth checking the art against. The pet window is transparent, so
/// the checker and the magenta are the ones that expose bad alpha edges.
const BACKDROPS: [(&str, Color); 4] = [
    ("checker", BLANK),
    ("dark", Color::new(0.12, 0.12, 0.14, 1.0)),
    ("light", Color::new(0.94, 0.94, 0.96, 1.0)),
    ("magenta", Color::new(1.0, 0.0, 1.0, 1.0)),
];

fn window_conf() -> Conf {
    Conf {
        window_title: "spine-viewer".to_owned(),
        window_width: 900,
        window_height: 720,
        high_dpi: true,
        ..Default::default()
    }
}

/// Find the `.atlas` and `.json` in `dir` so we don't hardcode the Chinese filename.
fn find_assets(dir: &Path) -> (PathBuf, PathBuf) {
    let (mut atlas, mut json) = (None, None);
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        match path.extension().and_then(|e| e.to_str()) {
            Some("atlas") => atlas = Some(path),
            // `.json` is the skeleton; there is no other json in that folder.
            Some("json") => json = Some(path),
            _ => {}
        }
    }
    (
        atlas.expect("no .atlas file found"),
        json.expect("no .json skeleton found"),
    )
}

fn draw_checker(cell: f32) {
    let (w, h) = (screen_width(), screen_height());
    clear_background(Color::new(0.82, 0.82, 0.84, 1.0));
    let (mut row, mut y) = (0, 0.0);
    while y < h {
        let mut x = if row % 2 == 0 { 0.0 } else { cell };
        while x < w {
            draw_rectangle(x, y, cell, cell, Color::new(0.70, 0.70, 0.73, 1.0));
            x += cell * 2.0;
        }
        y += cell;
        row += 1;
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../../assets/spine-asset".to_owned());
    let dir = PathBuf::from(dir);
    let (atlas_path, json_path) = find_assets(&dir);

    // The C runtime asks us to load textures; hand it a macroquad texture.
    rusty_spine::extension::set_create_texture_cb(|page, path| {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("texture {path}: {e}"));
        let tex = Texture2D::from_file_with_format(&bytes, None);
        tex.set_filter(FilterMode::Linear);
        page.renderer_object().set(SpineTex(tex));
    });
    rusty_spine::extension::set_dispose_texture_cb(|page| unsafe {
        page.renderer_object().dispose::<SpineTex>();
    });

    let atlas = Arc::new(Atlas::new_from_file(&atlas_path).expect("failed to load atlas"));
    let skeleton_data = Arc::new(
        SkeletonJson::new(atlas)
            .read_skeleton_data_file(&json_path)
            .expect("failed to parse skeleton"),
    );

    // Skip the empty setup pose (`0`) -- it renders nothing and just looks broken.
    let animations: Vec<(String, f32)> = skeleton_data
        .animations()
        .filter(|a| a.duration() > 0.0)
        .map(|a| (a.name().to_owned(), a.duration()))
        .collect();
    assert!(!animations.is_empty(), "skeleton has no animations");

    let mut controller = SkeletonController::new(
        skeleton_data.clone(),
        Arc::new(rusty_spine::AnimationStateData::new(skeleton_data)),
    );

    let mut current = 0usize;
    let mut looping = true;
    let mut paused = false;
    let mut speed = 1.0f32;
    let mut backdrop = 0usize;
    let mut show_help = true;

    // View transform: spine is Y-up and centred near the origin, the screen is Y-down.
    let mut zoom = 2.2f32;
    let mut pan = Vec2::new(0.0, 60.0);
    let mut elapsed = 0.0f32;

    let set_anim = |c: &mut SkeletonController, i: usize, looping: bool| {
        c.animation_state
            .set_animation_by_name(0, &animations[i].0, looping)
            .expect("animation missing");
    };
    set_anim(&mut controller, current, looping);

    loop {
        // ---- input ----
        let n = animations.len();
        if is_key_pressed(KeyCode::Right) || is_key_pressed(KeyCode::Down) {
            current = (current + 1) % n;
            elapsed = 0.0;
            set_anim(&mut controller, current, looping);
        }
        if is_key_pressed(KeyCode::Left) || is_key_pressed(KeyCode::Up) {
            current = (current + n - 1) % n;
            elapsed = 0.0;
            set_anim(&mut controller, current, looping);
        }
        for (i, key) in [
            KeyCode::Key1, KeyCode::Key2, KeyCode::Key3, KeyCode::Key4,
            KeyCode::Key5, KeyCode::Key6, KeyCode::Key7, KeyCode::Key8,
        ]
        .into_iter()
        .enumerate()
        {
            if is_key_pressed(key) && i < n {
                current = i;
                elapsed = 0.0;
                set_anim(&mut controller, current, looping);
            }
        }
        if is_key_pressed(KeyCode::Space) {
            paused = !paused;
        }
        if is_key_pressed(KeyCode::L) {
            looping = !looping;
            set_anim(&mut controller, current, looping);
        }
        if is_key_pressed(KeyCode::R) {
            elapsed = 0.0;
            set_anim(&mut controller, current, looping);
        }
        if is_key_pressed(KeyCode::B) {
            backdrop = (backdrop + 1) % BACKDROPS.len();
        }
        if is_key_pressed(KeyCode::H) {
            show_help = !show_help;
        }
        if is_key_pressed(KeyCode::LeftBracket) {
            speed = (speed - 0.25).max(0.25);
        }
        if is_key_pressed(KeyCode::RightBracket) {
            speed = (speed + 0.25).min(3.0);
        }
        let scroll = mouse_wheel().1;
        if scroll != 0.0 {
            zoom = (zoom * if scroll > 0.0 { 1.1 } else { 1.0 / 1.1 }).clamp(0.3, 12.0);
        }
        if is_mouse_button_down(MouseButton::Left) {
            let d = mouse_delta_position();
            // mouse_delta_position is normalised and inverted; scale back to pixels.
            pan.x -= d.x * screen_width() * 0.5;
            pan.y -= d.y * screen_height() * 0.5;
        }

        // ---- update ----
        let dt = if paused { 0.0 } else { get_frame_time() * speed };
        elapsed += dt;
        controller.update(dt, Physics::Update);

        // ---- draw ----
        let (name, color) = BACKDROPS[backdrop];
        if name == "checker" {
            draw_checker(16.0);
        } else {
            clear_background(color);
        }

        let origin = Vec2::new(screen_width() * 0.5 + pan.x, screen_height() * 0.5 + pan.y);
        for r in controller.renderables() {
            let Some(obj) = r.attachment_renderer_object else {
                continue;
            };
            let tex = unsafe { &*(obj as *const SpineTex) };
            let tint = Color::new(r.color.r, r.color.g, r.color.b, r.color.a);

            let vertices: Vec<Vertex> = r
                .vertices
                .iter()
                .zip(r.uvs.iter())
                .map(|(v, uv)| {
                    Vertex::new(
                        origin.x + v[0] * zoom,
                        origin.y - v[1] * zoom, // flip: spine Y-up -> screen Y-down
                        0.0,
                        uv[0],
                        uv[1],
                        tint,
                    )
                })
                .collect();

            draw_mesh(&Mesh {
                vertices,
                indices: r.indices.clone(),
                texture: Some(tex.0.clone()),
            });
        }

        // ---- overlay ----
        let (anim, dur) = &animations[current];
        let head = format!(
            "[{}/{}]  {}   {:.2}s   t={:.2}s   x{:.2}{}",
            current + 1,
            animations.len(),
            anim,
            dur,
            if *dur > 0.0 { elapsed % dur } else { 0.0 },
            speed,
            if paused { "   PAUSED" } else { "" }
        );
        let shade = if backdrop == 2 { BLACK } else { WHITE };
        draw_text(&head, 14.0, 26.0, 26.0, shade);
        if show_help {
            let help = [
                "1-8 / arrows  switch animation      space  pause",
                "[ ]  speed    L loop    R restart   B backdrop",
                "scroll zoom   drag pan              H  hide help",
            ];
            for (i, line) in help.iter().enumerate() {
                draw_text(line, 14.0, screen_height() - 58.0 + i as f32 * 20.0, 18.0, shade);
            }
        }

        next_frame().await;
    }
}
