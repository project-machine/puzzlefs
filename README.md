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
mkdir /tmp/mounted-image
```
Then run `puzzlefs mount` with the location of the puzzlefs image, the image tag and the mountpoint:
```
$ cargo run --release -- mount /tmp/puzzlefs-image puzzlefs_example /tmp/mounted-image
```

If everything was successful, you will see a `fuse` entry in the output of `mount`:
```
$ mount
...
/dev/fuse on /tmp/mounted-image type fuse (rw,nosuid,nodev,relatime,user_id=1000,group_id=1000)
```

and the following message in the journal:
```
$ journalctl --since "2 min ago" | grep puzzlefs
Aug 14 10:30:27 archlinux-cisco puzzlefs[55544]: Mounting /tmp/mounted-image
```

The mountpoint also contains the rootfs:
```
$ tree /tmp/mounted-image
/tmp/mounted-image
├── algorithms
│   └── binary-search.txt
└── lorem_ipsum.txt

2 directories, 2 files
```

For additional mount options, run `cargo run -- mount -h`.

### Mounting with fs-verity enabled
If you want to mount the filesystem with `fs-verity` authenticity protection, first enable `fs-verity` by running:
```
$ cargo run --release -- enable-fs-verity /tmp/puzzlefs-image puzzlefs_example 9ac9abc098870c55cc61431dae8635806273d8f61274d34bec062560e79dc2f5
```
This makes the data and metadata files readonly. Any reads of corrupted data will fail.

Then run mount with the `--digest` option:
```
$ cargo run --release -- mount --digest 9ac9abc098870c55cc61431dae8635806273d8f61274d34bec062560e79dc2f5 /tmp/puzzlefs-image puzzlefs_example /tmp/mounted-image
```
PuzzleFS now ensures that each file it opens has fs-verity enabled and that the
fs-verity measurement matches the fs-verity data stored in the manifest. The
image manifest's fs-verity digest is compared with the digest passed on the
command line via the `--digest` option.

This only works if `fsverity` is [supported and
enabled](https://www.kernel.org/doc/html/latest/filesystems/fsverity.html#filesystem-support)
in the underlying filesystem on which the puzzlefs image resides.  Otherwise
you might get an error like this when running `enable-fs-verity`:
```
Error: fs error: Inappropriate ioctl for device (os error 25)

Caused by:
    Inappropriate ioctl for device (os error 25)
```

To check wheter fs-verity is enabled, use `tune2fs`:
```
$ mount | grep -w '/'
/dev/mapper/MyVolGroup-root on / type ext4 (rw,relatime)

$ sudo tune2fs -l /dev/mapper/MyVolGroup-root | grep verity
Filesystem features:      has_journal ext_attr resize_inode dir_index filetype needs_recovery extent 64bit flex_bg sparse_super large_file huge_file dir_nlink extra_isize metadata_csum verity
```

To set up an 1MB loop device with an ext4 filesystem which supports `fs-verity`
and mount it under `/mnt`, run:
```
$ mktemp -u
/tmp/tmp.2CDDHVPLXp

$ touch /tmp/tmp.2CDDHVPLXp

$ dd if=/dev/zero of=/tmp/tmp.2CDDHVPLXp bs=1k count=1024
1024+0 records in
1024+0 records out
1048576 bytes (1.0 MB, 1.0 MiB) copied, 0.00203188 s, 516 MB/s

$ sudo losetup -f --show /tmp/tmp.2CDDHVPLXp
/dev/loop1

$ sudo mkfs -t ext4 -F -b4096 -O verity /dev/loop1
mke2fs 1.47.0 (5-Feb-2023)

Filesystem too small for a journal
Discarding device blocks: done
Creating filesystem with 256 4k blocks and 128 inodes

Allocating group tables: done
Writing inode tables: done
Writing superblocks and filesystem accounting information: done

$ sudo mount /dev/loop1 /mnt

$ sudo chown -R $(id -u):$(id -g) /mnt

