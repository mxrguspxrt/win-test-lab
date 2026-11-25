#!/usr/bin/env bash
# build.sh - what the nested example needs on the host, once: out/hello.img, the tiny VM (needs
# nasm), and out/qemu-w64-setup.exe, QEMU for Windows from https://qemu.weilnetz.de/w64/ (Stefan
# Weil's builds), checked against its SHA-512. The lab's setup installs it into the 0 state.
#
#   QEMU_W64=<date> QEMU_W64_SHA512=<its .sha512> build.sh    another build than QEMU 11.1.0
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
od="$here/out"
mkdir -p "$od"
nasm -f bin -o "$od/hello.img" "$here/hello.asm"
echo "built $od/hello.img"

ver=${QEMU_W64:-20260811}
sha=${QEMU_W64_SHA512:-5bcf9eed634e8575a37b74f445af41a2fe4106da512d0c30c368301d4c105037fdfab40a5287367a28a957624cddebbc8c07e16c88ab6634f554cdf3d16bf543}
exe="$od/qemu-w64-setup.exe"
if [ -f "$exe" ] && echo "$sha  $exe" | sha512sum -c --status -; then
  echo "have $exe (QEMU for Windows $ver)"
  exit 0
fi
curl -fL --retry 3 -o "$exe.part" "https://qemu.weilnetz.de/w64/qemu-w64-setup-$ver.exe"
if ! echo "$sha  $exe.part" | sha512sum -c --status -; then
  rm -f "$exe.part"
  echo "qemu-w64-setup-$ver.exe does not have the expected SHA-512 (for another build, set QEMU_W64_SHA512 too)" >&2
  exit 1
fi
mv "$exe.part" "$exe"
echo "downloaded $exe (QEMU for Windows $ver)"
