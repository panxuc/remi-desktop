//! Tauri codegen, plus staging the art into `ui/assets/` for the webview to fetch.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    stage_assets();
    tauri_build::build();
}

/// The repo's Spine files are named `Q蕾米.json` and `leimi.png`; the webview fetches them by
/// URL, and non-ASCII in a URL is an avoidable class of bug. Copy them into `ui/assets/` under
/// ASCII names the renderer can hardcode.
///
/// `ui/assets/` is gitignored: it is generated on every build and must never be edited by hand.
fn stage_assets() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = crate_dir
        .parent()
        .and_then(Path::parent)
        .expect("crate is at <repo>/crates/remi-desktop");
    let src = repo.join("assets");
    let out = crate_dir.join("ui").join("assets");

    // Rebuild when the source art changes. The directory itself is watched too, so adding or
    // removing a GIF re-runs the staging rather than leaving a stale copy behind.
    println!("cargo:rerun-if-changed={}", src.display());
    println!(
        "cargo:rerun-if-changed={}",
        src.join("spine-asset").display()
    );
    // ⚠️ The *destination* is an input as well, and not watching it is a silent bug. Cargo caches
    // one build-script result per feature set: turning `gif-fallback` back off finds a fresh
    // no-feature fingerprint, skips the script, and embeds the art the previous build left on
    // disk — the feature appears to do nothing. Watching `ui/assets` makes that mtime change the
    // thing that invalidates the cache. At worst it costs one extra re-run, which then finds
    // everything already in place and writes nothing.
    println!("cargo:rerun-if-changed={}", out.display());

    fs::create_dir_all(&out).unwrap_or_else(|e| panic!("creating {}: {e}", out.display()));

    let spine = src.join("spine-asset");
    copy_if_stale(&spine.join("Q蕾米.json"), &out.join("remi.json"));
    copy_if_stale(&spine.join("leimi.png"), &out.join("remi.png"));
    stage_atlas(&spine.join("leimi.atlas"), &out.join("remi.atlas"));
    stage_gifs(&src, &out.join("gif"));
}

/// Everything under `ui/` is embedded in the executable by `generate_context!`, so staging a file
/// is the same decision as shipping it. The GIF art is 8.5 MiB of that, hence the feature.
///
/// ⚠️ Not `cfg!(feature = ...)`: a build script is compiled without its own crate's features, so
/// that macro is always false here and would silently disable the feature for good. `CARGO_FEATURE_*`
/// is the only thing cargo actually sets.
fn gif_fallback_enabled() -> bool {
    std::env::var_os("CARGO_FEATURE_GIF_FALLBACK").is_some()
        || std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
}

/// An atlas names its page image on its own line, so renaming `leimi.png` means rewriting that
/// line — copying the file is not enough.
fn stage_atlas(src: &Path, dst: &Path) {
    const PAGE: &str = "leimi.png";
    const RENAMED: &str = "remi.png";

    let atlas =
        fs::read_to_string(src).unwrap_or_else(|e| panic!("reading {}: {e}", src.display()));
    if !atlas.lines().any(|line| line.trim() == PAGE) {
        panic!(
            "{}: expected a page-image line `{PAGE}` to rewrite, found none — \
             the atlas was re-exported under a different page name",
            src.display()
        );
    }

    let mut rewritten: String = atlas
        .lines()
        .map(|line| if line.trim() == PAGE { RENAMED } else { line })
        .collect::<Vec<_>>()
        .join("\n");
    rewritten.push('\n');
    write_if_changed(dst, rewritten.as_bytes());
}

/// The GIF fallback renderer (`ui/renderer/gif.js`) addresses these by their original names, so
/// they are staged as-is — only the Spine files need renaming.
fn stage_gifs(src: &Path, dst_dir: &Path) {
    if !gif_fallback_enabled() {
        // Turning the feature back off has to *remove* the art, not merely stop copying it:
        // `ui/assets/` survives between builds, so a directory left over from a `--features
        // gif-fallback` build would keep being embedded and the feature would look like it does
        // nothing.
        if dst_dir.exists() {
            fs::remove_dir_all(dst_dir)
                .unwrap_or_else(|e| panic!("removing {}: {e}", dst_dir.display()));
        }
        return;
    }

    fs::create_dir_all(dst_dir).unwrap_or_else(|e| panic!("creating {}: {e}", dst_dir.display()));
    let entries = fs::read_dir(src).unwrap_or_else(|e| panic!("reading {}: {e}", src.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("gif") {
            let name = path.file_name().expect("read_dir entry has a file name");
            copy_if_stale(&path, &dst_dir.join(name));
        }
    }
}

/// `remi.png` is 700 KB and the atlas is copied on every `cargo build` that touches this crate;
/// skipping unchanged files keeps the incremental loop from rewriting a megabyte each time.
fn copy_if_stale(src: &Path, dst: &Path) {
    if let (Ok(s), Ok(d)) = (fs::metadata(src), fs::metadata(dst))
        && let (Ok(s), Ok(d)) = (s.modified(), d.modified())
        && d >= s
    {
        return;
    }
    fs::copy(src, dst)
        .unwrap_or_else(|e| panic!("staging {} -> {}: {e}", src.display(), dst.display()));
}

fn write_if_changed(dst: &Path, contents: &[u8]) {
    if fs::read(dst).is_ok_and(|existing| existing == contents) {
        return;
    }
    fs::write(dst, contents).unwrap_or_else(|e| panic!("writing {}: {e}", dst.display()));
}
