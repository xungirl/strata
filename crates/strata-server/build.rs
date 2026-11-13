//! Compile `proto/strata.proto` into Rust gRPC stubs at build time.
//! `protox` is a pure-Rust protobuf compiler, so no `protoc` binary is needed.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/strata.proto");
    let fds = protox::compile(["proto/strata.proto"], ["proto"])?;
    tonic_prost_build::configure().compile_fds(fds)?;
    Ok(())
}
