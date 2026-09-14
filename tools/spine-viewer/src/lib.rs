//! Loading the Spine asset, shared by the two binaries in this crate: the interactive
//! `spine-viewer` and the deterministic `spine-export`.
//!
//! Everything here uses the official Spine 4.2 C runtime via `rusty_spine`, so what comes out is
//! what a real runtime would draw — no hand-rolled skinning to distrust.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use macroquad::prelude::*;
use rusty_spine::controller::SkeletonController;
use rusty_spine::{Atlas, AnimationStateData, SkeletonJson};

/// Stashed on each atlas page so `attachment_renderer_object` can hand it back.
pub struct SpineTex(pub Texture2D);

/// Find the `.atlas` and `.json` in `dir` so we don't hardcode the Chinese filename.
pub fn find_assets(dir: &Path) -> (PathBuf, PathBuf) {
    let (mut atlas, mut json) = (None, None);
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
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

/// Loads the skeleton and hands back a controller plus the names and durations of every animation
/// worth playing. Installs the texture callbacks the C runtime needs on the way.
///
/// The caller picks the texture filter, but in practice both binaries want `Linear`: the atlas is
/// magnified in either of them — the exporter supersamples 4x, so even a 256px export renders
/// internally at 1024, well past the ~421px at which this art is 1:1 — and `Nearest` at any
/// magnification shows texel blocking along every edge.
pub fn load(dir: &Path, filter: FilterMode) -> (SkeletonController, Vec<(String, f32)>) {
    rusty_spine::extension::set_create_texture_cb(move |page, path| {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("texture {path}: {e}"));
        let tex = Texture2D::from_file_with_format(&bytes, None);
        tex.set_filter(filter);
        page.renderer_object().set(SpineTex(tex));
    });
    rusty_spine::extension::set_dispose_texture_cb(|page| unsafe {
        page.renderer_object().dispose::<SpineTex>();
    });

    let (atlas_path, json_path) = find_assets(dir);
    let atlas = Arc::new(Atlas::new_from_file(&atlas_path).expect("failed to load atlas"));
    let skeleton_data = Arc::new(
        SkeletonJson::new(atlas)
            .read_skeleton_data_file(&json_path)
            .expect("failed to parse skeleton"),
    );

    // Skip the empty setup pose (`0`) — it renders nothing and just looks broken.
    let animations: Vec<(String, f32)> = skeleton_data
        .animations()
        .filter(|a| a.duration() > 0.0)
        .map(|a| (a.name().to_owned(), a.duration()))
        .collect();
    assert!(!animations.is_empty(), "skeleton has no animations");

    let controller = SkeletonController::new(
        skeleton_data.clone(),
        Arc::new(AnimationStateData::new(skeleton_data)),
    );
    (controller, animations)
}

/// The world-space bounding box of everything the skeleton is currently drawing, as
/// `(min_x, min_y, max_x, max_y)` in skeleton units.
pub fn bounds(controller: &mut SkeletonController) -> (f32, f32, f32, f32) {
    let (mut x0, mut y0) = (f32::INFINITY, f32::INFINITY);
    let (mut x1, mut y1) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for r in controller.renderables() {
        // A renderable with no texture draws nothing, so it must not stretch the box.
        if r.attachment_renderer_object.is_none() {
            continue;
        }
        for v in &r.vertices {
            x0 = x0.min(v[0]);
            y0 = y0.min(v[1]);
            x1 = x1.max(v[0]);
            y1 = y1.max(v[1]);
        }
    }
    (x0, y0, x1, y1)
}

/// Draws the skeleton in world space, for a caller that has set a [`Camera2D`]. Spine is Y-up and
/// so is macroquad's camera space, so unlike the viewer's screen-space loop there is no flip here.
pub fn draw_world(controller: &mut SkeletonController) {
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
            .map(|(v, uv)| Vertex::new(v[0], v[1], 0.0, uv[0], uv[1], tint))
            .collect();
        draw_mesh(&Mesh {
            vertices,
            indices: r.indices.clone(),
            texture: Some(tex.0.clone()),
        });
    }
}
