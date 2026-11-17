//! Embeds the browser viewer's build, `target/viewer-web` (made by
//! `scripts/build-web.sh`), when there is one: each file becomes an `Asset`,
//! its bytes compiled in, with the `.br` and `.gz` copies the script made
//! of it and a hash of its bytes for its ETag. Without a build, the
//! gateway serves a page that says so.

use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};

fn files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn extension(path: &Path) -> Option<&str> {
    path.extension()?.to_str()
}

fn content_type(path: &Path) -> Option<&'static str> {
    Some(match extension(path)? {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "json" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => return None,
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "a build script reads Cargo's environment"
)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let dist = manifest.join("../../target/viewer-web");
    println!("cargo::rerun-if-changed={}", dist.display());
    let mut code =
        String::from("/// The browser viewer's build, if this doppel was built with one.\n");
    let mut found = Vec::new();
    if dist.join("index.html").is_file() {
        files(&dist, &mut found)?;
        found.sort();
    }
    if found.is_empty() {
        code.push_str("pub(crate) const BUILT: Option<Assets> = None;\n");
    } else {
        code.push_str("pub(crate) const BUILT: Option<Assets> = Some(Assets::new(&[\n");
        for file in &found {
            // Compressed copies ride with the file they're of.
            if matches!(extension(file), Some("br" | "gz")) {
                continue;
            }
            let relative = file
                .strip_prefix(&dist)?
                .to_string_lossy()
                .replace('\\', "/");
            let Some(content_type) = content_type(file) else {
                continue;
            };
            let bytes = std::fs::read(file)?;
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            let etag = format!("\"{:016x}\"", hasher.finish());
            // A copy is kept only when it's smaller.
            let copy = |suffix: &str| -> std::io::Result<String> {
                let mut name = file.clone().into_os_string();
                name.push(suffix);
                let path = PathBuf::from(name);
                Ok(match std::fs::metadata(&path) {
                    Ok(meta) if meta.len() < bytes.len() as u64 => format!(
                        "Some(include_bytes!({:?}))",
                        std::fs::canonicalize(&path)?.display().to_string()
                    ),
                    Ok(_) | Err(_) => "None".to_owned(),
                })
            };
            let absolute = std::fs::canonicalize(file)?;
            writeln!(
                code,
                "    Asset {{ path: {:?}, content_type: {content_type:?}, etag: {etag:?}, body: include_bytes!({:?}), brotli: {}, gzip: {} }},",
                format!("/{relative}"),
                absolute.display().to_string(),
                copy(".br")?,
                copy(".gz")?,
            )?;
        }
        code.push_str("]));\n");
    }
    std::fs::write(
        PathBuf::from(std::env::var("OUT_DIR")?).join("built.rs"),
        code,
    )?;
    Ok(())
}
