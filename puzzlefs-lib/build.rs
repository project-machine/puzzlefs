fn main() {
    ::capnpc::CompilerCommand::new()
        .src_prefix("src/format")
        .file("src/format/metadata.capnp")
        .file("src/format/manifest.capnp")
        .run()
        .expect("compiling metadata schema");
}
