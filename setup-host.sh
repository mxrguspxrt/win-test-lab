#!/usr/bin/env bash
# setup-host.sh - install/check host prerequisites for win-test-lab.
#   ./setup-host.sh [--check]     (--check: only check, never install)
# Installs qemu-system-x86 qemu-utils python3 (+ ovmf, optional) on Debian/Ubuntu via sudo;
# prints the dnf/pacman equivalents elsewhere. Checks /dev/kvm, kvm group, free disk space.
set -euo pipefail
# shellcheck source=lib/common.sh
source "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/lib/common.sh"

CHECK_ONLY=0
[[ "${1:-}" == --check ]] && CHECK_ONLY=1
PROBLEMS=0

ID_LIKE_ALL=""
if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091
    ID_LIKE_ALL="$(. /etc/os-release; echo "${ID:-} ${ID_LIKE:-}")"
fi

need_pkgs() {
    command -v qemu-system-x86_64 >/dev/null 2>&1 || return 0
    command -v qemu-img >/dev/null 2>&1 || return 0
    command -v python3 >/dev/null 2>&1 || return 0
    return 1
}

log "== packages"
if need_pkgs; then
    case " $ID_LIKE_ALL " in
        *" debian "*|*" ubuntu "*)
            if ((CHECK_ONLY)); then
                warn "missing packages; run: sudo apt-get install -y qemu-system-x86 qemu-utils python3 ovmf"
                PROBLEMS=1
            else
                log "installing qemu-system-x86 qemu-utils python3 (sudo)"
                sudo apt-get update
                sudo apt-get install -y qemu-system-x86 qemu-utils python3
                sudo apt-get install -y ovmf || warn "ovmf (UEFI firmware) not installed - optional, not needed"
            fi
            ;;
        *" fedora "*|*" rhel "*|*" centos "*)
            warn "Fedora/RHEL: run   sudo dnf install -y qemu-kvm qemu-img python3 edk2-ovmf"
            PROBLEMS=1 ;;
        *" arch "*|*" manjaro "*)
            warn "Arch: run   sudo pacman -S --needed qemu-desktop python edk2-ovmf"
            PROBLEMS=1 ;;
        *" suse "*|*" opensuse "*)
            warn "openSUSE: run   sudo zypper install qemu-x86 qemu-tools python3"
            PROBLEMS=1 ;;
        *)
            warn "unknown distro; install QEMU (qemu-system-x86_64, qemu-img) and python3 >= 3.8"
            warn "  Debian/Ubuntu: sudo apt-get install -y qemu-system-x86 qemu-utils python3"
            warn "  Fedora:        sudo dnf install -y qemu-kvm qemu-img python3"
            warn "  Arch:          sudo pacman -S --needed qemu-desktop python"
            PROBLEMS=1 ;;
    esac
fi
for t in qemu-system-x86_64 qemu-img python3; do
    if command -v "$t" >/dev/null 2>&1; then
        ok "$t: $(command -v "$t")"
    else
        err "$t not found"; PROBLEMS=1
    fi
done
if command -v python3 >/dev/null 2>&1; then
    python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)' \
        && ok "python3 $(python3 -c 'import platform; print(platform.python_version())')" \
        || { err "python3 >= 3.8 required"; PROBLEMS=1; }
fi
if command -v qemu-system-x86_64 >/dev/null 2>&1; then
    log "$(qemu-system-x86_64 --version | head -1)"
fi

log "== KVM"
if [[ -e /dev/kvm ]]; then
    ok "/dev/kvm exists"
    if [[ -r /dev/kvm && -w /dev/kvm ]]; then
        ok "/dev/kvm is accessible by $(id -un)"
    else
        PROBLEMS=1
        if id -nG | tr ' ' '\n' | grep -qx kvm; then
            warn "you are in group kvm but /dev/kvm is not accessible yet: log out and back in"
        else
            err "user $(id -un) is not in group 'kvm'. Fix with:"
            echo "    sudo usermod -aG kvm $(id -un)    # then log out and back in" >&2
        fi
    fi
else
    PROBLEMS=1
    err "/dev/kvm missing: enable virtualization (VT-x/AMD-V) in the BIOS/UEFI and load the module:"
    echo "    sudo modprobe kvm_intel   # or: sudo modprobe kvm_amd" >&2
    grep -Eqc '(vmx|svm)' /proc/cpuinfo 2>/dev/null \
        && echo "    (the CPU supports it: vmx/svm flag present)" >&2 \
        || echo "    (no vmx/svm flag in /proc/cpuinfo - nested virt disabled or unsupported)" >&2
fi

log "== disk space"
mkdir -p "$WTL_IMAGES_DIR"
avail="$(free_gb "$WTL_IMAGES_DIR")"
df -h "$WTL_IMAGES_DIR" | sed 's/^/    /' >&2
if ((avail < 40)); then
    warn "only ${avail} GiB free for images/ - each built image needs ~15-30 GiB, a build ~40 GiB"
else
    ok "${avail} GiB free for images/"
fi

log "== ISOs in $WTL_ISO_DIR"
while IFS= read -r k; do
    os_lookup "$k"
    if iso="$(find_iso "$k")"; then
        ok "$k: $(basename "$iso")"
    else
        log "$k: no ISO (./download-isos.sh $k)"
    fi
done < <(os_keys)

if ((PROBLEMS)); then
    warn "setup incomplete - see messages above"
    exit 1
fi
ok "host ready. Next: ./build-image.sh 2012"
