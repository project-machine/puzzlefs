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


## TODO

* explore chunking algorithms
* flesh out the rest of the file type metadata
* consider what "minimal" metadata might look like
* play around with zstd seekable compression a bit

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
