//! Compile the NVIDIA Audio2Face-3D `.proto` files required by the avatar's A2F client.
//!
//! Only the `a2f_controller.v1` surface is needed at runtime; the transitive message modules
//! come along for free. Keeping this narrow avoids pulling in every ACE service.
//!
//! Codegen uses `tonic_prost_build` (tonic 0.14+). The library depends on `tonic-prost`
//! so generated stubs resolve `tonic_prost::ProstCodec`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = "proto";
    let protos = [
        "proto/nvidia_ace.audio.v1.proto",
        "proto/nvidia_ace.status.v1.proto",
        "proto/nvidia_ace.animation_id.v1.proto",
        "proto/nvidia_ace.emotion_with_timecode.v1.proto",
        "proto/nvidia_ace.a2f.v1.proto",
        "proto/nvidia_ace.animation_data.v1.proto",
        "proto/nvidia_ace.controller.v1.proto",
        "proto/nvidia_ace.services.a2f_controller.v1.proto",
    ];

    for p in &protos {
        println!("cargo:rerun-if-changed={p}");
    }

    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(&protos, &[proto_dir])?;

    Ok(())
}
