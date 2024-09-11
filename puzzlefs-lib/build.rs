fn main() {
    ::capnpc::CompilerCommand::new()
        .src_prefix("src/format")
        .file("src/format/metadata.capnp")
        .run()
        .expect("compiling metadata schema");
}
