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
        hash_value blob;
        u64 offset;
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

    struct string {
        u64 len;
        char val[];
    };

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
        metadata_ref chunk;
        u64 file_offset;
        u64 len;
    }

    struct chunk_list {
        u64 chunks_len;
        chunk chunks[];
    }

    // this must be a fixed size so we can binary search over it easily
    struct inode {
        u64 ino;
        inode_type type;
        union {
            // fifo
            struct {
                /* nothing additional about fifos */
            },

            // chr
            struct {
                dev_t major;
                dev_t minor;
            },

            // dir
            struct {
                u64 dir_offset;
            },

            // blk; do we even want these? seems like maybe not since they're
            // system specific.
            struct {
                dev_t major;
                dev_t minor;
            },

            // reg
            struct {
                u64 file_size; /* total file size */
                u64 file_offset;
            },

            // lnk
            struct {
                string target[PATH_MAX];
                #define LNK_HARD 1
                #define LNK_SOFT 2
                u32 flags;
            },

            // sock; this seems like it should probably also be ignored?
            struct {
                /* no extra info */
            },

            // wht, unused for now
            struct {
                /* no extra info */
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
(Maybe not; it looks like zstd just chunks things up in a hard coded chunk
size, same as we would potentially do. Additionally, there are some proposals
out there for [storing other kinds of
metadata](https://github.com/containers/storage/pull/775) in the zstd skippable
frames, same as the `seekable_format` code does above, and that code currently
dies if it encounters skippable frames that it doesn't understand. So perhaps
we should just design our own seekable container format.)

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

Given a target inode `ino`:


    struct chunk_info {
        chunk *chunk;
        u64 offset_in_chunk;
        u64 len;
    }

    inode_chunks = xarray_new()
    size = binary_search(metadata_refs[0], ino).size

    def have_all_chunks(xa):
        max_seen = 0
        xa_for_each_range(xa, ent)
            // make sure that we have [0, size) populated
            // i.e. max_seen should == ent.min; max_seen = ent.max

    for each metadata_ref:
        if have_all_chunks(inode_chunks):
            break

        // add the inode chunks here
        i = binary_search(metadata_ref->inodes, ino)
        if not i:
            fail("incomplete inode definition")

        def add_chunk(xa, chunk):
            while 1:
                existing = xa_find(xa, chunk.chunk_offset, chunk.chunk_offset+chunk.len)
                if not existing:
                    xa_store_range(xa, chunk, chunk.file_offset, chunk.file_offset+len)
                    return

                // does the existing chunk cover the whole range? if so split
                // it and insert ours
                if existing.offset + existing.len > chunk.max:
                    xa_store_range(xa, existing, existing.offset, existing.offset - chunk.offset)
                    xa_store_range(xa, chunk, chunk.offset, chunk.len)
                    xa_store_range(xa, existing, existing.offset+chunk.len, existing.offset + existing.len - chunk.len)
                    return

                // special cases where it only covers the left or right half of
                // the range, handle as above


                // otherwise, remove the whole chunk, as our new chunk subsumes
                // it, and keep iterating
                xa_remove(xa, existing)

        for chunk in i.chunks:

                // uh oh, some existing chunk overlaps with our range. let's 
                find_file_offsets(inode_chunks, chunk)
                xa_store_range(chunk, chunk.file_offset, chunk.file_offset+len)

Now, when someone causes a fault at offset `off`:

    chunk_info = xa_load(inode_chunks, off)
    read_from_chunk(chunk, chunk_info.offeset_in_chunk + chunk.file_offset)
    /*
     * in reality this is a little more complicated, because you could ask for
     * more than a single chunk, but in that case you just xa_load(inode_chunks,
     * off+num_read)
     */


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