$ sudo tune2fs -l /dev/loop1 | grep verity
Filesystem features:      ext_attr resize_inode dir_index filetype extent 64bit flex_bg metadata_csum_seed sparse_super large_file huge_file dir_nlink extra_isize metadata_csum verity
```

Now copy the puzzlefs image to `/mnt` and try the verity setup commands again.

### Debugging mount issues
When mounting a puzzlefs filesystem in the background (i.e. without `-f` flag),
then errors are logged into the journal, e.g.:
```
$ journalctl --since "2 min ago" | grep puzzlefs
Jul 13 18:37:30 archlinux-cisco puzzlefs[305462]: mount_background failed: fs error: fs error: Inappropriate ioctl for device (os error 25)
```
For debugging purposes you can use the [RUST_LOG](https://docs.rs/env_logger/latest/env_logger/) environment variable together with `-f` flag of mount:
```
$ RUST_LOG=DEBUG cargo run --release -- mount -f /tmp/puzzlefs-image puzzlefs_example /tmp/mounted-image
[2023-07-13T16:08:27Z INFO  fuser::session] Mounting /tmp/mounted-image
[2023-07-13T16:08:27Z DEBUG fuser::mnt::fuse_pure] fusermount:
[2023-07-13T16:08:27Z DEBUG fuser::mnt::fuse_pure] fusermount:
[2023-07-13T16:08:27Z DEBUG fuser::request] FUSE(  2) ino 0x0000000000000000 INIT kernel ABI 7.38, capabilities 0x73fffffb, max readahead 131072
[2023-07-13T16:08:27Z DEBUG fuser::request] INIT response: ABI 7.8, flags 0x1, max readahead 131072, max write 16777216
...
```

### Notification when the mountpoint is ready
#### Foreground mount (`mount -f`)
A named pipe can be passed to the `mount` command. Reading from this pipe is
blocking operation, waiting until puzzlefs signals that the mountpoint is
ready.  If the mount operation is successful, the `s` character is written to
the pipe, otherwise `f` is written. It is inspired by this [squashfuse
issue](https://github.com/vasi/squashfuse/issues/49#issuecomment-785398828).

The following script shows how to wait until the puzzlefs mountpoint is ready.
The script assumes there is puzzlefs image available at `/tmp/puzzlefs-image`
and the directory `/tmp/mounted-image` already exists.
```
#!/bin/bash
FIFO=$(mktemp -u)
mkfifo "$FIFO"
cargo run --release -- mount -i "$FIFO" -f /tmp/puzzlefs-image puzzlefs_example /tmp/mounted-image&
STATUS=$(head -c1 "$FIFO")
if [ "$STATUS" = "s" ]; then
	echo "Mountpoint contains:"
	ls /tmp/mounted-image
else
	echo "Mounting puzzlefs on /tmp/mounted-image failed"
