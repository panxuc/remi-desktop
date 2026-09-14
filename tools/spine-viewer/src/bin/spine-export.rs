//! Renders one frame of one Spine animation to a square RGBA PNG, with true 8-bit alpha.
//!
//!     cargo run --bin spine-export -- --animation c --time 0 --size 256 --out icon.png
//!
//! This is where the pet's icon art comes from. The GIFs cannot serve that purpose: GIF carries
//! 1-bit transparency, so their edges were matted against a background before export and no amount
//! of rescaling recovers them. The Spine asset has the same poses with real alpha.
//!
//! Deterministic by construction — same arguments, same bytes — so the icon can be regenerated
//! rather than remembered.

use std::path::PathBuf;

use macroquad::prelude::*;
use rusty_spine::Physics;
use spine_viewer::{SpineTex, bounds, draw_world, load};

/// Animation time is stepped at a fixed rate rather than jumped to in one go, because the asset's
/// 80 physics constraints are stateful: hair and ribbons settle over time, and one giant `dt` puts
/// them somewhere a real playback never visits. A fixed step also makes the result reproducible.
const STEP: f32 = 1.0 / 60.0;

/// Each output pixel is averaged down from this many rendered pixels per side.
///
/// Supersampling rather than MSAA: an MSAA render target has to be resolved before it can be read
/// back, which is a step that can silently hand you the unresolved samples. Rendering big and
/// averaging down is dumber, exact, and produces the same edges.
const SUPERSAMPLE: u32 = 4;

struct Options {
    asset_dir: PathBuf,
    animation: String,
    time: f32,
    size: u32,
    margin: f32,
    out: PathBuf,
}

