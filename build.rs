fn main() {
    let proto = "docs/protocol/dolgorae/public/v1/dolgorae.proto";
    println!("cargo:rerun-if-changed={proto}");
    println!("cargo:rerun-if-changed=docs/protocol/dolgorae-public-v1.descriptor.pb");

    prost_build::Config::new()
        .compile_protos(&[proto], &["docs/protocol"])
        .expect("checked public v1 protobuf must compile");
}
