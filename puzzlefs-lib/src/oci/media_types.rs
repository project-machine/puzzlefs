pub trait PuzzleFSMediaType {
    fn name(&self) -> &'static str;
}

pub(crate) const PUZZLEFS_ROOTFS: &str = "application/vnd.puzzlefs.image.rootfs.v1";

pub struct Rootfs {}

impl PuzzleFSMediaType for Rootfs {
    fn name(&self) -> &'static str {
        PUZZLEFS_ROOTFS
    }
}

pub(crate) const PUZZLEFS_CHUNK_DATA: &str = "application/vnd.puzzlefs.image.filedata.v1";

pub struct Chunk {}

impl PuzzleFSMediaType for Chunk {
    fn name(&self) -> &'static str {
        PUZZLEFS_CHUNK_DATA
    }
}

pub(crate) const VERITY_ROOT_HASH_ANNOTATION: &str =
    "io.puzzlefsoci.puzzlefs.puzzlefs_verity_root_hash";
