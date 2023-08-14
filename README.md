# PuzzleFS [![Build Status](https://github.com/anuvu/puzzlefs/workflows/ci/badge.svg?branch=master)](https://github.com/anuvu/puzzlefs/actions)

PuzzleFS is a next-generation container filesystem.

## Design Goals

* Do computation when we want to, i.e.:
    * Image building should be fast
    * Image mounting/reading should be fast
    * Optional "canonicalization" step in the middle
* No full-tree walk required
    * mtree style walks of filesystems are not necessary with clever use of
      overlay
    * casync style generate-a-tar-then-diff is more for general purpose use
      where you don't want to have a special filesystem setup
* Be simple enough to decode in the kernel
    * A primary motivator for our working on this at Cisco is direct-mount
      support

## Abstract
Puzzlefs is a container filesystem designed to address the limitations of the
existing OCI format. The main goals of the project are reduced duplication,
reproducible image builds, direct mounting support and memory safety
guarantees, some inspired by the
[OCIv2](https://hackmd.io/@cyphar/ociv2-brainstorm) design document.

Reduced duplication is achieved using the content defined chunking algorithm
FastCDC. This implementation allows chunks to be shared among layers. Building
a new layer starting from an existing one allows reusing most of the chunks.

Another goal of the project is reproducible image builds, which is achieved by
defining a canonical representation of the image format.

Direct mounting support is a key feature of puzzlefs and, together with
fs-verity, it provides data integrity. Currently, puzzlefs is implemented as a
userspace filesystem (FUSE). A read-only kernel filesystem driver is underway.

Lastly, memory safety is critical to puzzlefs, leading to the decision to
implement it in Rust. Another goal is to share the same code between user space
and kernel space in order to provide one secure implementation.

## OCIv2 Design doc

https://hackmd.io/@cyphar/ociv2-brainstorm

For the most part, I think this addresses things there except for two:

* Explicit Minimal Metadata: this is mostly unaddressed because I didn't think
  about it very hard; there's no reason we couldn't just drop e.g. block
  devices from the spec, or at least add a note about discouraging their use.
  Perhaps we should make mtimes and such optional? But then canonicalization
  would be harder. Maybe this should be specified at image build time, sort of
  like the chunking algorithm is in our design.

* Lazy fetch support: this seems directly at odds with the "direct mount"
  support at least if the direct mount code is to live in the kernel; we
  probably don't want to implement lazy fetch directly in the kernel, because
  it involves the network and lots of other stuff. However, this would be
  relatively easy to do using fuse, which suggests that perhaps we should
  choose a good language (e.g. rust :) for the implementation so that we could
  use the same code in the kernel and userspace, thus easily supporting this
  one.

## Getting started
### Build dependencies
Puzzlefs is written in rust, which you can download from https://www.rust-lang.org/tools/install.
It requires a [nightly toolchain](https://rust-lang.github.io/rustup/concepts/channels.html#working-with-nightly-rust) which you can add with `rustup toolchain install nightly`.

The [capnp tool](https://capnproto.org/install.html) is required for
autogenerating rust code from the capnproto schema language. This is done at
build time using the [capnpc crate](https://docs.rs/capnpc/latest/capnpc/).

### How to build
Run `make` (or `cargo build`) for the debug build and `make release` (`cargo build --release`) for the release build. The
resulting binaries are in `target/debug/puzzlefs` and
`target/release/puzzlefs`, respectively.

### Running tests
To run the tests, run `make check`.

The tests require
[skopeo](https://github.com/containers/skopeo/blob/main/install.md) and
[umoci](https://umo.ci/) to be installed. It also requires root to run the
`test_fs_verity` test.

### Building a puzzlefs image
To build a puzzlefs image, you need to specify a directory with the root
filesystem you want included in your image. For example:
```
$ tree /tmp/example-rootfs
/tmp/example-rootfs
├── algorithms
│   └── binary-search.txt
└── lorem_ipsum.txt

2 directories, 2 files
```

Then run:
```
$ cargo run --release -- build /tmp/example-rootfs /tmp/puzzlefs-image puzzlefs_example
puzzlefs image manifest digest: 9ac9abc098870c55cc61431dae8635806273d8f61274d34bec062560e79dc2f5
```
This builds a puzzlefs image with the above root filesystem in `/tmp/puzzlefs-image`, with the tag `puzzlefs_example`.
It also outputs the image's manifest digest, which is useful for verifying the integrity of the image using [fs-verity](https://www.kernel.org/doc/html/next/filesystems/fsverity.html).

For additional build options, run `puzzlefs build -h`.

### Mounting a puzzlefs image
To mount the above puzlefs image, first we need to create a mountpoint:
```
mkdir /tmp/mounted_image
```
Then run `puzzlefs mount` with the location of the puzzlefs image, the image tag and the mountpoint:
```
$ cargo run --release -- mount /tmp/puzzlefs-image puzzlefs_example /tmp/mounted_image
```

If everything was successful, you will see a `fuse` entry in the output of `mount`.
```
$ mount
...
/dev/fuse on /tmp/mounted_image type fuse (rw,nosuid,nodev,relatime,user_id=1000,group_id=1000)
```

and the mountpoint contains the rootfs:
```
$ tree /tmp/mounted_image
/tmp/mounted_image
├── algorithms
│   └── binary-search.txt
└── lorem_ipsum.txt

2 directories, 2 files
```

If you want to enable `fs-verity` checks, run
```
$ cargo run --release -- mount --digest 9ac9abc098870c55cc61431dae8635806273d8f61274d34bec062560e79dc2f5 /tmp/puzzlefs-image puzzlefs_example /tmp/mounted_image
```

This only works if `fsverity` is [supported and enabled](https://www.kernel.org/doc/html/latest/filesystems/fsverity.html#filesystem-support) in the underlying filesystem on which the puzzlefs image resides.
Otherwise you might get an error like this:
```
$ journalctl --since "2 min ago" | grep puzzlefs
Jul 13 18:37:30 archlinux-cisco puzzlefs[305462]: mount_background failed: fs error: fs error: Inappropriate ioctl for device (os error 25)
```
For debugging purposes you can use the `RUST_LOG` environment variable together with `-f` flag of mount:
```
$ RUST_LOG=DEBUG cargo run --release -- mount -f /tmp/puzzlefs-image puzzlefs_example /tmp/mounted_image
[2023-07-13T16:08:27Z INFO  fuser::session] Mounting /tmp/mounted_image
[2023-07-13T16:08:27Z DEBUG fuser::mnt::fuse_pure] fusermount:
[2023-07-13T16:08:27Z DEBUG fuser::mnt::fuse_pure] fusermount:
[2023-07-13T16:08:27Z DEBUG fuser::request] FUSE(  2) ino 0x0000000000000000 INIT kernel ABI 7.38, capabilities 0x73fffffb, max readahead 131072
[2023-07-13T16:08:27Z DEBUG fuser::request] INIT response: ABI 7.8, flags 0x1, max readahead 131072, max write 16777216
...
```

For additional mount options, run `puzzlefs mount -h`.

### Umounting a puzzlefs image
If you have specified the `-f` flag to `mount`, simply press `Ctrl-C`.

Otherwise, run `fusermount -u /tmp/mounted_image`. You will need to have `fuse` package installed.

### Inspecting a puzzlefs image
```
$ cd /tmp/puzzlefs-image
$ cat index.json | jq .
{
  "schemaVersion": -1,
  "manifests": [
    {
      "digest": "sha256:0efa2a4b490abb02a5b9b5f2d43c8262643dba48c67f14b236df0a6f1ea745d8",
      "size": 272,
      "media_type": "application/vnd.puzzlefs.image.rootfs.v1",
      "annotations": {
        "org.opencontainers.image.ref.name": "puzzlefs_example"
      }
    }
  ],
  "annotations": {}
}
```
The `digest` specifies the puzzlefs image manifest, which needs to be decoded using the `capnp tool` and the manifest schema
(assuming you've cloned puzzlefs in `~/puzzlefs`):
```
$ capnp convert binary:json ~/puzzlefs/format/manifest.capnp Rootfs < blobs/sha256/0efa2a4b490abb02a5b9b5f2d43c8262643dba48c67f14b236df0a6f1ea745d8

{ "metadatas": [{ "digest": [102, 197, 227, 96, 136, 156, 147, 144, 139, 154, 248, 228, 29, 161, 252, 228, 118, 222, 21, 44, 132, 0, 214, 164, 80, 74, 121, 156, 26, 85, 123, 57],
    "offset": "0",
    "compressed": false }],
  "fsVerityData": [
    { "digest": [102, 197, 227, 96, 136, 156, 147, 144, 139, 154, 248, 228, 29, 161, 252, 228, 118, 222, 21, 44, 132, 0, 214, 164, 80, 74, 121, 156, 26, 85, 123, 57],
      "verity": [224, 180, 63, 193, 142, 198, 24, 175, 78, 42, 126, 227, 253, 187, 102, 162, 31, 77, 85, 252, 205, 137, 198, 216, 26, 213, 113, 238, 144, 79, 93, 244] },
    { "digest": [239, 32, 68, 39, 210, 105, 37, 83, 131, 158, 224, 24, 162, 25, 96, 90, 140, 95, 158, 194, 97, 2, 153, 175, 54, 197, 216, 193, 115, 121, 62, 22],
      "verity": [196, 54, 71, 79, 3, 104, 3, 253, 163, 243, 85, 213, 67, 235, 144, 210, 20, 206, 160, 209, 75, 164, 93, 22, 79, 84, 41, 119, 20, 84, 64, 164] } ],
  "manifestVersion": "1" }
```
`metadatas` contains a list of layers (in this case only one) which can be further decoded (the sha of the blob is obtained by a decimal to hexadecimal conversion):
```
$ capnp convert binary:json ~/puzzlefs/format/metadata.capnp InodeVector < blobs/sha256/66c5e360889c93908b9af8e41da1fce476de152c8400d6a4504a799c1a557b39

{"inodes": [
  { "ino": "1",
    "mode": {"dir": {
      "entries": [
        { "ino": "2",
          "name": [97, 108, 103, 111, 114, 105, 116, 104, 109, 115] },
        { "ino": "3",
          "name": [108, 111, 114, 101, 109, 95, 105, 112, 115, 117, 109, 46, 116, 120, 116] } ],
      "lookBelow": false }},
    "uid": 1000,
    "gid": 1000,
    "permissions": 493 },
  { "ino": "2",
    "mode": {"dir": {
      "entries": [{ "ino": "4",
        "name": [98, 105, 110, 97, 114, 121, 45, 115, 101, 97, 114, 99, 104, 46, 116, 120, 116] }],
      "lookBelow": false }},
    "uid": 1000,
    "gid": 1000,
    "permissions": 493 },
  { "ino": "3",
    "mode": {"file": {"chunks": [{ "blob": {
        "digest": [239, 32, 68, 39, 210, 105, 37, 83, 131, 158, 224, 24, 162, 25, 96, 90, 140, 95, 158, 194, 97, 2, 153, 175, 54, 197, 216, 193, 115, 121, 62, 22],
        "offset": "0",
        "compressed": false },
      "len": "865" }]}},
    "uid": 1000,
    "gid": 1000,
    "permissions": 420 },
  { "ino": "4",
    "mode": {"file": {"chunks": [{ "blob": {
        "digest": [239, 32, 68, 39, 210, 105, 37, 83, 131, 158, 224, 24, 162, 25, 96, 90, 140, 95, 158, 194, 97, 2, 153, 175, 54, 197, 216, 193, 115, 121, 62, 22],
        "offset": "865",
        "compressed": false },
      "len": "278" }]}},
    "uid": 1000,
    "gid": 1000,
    "permissions": 420 } ]}
```

## Implementation

* `format/` is the code for serializing/de-serializing the puzzlefs format
* `builder/` is the code for building a puzzlefs image
* `extractor/` is the code for extracting a puzzlefs image
* `mount/` is the code for fuse mounting a puzzlefs image
* `exe/` is the executable frontend for the above

### Contributing

Contributions need to pass all static analysis.

In addition, all commits must include a `Signed-off-by:` line in their
description. This indicates that you certify [the following statement, known as
the Developer Certificate of Origin][dco]). You can automatically add this line
to your commits by using `git commit -s --amend`.

```
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.
1 Letterman Drive
Suite D4700
San Francisco, CA, 94129

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

[dco]: https://developercertificate.org/

### License

puzzlefs is released under the [Apache License, Version 2.0](LICENSE), and is:

Copyright (C) 2020-2021 Cisco Systems, Inc.
