//! Compiles `proto/` with protox (pure Rust), so builds need no system `protoc`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    let files = [
        "doppel/common/v1/common.proto",
        "doppel/agent/v1/agent.proto",
        "doppel/vmm/v1/vmm.proto",
        "doppel/daemon/v1/daemon.proto",
        "doppel/bundle/v1/bundle.proto",
    ];
    for file in files {
        println!("cargo:rerun-if-changed={}", root.join(file).display());
    }
    let descriptors = protox::compile(files, [&root])?;
    prost_build::Config::new()
        .bytes(["."])
        .compile_fds(descriptors)?;
    Ok(())
}
