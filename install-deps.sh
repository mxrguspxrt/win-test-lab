#!/usr/bin/env bash
# install-deps.sh - install win-test-lab's host dependencies on Ubuntu / Debian.
#
#   ./install-deps.sh            # core deps + add you to the kvm group
#   ./install-deps.sh --all      # also the optional ones (VHD unpacking, the example build)
#
# What it installs (apt):
#   qemu-system-x86      qemu-system-x86_64  - run x64 Windows (every Server, 10, 11 x64)
#   qemu-system-arm      qemu-system-aarch64 - run Windows 11 on Arm
#   qemu-utils           qemu-img            - make and grow the disk images
#   ovmf                 x64 UEFI firmware   - Windows 11 (and any UEFI machine)
#   qemu-efi-aarch64     arm64 UEFI firmware - Windows 11 on Arm
#   libguestfs-tools     virt-customize      - ONLY for Server 2003 / 2008 R2 (VHD import)
# Optional (--all):
#   gcc-mingw-w64-x86-64                     - build the bundled example smoke.exe
#   gcc-mingw-w64-i686                       - ... and its 32-bit build for Windows Server 2003
#   unar                                     - unpack the 2003/2008 VHD Test Drive archives
#
# Rust is not installed here: get it from https://rustup.rs, then `cargo install win-test-lab`
# (or `cargo build --release` in a clone). QEMU needs hardware virtualization enabled in the
# BIOS/host; a cloud VM without nested virtualization cannot run KVM.
set -euo pipefail

want_all=0
[ "${1:-}" = "--all" ] && want_all=1

if ! command -v apt-get >/dev/null 2>&1; then
    echo "This script is for Ubuntu/Debian (apt). On Fedora use:" >&2
    echo "  sudo dnf install qemu-kvm qemu-img edk2-ovmf guestfs-tools" >&2
    echo "On macOS: brew install qemu  (libguestfs is not needed there; 2003/2008 are Linux-only)." >&2
    exit 1
fi

core=(qemu-system-x86 qemu-system-arm qemu-utils ovmf qemu-efi-aarch64 libguestfs-tools)
optional=(gcc-mingw-w64-x86-64 gcc-mingw-w64-i686 unar)

pkgs=("${core[@]}")
[ "$want_all" = 1 ] && pkgs+=("${optional[@]}")

echo "==> apt-get update"
sudo apt-get update
echo "==> installing: ${pkgs[*]}"
sudo apt-get install -y "${pkgs[@]}"

# libguestfs (virt-customize, used for the 2003/2008 R2 VHD import) builds its appliance with
# supermin, which must read the host kernel. On Debian/Ubuntu /boot/vmlinuz-* is installed mode
# 0600 (root only), so supermin fails with "supermin exited with error status 1". Make the
# kernel images world-readable, and register a dpkg-statoverride so kernel updates keep them so.
if ls /boot/vmlinuz-* >/dev/null 2>&1; then
    echo "==> making /boot/vmlinuz-* readable (needed by libguestfs/supermin)"
    sudo chmod 0644 /boot/vmlinuz-* || true
    for k in /boot/vmlinuz-*; do
        if ! sudo dpkg-statoverride --list "$k" >/dev/null 2>&1; then
            sudo dpkg-statoverride --update --add root root 0644 "$k" 2>/dev/null || true
        fi
    done
fi

# KVM access: being in the 'kvm' group lets you open /dev/kvm without root.
if getent group kvm >/dev/null 2>&1; then
    if id -nG "$USER" | tr ' ' '\n' | grep -qx kvm; then
        echo "==> $USER is already in the kvm group"
    else
        echo "==> adding $USER to the kvm group"
        sudo usermod -aG kvm "$USER"
        echo "    log out and back in (or run 'newgrp kvm') for it to take effect"
    fi
fi

echo
echo "Done. Next:"
echo "  1. install Rust from https://rustup.rs, then: cargo install win-test-lab"
echo "  2. wtl doctor          # what this host can run"
echo "  3. put the Windows ISOs (and, for 2003/2008 R2, the VHDs) in the ISO folder (wtl home)"
echo "     see docs/ISOS.md"
