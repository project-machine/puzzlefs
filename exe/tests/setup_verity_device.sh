#!/bin/bash
set -e

BACKING_FILE=$(mktemp -u)
touch $BACKING_FILE
dd if=/dev/zero of="$BACKING_FILE" bs=1k count=1024
BLOCKDEV=$(sudo losetup -f --show "$BACKING_FILE")
sudo mkfs -t ext4 -F -b4096 -O verity "$BLOCKDEV"
MOUNTPOINT=$(mktemp -u)
mkdir "$MOUNTPOINT"
sudo mount "$BLOCKDEV" "$MOUNTPOINT"
USER_ID=$(id -u)
GROUP_ID=$(id -g)
sudo chown -R "$USER_ID":"$GROUP_ID" "$MOUNTPOINT"
echo "mounted $BLOCKDEV backed by $BACKING_FILE at $MOUNTPOINT"
