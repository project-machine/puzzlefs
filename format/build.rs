fn main() {
    ::capnpc::CompilerCommand::new()
        .file("metadata.capnp")
        .run()
        .expect("compiling metadata schema");
    ::capnpc::CompilerCommand::new()
        .file("manifest.capnp")
        .run()
        .expect("compiling manifest schema");
}
