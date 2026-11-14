//! Generates the Rust types for `engine.proto`, with serde impls that follow the
//! protobuf JSON mapping, so routes speak the same JSON as the TypeScript client.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");
    // SAFETY: build scripts are single threaded here and nothing else reads PROTOC.
    unsafe { std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?) };

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    let descriptor = out.join("engine_descriptor.bin");
    prost_build::Config::new()
        .file_descriptor_set_path(&descriptor)
        .compile_well_known_types()
        .extern_path(".google.protobuf", "::pbjson_types")
        .compile_protos(&["proto/kiln/engine/v1/engine.proto"], &["proto"])?;
    pbjson_build::Builder::new()
        .register_descriptors(&std::fs::read(&descriptor)?)?
        .build(&[".kiln.engine.v1"])?;
    Ok(())
}
