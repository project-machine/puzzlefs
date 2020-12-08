## PuzzleFS filesystem metadata

    // struct rootfs is the entry point for the filesystem metadata. it has a
    // list of metadata objects, the 0th being the "highest" in the stack.
    //
    // this list of metadatas can either be included at offsets in the rootfs
    // file, or referenced as other blobs.
    struct rootfs {
        u64 metadata_count;
        metadata_ref metadatas[];
    }

    enum metadata_type {
        local,
        blob,
    };
    struct metadata_ref {
        metadata_type type;
        union {
            u64 offset;
            hash_value blob;
        }
    }

    struct metadata {
        u64 inode_count;
        struct inode inodes[];
    }

    // defined in dirent.h
    enum inode_type {
        unknown,
        fifo,
        chr,
        dir,
        blk,
        reg,
        lnk,
        sock,
        /*
         * bsd style whiteout, mostly unused in linux. maybe we can teach
         * overlay about these?
         */
        wht,
    }

    struct dirent {
        u64 ino;
        string name;
    }

    // when set, also look at layers below for the dir list. when not set, this
    // dir list is complete. note that dirlists allow wht inode_types to white out
    // entries below them.
    #define DIR_LIST_LOOK_BELOW 1

    struct dir_list {
        u64 flags;
        u64 entries_len;
        dirent entries[];
    }

    struct chunk {
        hash_value blob;
        u64 chunk_num;
        u64 offset;
        u64 len;
    }

    struct chunk_list {
        u64 chunks_len;
        chunk chunks[];
    }

    struct inode {
        u64 ino;
        inode_type type;
        union {
            // directory
            struct {
                u64 offset;
            },

            // file
            struct {
                u64 offest;
            },
        };
    }

### Reading compressed data at an offset

This is called "random access" in the compression format community. Mainly,
people suggest breaking things up into smaller chunks and compressing the
chunks individually (this would be after and independent of the chunking above,
which is likely to be some kind of Rabin fingerprinting style chunking).

It looks like zstd has some experimental out of tree support for it:

https://github.com/facebook/zstd/issues/395
https://github.com/facebook/zstd/tree/dev/contrib/seekable_format

So we should probably play around with that. Alternatively, we could add some
kind of wrapper in the specification about this for arbitrary compression
formats. But it's likely that the compression people themselves will implement
a better version of this, so it seems like we should try to use theirs first.

In any case, for now this document assumes this is possible without specifying
how it is done.

### Algorithm for generating a delta on top of an existing layer

given some new set of files S:

    create a .catar of S
    chunks = run chunking algorithm on S
    generate metadata for S

should we do some special handling for overlay -> bsd style whiteouts? since
we'll have direct mount support, maybe this is a "good time" to change the
convention, since the kernel can just interpret the thing for us correctly.

### Algorithm for finding inode n

Given a target inode `ino`:

    for each metadata_ref:
        i = binary_search(metadata_ref->inodes, ino)
        if i:
            return i

### Algorithm for looking up a dirlist

Given a target inode `ino`:

    dirlist = []
    for each metadata_ref:
        i = binary_search(metadata_ref->inodes, ino)
        if not i:
            continue // not every layer has to change every directory
        dl = resolve_dirlist_at_offset(i, metadata_ref)
        append_respecting_whiteouts(dirlist, dl)
        if !(dl->flags & DIR_LIST_LOOK_BELOW)
            break

### Algorithm for looking up file contents

Need to build a map of file length -> chunk contents. I believe this is
possible+efficient with some data structure like the xarray/maple tree that the
kernel has.

### Canonicalization

While the above metadata can be additive (i.e., it is explicit that puzzlefs
ignores metadata in lower metadata files for indoes whose metadata is present
in files above), the canonical representation of metadata for a puzzlefs
filesystem is as one single metadata layer (and some set of chunks for this
single filesystem represented by the chunking algorithm), i.e. everything is
included in the current file.

This means that the only thing left to do is define the ordering of things, and
the ordering should be the "sensible" order for objects: dirents are stored in
lexicographic order, inodes are stored by inode number, etc.