fi
```

#### Background mount
When mounting in the background, puzzlefs uses an anonymous pipe to communicate
between its original process and the daemon it spawns in order to wait until
the mountpoint is available. This means that the  `puzzlefs mount` command
finishes its execution only after the mountpoint becomes ready.

### Umounting a puzzlefs image
If you have specified the `-f` flag to `mount`, simply press `Ctrl-C`.

Otherwise, run `fusermount -u /tmp/mounted-image`. You will need to have `fuse` package installed.

### Inspecting a puzzlefs image
```
$ cd /tmp/puzzlefs-image
$ cat index.json | jq
{
  "manifests": [
    {
      "annotations": {
        "org.opencontainers.image.ref.name": "puzzlefs_example"
      },
      "digest": "sha256:c9106994f5e18833e45164e2028431e9c822b4697172f8a997a0d9a3b0d26c9e",
      "mediaType": "application/vnd.oci.image.manifest.v1+json",
      "platform": {
        "architecture": "amd64",
        "os": "linux"
      },
      "size": 619
    }
  ],
  "schemaVersion": 2
}
```
`index.json` follows the [OCI Image Index Specification](https://github.com/opencontainers/image-spec/blob/main/image-index.md).

The digest tagged with the `puzzlefs_example` tag is an [OCI Image
Manifest](https://github.com/opencontainers/image-spec/blob/main/manifest.md)
with the caveat that `layers` are not applied in the usual way (i.e. by
stacking each one on top of one another). See below for details about the
PuzzleFS `layer` descriptors.

The Image Manifest looks like this:
```
$ cat blobs/sha256/c9106994f5e18833e45164e2028431e9c822b4697172f8a997a0d9a3b0d26c9e | jq
{
  "config": {
    "data": "e30=",
    "digest": "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
    "mediaType": "application/vnd.oci.empty.v1+json",
    "size": 2
  },
  "layers": [
    {
      "digest": "sha256:b7f1ee9373416a49835747455ec4d287bcccc5a4bf8c38156483d46b35ce4dbd",
      "mediaType": "application/vnd.puzzlefs.image.filedata.v1",
      "size": 27
    },
    {
      "annotations": {
        "io.puzzlefsoci.puzzlefs.puzzlefs_verity_root_hash": "7b22d0210c16134159be75d8239d100817b451591d39af2031d94ae84ac4f8c7"
      },
      "digest": "sha256:9e2edc6917b65606b1112ac8663665dfd2d945cfea960ca595accf790922b910",
      "mediaType": "application/vnd.puzzlefs.image.rootfs.v1",
      "size": 552
    }
  ],
  "schemaVersion": 2
}
```

There are two types of layer descriptors:
* `application/vnd.puzzlefs.image.rootfs.v1`: the PuzzleFS image rootfs which
  contains metadata in Capnproto format and must appear only once in the
  `layers` array
* `application/vnd.puzzlefs.image.filedata.v1`: a PuzzleFS data chunk generated
  by the FastCDC algorithm; usually there are multiple chunks in an image and
  they contain all the filesystem data

There is no extraction step for these layers, PuzzleFS mounts the filesystem by
reading the PuzzleFS image rootfs and using this metadata to combine the data
chunks back into the original files. In fact, the data chunks are part of the
OCI Image Manifest so that the other tools copy the image correctly. For
example, with skopeo:
```
$ skopeo --version
skopeo version 1.15.2
$ skopeo copy oci:/tmp/puzzlefs-image:puzzlefs_example oci:/tmp/copy-puzzlefs-image:puzzlefs_example
```
The information about the data chunks is also stored in the PuzzleFS image rootfs,
so that PuzzleFS could mount the filesystem efficiently and that the PuzzleFS
image could also be decoded in the kernel.

The `digest` of the PuzzleFS iamge rootfs contains the filesystem metadata and
it can be decoded using the `capnp tool` and the capnp metadata schema (the
following snippet assumes that you've cloned puzzlefs in `~/puzzlefs`):
```
$ capnp convert binary:json ~/puzzlefs/puzzlefs-lib/src/format/metadata.capnp Rootfs < blobs/sha256/9e2edc6917b65606b1112ac8663665dfd2d945cfea960ca595accf790922b910
{ "metadatas": [{"inodes": [
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
      "permissions": 509 },
    { "ino": "3",
      "mode": {"file": [{ "blob": {
          "digest": [183, 241, 238, 147, 115, 65, 106, 73, 131, 87, 71, 69, 94, 196, 210, 135, 188, 204, 197, 164, 191, 140, 56, 21, 100, 131, 212, 107, 53, 206, 77, 189],
          "offset": "0",
          "compressed": false },
        "len": "27" }]},
      "uid": 1000,
      "gid": 1000,
      "permissions": 436 },
    {"ino": "4", "mode": {"file": []}, "uid": 1000, "gid": 1000, "permissions": 436} ]}],
  "fsVerityData": [{ "digest": [183, 241, 238, 147, 115, 65, 106, 73, 131, 87, 71, 69, 94, 196, 210, 135, 188, 204, 197, 164, 191, 140, 56, 21, 100, 131, 212, 107, 53, 206, 77, 189],
    "verity": [91, 20, 52, 173, 44, 8, 31, 244, 53, 178, 16, 121, 46, 144, 14, 39, 2, 30, 196, 43, 104, 230, 143, 98, 219, 173, 82, 223, 224, 201, 247, 164] }],
  "manifestVersion": "3" }
```

`metadatas` contains a list of PuzzleFS layers, each layer consisting of a
vector of Inodes. See the [capnp
schema](./puzzlefs-lib/src/format/metadata.capnp) for details.

## Implementation

This workspace contains a library and an executable crate:
* `puzzlefs-lib` is the library crate
  * `format` is the module for serializing/de-serializing the puzzlefs format
  * `builder` is the module for building a puzzlefs image
  * `extractor` is the module for extracting a puzzlefs image
  * `reader` is the module for fuse mounting a puzzlefs image
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
