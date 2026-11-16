//! Embeds the browser viewer's build, `web/dist`, when there is one: each
//! file becomes an `Asset`, its bytes compiled in. Without a build, the
//! gateway serves a page that says so.

use std::fmt::Write as _;
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

fn content_type(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "json" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "a build script reads Cargo's environment"
)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let dist = manifest.join("../../web/dist");
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
            let relative = file
                .strip_prefix(&dist)?
                .to_string_lossy()
                .replace('\\', "/");
            let Some(content_type) = content_type(file) else {
                continue;
            };
            // Vite names everything under assets/ by its content.
            let cache = if relative.starts_with("assets/") {
                "Cache::Immutable"
            } else {
                "Cache::Revalidate"
            };
            let absolute = std::fs::canonicalize(file)?;
            writeln!(
                code,
                "    Asset {{ path: {:?}, content_type: {content_type:?}, body: include_bytes!({:?}), cache: {cache} }},",
                format!("/{relative}"),
                absolute.display().to_string(),
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
