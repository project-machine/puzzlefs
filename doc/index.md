# PuzzleFS format

Puzzlefs consists of two parts: a metadata format for inode information, and
actual filesystem data chunks, defined by various chunking algorithms.

All puzzlefs blobs are wrapped in the following structure:

    enum hash {
        sha256,
    }

    typedef hash_value byte[32] // for sha256

    enum blob_type {
        root,
        metadata,
        file,
    }

    type puzzlefs_blob {
        enum hash;
        u64 references_len;
        hash_value references[];
        blob_type type;
        // followed by the actual blob
    }

## Metadata

See metadata.md

## Chunking

See chunking.md
