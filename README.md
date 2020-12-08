# PuzzleFS

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
