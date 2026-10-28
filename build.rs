fn main() {
    let proto = "docs/protocol/dolgorae/public/v1/dolgorae.proto";
    println!("cargo:rerun-if-changed={proto}");
    println!("cargo:rerun-if-changed=docs/protocol/dolgorae-public-v1.descriptor.pb");

    println!("cargo:rerun-if-changed=docs/protocol/dolgorae/public/v2/dolgorae.proto");
    tonic_prost_build::configure()
        .compile_protos(
            &[proto, "docs/protocol/dolgorae/public/v2/dolgorae.proto"],
            &["docs/protocol"],
        )
        .expect("checked public protobuf must compile");
}