fn parse_args() -> Options {
    let mut opts = Options {
        asset_dir: PathBuf::from("../../assets/spine-asset"),
        animation: "c".into(),
        time: 0.0,
        size: 256,
        margin: 0.02,
        out: PathBuf::from("frame.png"),
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let value = || {
            args.get(i + 1)
                .unwrap_or_else(|| panic!("{} needs a value", args[i]))
                .clone()
        };
        match args[i].as_str() {
            "--asset-dir" => opts.asset_dir = value().into(),
            "--animation" => opts.animation = value(),
            "--time" => opts.time = value().parse().expect("--time must be a number"),
            "--size" => opts.size = value().parse().expect("--size must be a number"),
            "--margin" => opts.margin = value().parse().expect("--margin must be a number"),
            "--out" => opts.out = value().into(),
            "--help" | "-h" => {
                println!(
                    "usage: spine-export [--asset-dir DIR] [--animation NAME] [--time SECONDS]\n\
                     \x20                  [--size PX] [--margin FRACTION] --out FILE.png"
                );
                std::process::exit(0);
            }
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    opts
}

fn window_conf() -> Conf {
    Conf {
        window_title: "spine-export".to_owned(),
        // A window has to exist for there to be a GL context; nothing is ever drawn into it.
        window_width: 320,
        window_height: 200,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    let opts = parse_args();
    // `Linear`: the 4x supersampled render magnifies the atlas even for a small export, and
    // `Nearest` there blocks up every edge. See `load`.
    let (mut controller, animations) = load(&opts.asset_dir, FilterMode::Linear);

    let Some((_, duration)) = animations.iter().find(|(n, _)| *n == opts.animation) else {
        let names: Vec<&str> = animations.iter().map(|(n, _)| n.as_str()).collect();
        panic!(
            "no animation named {:?}; this skeleton has {}",
            opts.animation,
            names.join(", ")
        );
    };
    assert!(
        opts.time <= *duration + f32::EPSILON,
        "--time {} is past the end of `{}` ({duration:.2}s)",
        opts.time,
        opts.animation
    );

    controller
        .animation_state
        .set_animation_by_name(0, &opts.animation, false)
        .expect("animation missing");

    // Step to the requested time. See `STEP`.
    let mut elapsed = 0.0;
    while elapsed < opts.time {
        let dt = STEP.min(opts.time - elapsed);
        controller.update(dt, Physics::Update);
        elapsed += dt;
    }
    // Physics settles during `update`, but the renderables are only built on demand; one zero-length
    // update guarantees the pose being measured is the pose being drawn.
    controller.update(0.0, Physics::Update);

    // ---- framing ----
    let (x0, y0, x1, y1) = bounds(&mut controller);
    let (w, h) = (x1 - x0, y1 - y0);
    let extent = w.max(h) * (1.0 + opts.margin * 2.0);
    let centre = vec2((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    println!(
        "`{}` at t={:.3}s: bounds {:.1} x {:.1} skeleton units, square extent {:.1}",
        opts.animation, opts.time, w, h, extent
    );

    // What the art can actually resolve, as opposed to what we are willing to render. Past this
    // the renderer is magnifying atlas texels and inventing nothing.
    let density = texture_density(&mut controller);
    println!(
        "atlas detail: {density:.2} texture px per skeleton unit -> this pose is 1:1 at {:.0}px; \
         {}x{} is a {:.2}x {}",
        extent * density,
        opts.size,
        opts.size,
        opts.size as f32 / (extent * density),
        if opts.size as f32 > extent * density { "magnification" } else { "minification" },
    );

    // ---- render, twice ----
    //
    // Reading alpha straight out of a transparent render target means trusting the blend mode to
    // have accumulated it correctly through 199 slots, which it does not: macroquad blends colour
    // with straight alpha, so partially covered pixels come back with their RGB already mixed
    // toward whatever the target was cleared to. Compositing the same frame over black and over
    // white instead recovers both exactly, whatever happened in between:
    //
    //     over black: Cb = C·α           over white: Cw = C·α + (1 − α)
    //     so  α = 1 − (Cw − Cb)   and   C = Cb / α
    let ss = opts.size * SUPERSAMPLE;
    let over_black = render(&mut controller, ss, centre, extent, BLACK);
    let over_white = render(&mut controller, ss, centre, extent, WHITE);

    let (big, clipped) = unmatte(&over_black, &over_white, ss);
    if clipped > 0 {
        // Additive slots break the two-render algebra: they add light that neither background
        // subtracts. The asset has exactly one such slot (`light`), used by the `light` animation.
        println!(
            "note: {clipped} pixels had out-of-range alpha and were clamped — \
             an additively blended slot is visible in this frame"
        );
    }

    let small = downsample(&big, ss, opts.size);
    let image = Image {
        bytes: small,
        width: opts.size as u16,
        height: opts.size as u16,
    };
    let out = opts.out.to_string_lossy().to_string();
    image.export_png(&out);

    let opaque = image.bytes[3..].iter().step_by(4).filter(|a| **a == 255).count();
    let edge = image.bytes[3..]
        .iter()
        .step_by(4)
        .filter(|a| **a > 0 && **a < 255)
        .count();
    println!(
        "wrote {out}: {0}x{0}, {opaque} opaque px, {edge} anti-aliased edge px",
        opts.size
    );
    std::process::exit(0);
}

/// Texture pixels per skeleton unit, measured over everything currently drawn.
///
/// Compares the area each triangle covers in the world against the area its UVs cover on the atlas
/// page, so it reports what the art really carries rather than what the export scale claims. Area
/// ratios rather than edge lengths, because meshes are deformed and their edges are not axis-aligned.
fn texture_density(controller: &mut rusty_spine::controller::SkeletonController) -> f32 {
    let (mut world, mut texels) = (0f64, 0f64);
    for r in controller.renderables() {
        let Some(obj) = r.attachment_renderer_object else {
            continue;
        };
        let tex = unsafe { &*(obj as *const SpineTex) };
        let (pw, ph) = (tex.0.width() as f64, tex.0.height() as f64);
        for tri in r.indices.chunks_exact(3) {
            let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
            let (v, u) = (&r.vertices, &r.uvs);
            world += cross(v[a], v[b], v[c]).abs() as f64 * 0.5;
            texels += cross(
                [u[a][0] * pw as f32, u[a][1] * ph as f32],
                [u[b][0] * pw as f32, u[b][1] * ph as f32],
                [u[c][0] * pw as f32, u[c][1] * ph as f32],
            )
            .abs() as f64
                * 0.5;
        }
    }
    if world <= 0.0 { 0.0 } else { (texels / world).sqrt() as f32 }
}

fn cross(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])
}

/// Draws the frame into an offscreen target cleared to `background`, and reads it back.
fn render(
    controller: &mut rusty_spine::controller::SkeletonController,
    size: u32,
    centre: Vec2,
    extent: f32,
    background: Color,
) -> Vec<u8> {
    let target = render_target(size, size);
    set_camera(&Camera2D {
        target: centre,
        // Maps a square of `extent` world units onto the target's [-1, 1] on both axes.
        zoom: vec2(2.0 / extent, 2.0 / extent),
        render_target: Some(target.clone()),
        ..Default::default()
    });
    clear_background(background);
    draw_world(controller);
    set_default_camera();
    target.texture.get_texture_data().bytes
}

/// Recovers straight-alpha RGBA from the two composites. Returns the pixels and how many had an
/// alpha outside `0..=1`, which only happens where something was blended additively.
fn unmatte(black: &[u8], white: &[u8], size: u32) -> (Vec<u8>, usize) {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let mut clipped = 0;
    for p in (0..out.len()).step_by(4) {
        // Averaging the three channels' estimates costs nothing and cancels the odd rounding
        // difference between them; in exact arithmetic they agree.
        let alpha: f32 = (0..3)
            .map(|c| 1.0 - (white[p + c] as f32 - black[p + c] as f32) / 255.0)
            .sum::<f32>()
            / 3.0;
        if !(-0.002..=1.002).contains(&alpha) {
            clipped += 1;
        }
        let alpha = alpha.clamp(0.0, 1.0);
        for c in 0..3 {
            // Un-premultiply. Below roughly one part in 255 there is no colour left to recover and
            // the division only amplifies rounding, so leave those pixels black and invisible.
            out[p + c] = if alpha > 1.0 / 255.0 {
                (black[p + c] as f32 / alpha).round().clamp(0.0, 255.0) as u8
            } else {
                0
            };
        }
        out[p + 3] = (alpha * 255.0).round() as u8;
    }
    (out, clipped)
}

/// Box-averages `size`-square RGBA down to `target`-square, in premultiplied alpha.
///
/// The premultiplication matters for the same reason it does in `tools/icon-gen`: averaging
/// straight RGBA lets the colour sitting under transparent pixels bleed into every edge.
/// `SUPERSAMPLE` is an integer ratio, so this is an exact box filter with no partial coverage.
fn downsample(src: &[u8], size: u32, target: u32) -> Vec<u8> {
    let n = (size / target) as usize;
    let (size, target) = (size as usize, target as usize);
    let mut out = Vec::with_capacity(target * target * 4);
    for ty in 0..target {
        for tx in 0..target {
            let (mut csum, mut asum) = ([0f32; 3], 0f32);
            for sy in ty * n..(ty + 1) * n {
                for sx in tx * n..(tx + 1) * n {
                    let i = (sy * size + sx) * 4;
                    let a = src[i + 3] as f32;
                    for c in 0..3 {
                        csum[c] += a * src[i + c] as f32;
                    }
                    asum += a;
                }
            }
            for c in 0..3 {
                out.push(if asum > 0.0 {
                    (csum[c] / asum).round().clamp(0.0, 255.0) as u8
                } else {
                    0
                });
            }
            out.push((asum / (n * n) as f32).round().clamp(0.0, 255.0) as u8);
        }
    }
    out
}
