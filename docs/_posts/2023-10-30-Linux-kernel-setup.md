---
title: "Linux kernel setup for the PuzzleFS driver"
date: 2023-10-30
---
The setup is based on [Wedson's
tutorial](https://www.youtube.com/watch?v=tPs1uRqOnlk). This document describes
the necessary steps to build an initrd and run a custom kernel under qemu. This
custom kernel includes the patches for the PuzzleFS driver.

# Prerequisites
Install the necessary tools for building the Linux kernel:

* For Ubuntu, you can find a list [here](https://wiki.ubuntu.com/Kernel/BuildYourOwnKernel#Build_Environment)
* For Fedora, you can find a list [here](https://docs.fedoraproject.org/en-US/quick-docs/kernel-build-custom/#_get_the_dependencies)
* For Arch Linux, you can find a list [here](https://wiki.archlinux.org/title/Kernel/Traditional_compilation#Install_the_core_packages)

Install [qemu](https://wiki.qemu.org/Main_Page).

# Steps

1. Get the [PuzzleFS branch](https://github.com/ariel-miculas/linux/tree/puzzlefs)
    ```
    git clone https://github.com/ariel-miculas/linux.git --branch puzzlefs
    ```
    We'll call this path `KERNEL_PATH`.

2. Follow the [rust quickstart guide](https://docs.kernel.org/rust/quick-start.html)

3. Make sure `make LLVM=1 rustavailable` is successful

    This is especially important because `CONFIG_RUST=y` will be silently
    ignored if the rust toolchain is not available.

4. Configure and build the kernel
    ```
    $ make LLVM=1 allnoconfig qemu-busybox-min.config puzzlefs.config
    $ make LLVM=1 -j$(nproc)
    ```

5. Setup busybox
    ```
    git clone git://git.busybox.net/busybox
    cd busybox
    make menuconfig # enable 'Build static binary' config
    make
    make install
    ```
    This will create the `_install` directory with the rootfs inside it. We'll
    call the busybox path `BUSYBOX_PATH`.

6. Create a home directory in the rootfs and build a puzzlefs image inside
   (`$BUSYBOX_PATH/_install/home/puzzlefs_oci`)

    To build a puzzlefs image:
    * install puzzlefs using cargo: `cargo install puzzlefs` or clone the
      [puzzlefs repository](https://github.com/project-machine/puzzlefs) and
      run `make release`

    * create a simple filesystem structure with a few directories and files
      (e.g. in `/tmp/simple_rootfs`)
        ```
        $ tree simple_rootfs
        simple_rootfs
        ├── dir-1
        ├── dir-2
        ├── dir-3
        ├── dir-4
        ├── file1
        └── file2

        5 directories, 2 files
        ```

    * build a puzzlefs oci image at
      `$BUSYBOX_PATH/_install/home/puzzlefs_oci` with the tag `first_try`:

        ```
        $ puzzlefs build /tmp/simple_rootfs \
        $BUSYBOX_PATH/_install/home/puzzlefs_oci first_try
        ```

    * get `first_try`'s image manifest from `puzzlefs_oci/index.json`

        ```
        $ jq ".manifests[] | .digest" index.json
              "sha256:c43e5ab9d0cee1dcfbf442d18023b34410de3deb0f6dbffcec72732b6830db09"
        ```

7. Add the following `init` script in the busybox rootfs (defaults to `$BUSYBOX_PATH/_install`):

    ```
    #!/bin/sh
    mount -t devtmpfs none /dev
    mkdir -p /proc
    mount -t proc none /proc

    ifconfig lo up
    udhcpc -i eth0

    mkdir /mnt
    mount -t puzzlefs -o oci_root_dir="/home/puzzlefs_oci" -o \
    image_manifest="c43e5ab9d0cee1dcfbf442d18023b34410de3deb0f6dbffcec72732b6830db09" \
    none /mnt

    setsid sh -c 'exec sh -l </dev/ttyS0 >/dev/ttyS0 2>&1'
    ```
    Make sure to replace the `image_manifest` with your own digest. This
    init script will be passed to rdinit in the kernel command line.

8. Generate the initramfs

    ```
    cd $BUSYBOX_PATH/_install && find . | cpio -H newc -o | gzip > ../ramdisk.img
    ```
    This will generate a compressed ramdisk image in
    `$BUSYBOX_PATH/ramdisk.img`.

9. Run with qemu:
    ```
    qemu-system-x86_64 \
        -accel kvm \
        -cpu host \
        -m 4G \
        -initrd $BUSYBOX_PATH/ramdisk.img \
        -kernel $KERNEL_PATH/arch/x86/boot/bzImage \
        -nographic \
        -append 'console=ttyS0 nokaslr debug rdinit=/init' \
        -nic user,model=rtl8139 \
        -no-reboot
    ```

10. Check whether puzzlefs has been successfully mounted:
    ```
    ~ # grep puzzlefs /proc/filesystems
    nodev   puzzlefs
    ~ # mount | grep puzzlefs
    none on /mnt type puzzlefs (rw,relatime)
    ~ # ls /mnt/
    dir-1  dir-2  dir-3  dir-4  file1  file2
    ```
