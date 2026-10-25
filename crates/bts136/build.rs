fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../../proto/operations.proto");
    tonic_prost_build::configure()
        .compile_protos(&["../../proto/operations.proto"], &["../../proto"])?;
    Ok(())
}
