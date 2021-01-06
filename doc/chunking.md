## Chunking a filesystem

This should definitely be rabin fingerprint/rolling hash based. casync is the
obvious prior art here.

## Defining the stream

In order to do content defined chunking, we need to serialize the filesystem
content into a stream. We can ignore everything besides regular files, since
everything else will be captured in the metadata representation. Since the
metadata representation is *not* stored inline with this stream, images built
at slightly different times (resulting in different mtimes for config files in
/etc) have a chance to share chunks.

We serialize the filesystem by doing a breadth first walk, ordering directory
entries lexicographically. We use a breadth first search so that hopefully
package directories can be shared. For example, if one image has a bunch of
stuff in `/etc/apt/sources.list.d` and another image has nothing there,
hopefully using this ordering we'll have a chance at sharing the contents of
`/etc`. This makes little difference for `/etc` since it only contains text
files, but could make a bigger difference e.g. for stuff in `/lib`, e.g. when
one image has a python package installed that the other does not.

## Rolling hash/Content Defined Chunking parameters

There are two philosophies about this: 1. let images define their own
parameters, so people can fine tune things for their particular image to get
good results on update or 2. hard code these parameters in the spec, so
everyone has to use the same algorithms and algorithm parameters. It seems (2)
would potentially enable more sharing across images, since it's hard to see how
anything (e.g. /etc/lsb-release should be mostly the same everywhere that's
based on the same distro, but may not be shared if e.g. ngnix chooses different
parameters than mysql). Additionally, (2) seems to be more in line with the
"canonicalization" goal, so that different image builders would be required to
choose the same parameters.

However, we leave the choice of hash, parameters, etc. as an exercise to the
reader :)
