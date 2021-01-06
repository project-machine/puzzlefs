# PuzzleFS format

Puzzlefs consists of two parts: a metadata format for inode information, and
actual filesystem data chunks, defined by various chunking algorithms.

All enums are encoded as u32s; all encodings are little endian.

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

Idea here is to define at least one algorithm for chunking filesystem data, but
allow for hot swapping of these algorithms and their parameters. Algorithms
should have one way to represent a file.

This should definitely be rabin fingerprint/rolling hash based. casync is the
obvious prior art here.
