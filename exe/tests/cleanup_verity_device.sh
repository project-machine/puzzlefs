#!/bin/bash
set -e
MOUNTPOINT=$1
BLOCKDEV=$2
BACKING_FILE=$3

[ -z "$MOUNTPOINT" ] && exit 1
[ -z "$BLOCKDEV" ] && exit 1
[ -z "$BACKING_FILE" ] && exit 1

echo "unmounting: $MOUNTPOINT, deleting blockdev $BLOCKDEV and backing file $BACKING_FILE"
sudo umount "$MOUNTPOINT"
sudo losetup -d "$BLOCKDEV"
rm "$BACKING_FILE"
