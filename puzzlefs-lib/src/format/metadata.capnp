@0x84ae5e6e88b7cbb7;

struct Chr {
    major@0: UInt64;
    minor@1: UInt64;
}

struct DirEntry {
    ino@0: UInt64;
    name@1: Data;
}

struct Dir {
    entries@0: List(DirEntry);
    lookBelow@1: Bool;
}

struct Blk {
    major@0: UInt64;
    minor@1: UInt64;
}

struct FileChunk {
    blob@0: BlobRef;
    len@1: UInt64;
}

struct BlobRef {
    digest@0: Data;
    offset@1: UInt64;
    compressed@2: Bool;
}

struct Xattr {
    key@0: Data;
    val@1: Data;
}

struct InodeAdditional {
    xattrs@0: List(Xattr);
    symlinkTarget@1: Data;
}

struct Inode {
    ino@0: UInt64;
    mode: union {
          unknown@1: Void;
          fifo@2: Void;
          chr@3: Chr;
          dir@4: Dir;
          blk@5: Blk;
          file@6: List(FileChunk);
          lnk@7: Void;
          sock@8: Void;
          wht@9: Void;
      }
    uid@10: UInt32;
    gid@11: UInt32;
    permissions@12: UInt16;
    additional@13: InodeAdditional;
}

struct InodeVector {
    inodes@0: List(Inode);
}
