@0xe0b6d460b1f22bb5;

using Metadata = import "metadata.capnp";

struct VerityData {
        digest@0: Data;
        verity@1: Data;
}

struct Rootfs {
        metadatas@0: List(Metadata.BlobRef);
        fsVerityData@1: List(VerityData);
        manifestVersion@2: UInt64;
}

