# Getting the Windows installer ISOs

win-test-lab installs Windows from Microsoft's own ISOs. They are not redistributed with
this project (they are large and Microsoft's to host). Download the ones you need **from
Microsoft's pages below, in your browser**, and put them in the **ISO folder** (`wtl home`
shows it: `iso-files/` in the home), or point `WTL_ISO_DIR` (or `"iso_dir"` in a config file)
at wherever you keep them. Do not use third-party download tools or mirrors.

The lab finds the right ISO by reading the Windows build number **and CPU** *inside* it, so
the file name does not matter. To pin a file when you keep several of one OS, add an
`images-list.conf` to the home, one `KEY = file.iso` per line (`2019 = server2019-de.iso`; a
path relative to the ISO folder, or absolute).

## Which OS has which CPU

| Lab key | Windows | Build | CPU | Where to download |
|---------|---------|-------|-----|-------------------|
| 2012 | Windows Server 2012 R2 | 9600 | x64 | https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2012-r2 |
| 2016 | Windows Server 2016 | 14393 | x64 | https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2016 |
| 2019 | Windows Server 2019 | 17763 | x64 | https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2019 |
| 2022 | Windows Server 2022 | 20348 | x64 | https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2022 |
| 2025 | Windows Server 2025 | 26100 | x64 | https://www.microsoft.com/en-us/evalcenter/download-windows-server-2025 |
| win10 | Windows 10 Pro 22H2 | 19041 | x64 | https://www.microsoft.com/en-us/software-download/windows10ISO |
| win11 | Windows 11 Pro | 26300 | x64 | https://www.microsoft.com/en-us/software-download/windows11 |
| win11-arm64 | Windows 11 Pro on Arm | 26300 | **arm64** | https://www.microsoft.com/en-us/software-download/windows11arm64 |

What exists for Arm (checked on Microsoft's pages, October 2026):

- **Windows 11** is the only Windows with a free Arm64 ISO (the page above; one multi-edition
  ISO, newest release only). The Insider Preview Arm64 ISO needs a Windows Insider sign-in;
  Enterprise / LTSC Arm64 media come only with Visual Studio subscriptions or the Microsoft
  365 admin center. The free Windows 11 Enterprise *evaluation* is x64 only.
- **No Windows Server has an Arm64 ISO** (the Server 2025 evaluation lists x64 only). An AD
  lab on an Arm host therefore always has an x64 Server as its DC, which runs emulated there.
- **Windows 10 on Arm** was never offered as an ISO, and cannot run x64 programs anyway.

The server evaluations are valid for 180 days and need no product key: pick the **ISO**
(not Azure, not VHD), your language, the **Standard** or **Datacenter** edition with *Desktop
Experience*. Client media install Pro with Microsoft's generic key (not activated).

## Windows Server 2003 R2 and 2008 R2 (VHD, not ISO)

Microsoft never published a free *installation* ISO for Server 2003 R2 or 2008 R2 — only
pre-installed **VHD Test Drive images**. So for these two, `wtl` imports the VHD as the base
image instead of installing from an ISO; everything after that (the 0 state, runs, restore) is
the same as any other lab.

| Lab key | Windows | Source |
|---------|---------|--------|
| 2003 | Windows Server 2003 R2 Enterprise (**32-bit**) | the "Win2k3R2EE" VHD Test Drive download |
| 2008 | Windows Server 2008 R2 Enterprise x64 | the "WS2008R2" VHD Test Drive download |

Download pages: [2003 R2](https://www.microsoft.com/en-us/download/details.aspx?id=19727)
(four parts) and [2008 R2](https://www.microsoft.com/en-us/download/details.aspx?id=2227)
(three parts). These arrive as split self-extracting archives; unpack them first so a `.vhd` is
on disk (`unar Win2k3R2EE.part01.exe`, or the Windows self-extractor).

Put the VHD anywhere under the ISO folder, with any name: `wtl` recognises it by its content.
Every copy of a download carries the same unique ID in its VHD footer, and `lib/vhds.conf` lists
the IDs (and the download pages) of the ones it knows; that file is the whole "catalog", since the
VHDs themselves cannot be shipped. A `.vhd` it does not know is still used if its path contains
`Win2k3` (2003) or `WS2008R2` (2008 R2), e.g. another release of the same download. Pin an exact
file with an `images-list.conf` line (`2008 = path/to/x.vhd`) if you keep several.

How the import works: the images are sysprepped, so their first boot runs Windows' own
unattended setup. `wtl` drops an answer file into the imported image with libguestfs'
`virt-customize` (2008 R2: `unattend.xml`; 2003: `sysprep.inf`, keeping the evaluation product
key and Administrator password the image already carries) that finishes setup without input,
logs on once as Administrator and runs win-test-lab's build setup from a disc. The image boots
from the legacy IDE controller (these are Hyper-V Gen 1 disks), so a lab on them can mount at
most two folders.

Server 2003 needs a few more things, all handled by `wtl` (they are why it has its own folder of
files, `guest/2003/`, and options on its row of `lib/oslist.conf`):

- A registry merge (libguestfs `virt-win-reg`, `guest/2003/image.reg`) makes QEMU's IDE
  controller a boot-critical device — the image only knows Hyper-V's and otherwise stops with
  `STOP 0x7B` — and registers the launcher that starts win-test-lab's setup at the first logon.
- Its first-boot setup shows a dialog no answer file can skip (the evaluation-copy notice), so
  `wtl build-image` presses Enter over the QEMU monitor every few seconds until the guest
  reports in (`+enter`).
- It hangs at the boot *after* that setup if QEMU gives it a USB controller, so its VMs have
  none — PS/2 keyboard and mouse only (`+nousb`).
- It is **32-bit**: Microsoft's 2003 R2 Test Drive VHD is the 32-bit edition (we know of no
  64-bit VHD of it; 64-bit Server 2003 only came as install media, which `wtl` does not install),
  so the programs you test on it must be 32-bit too (`examples/smoke-test/build.sh` builds one
  into `out32/` when `gcc-mingw-w64-i686` is there). 2008 R2, which is 64-bit only, is the oldest
  64-bit Windows here.

Extra things that apply only to these OSes:

- **libguestfs** must be installed on the host (`sudo apt install libguestfs-tools`, Fedora
  `sudo dnf install guestfs-tools`). On Debian/Ubuntu also make the kernel readable
  (`sudo chmod 0644 /boot/vmlinuz-*`) or libguestfs's supermin can't build its appliance.
  `./install-deps.sh` does both.
- **The guest side is different.** 2008 R2 has PowerShell 2.0, so it runs PowerShell scripts
  written for it (`guest/2008/`). **Server 2003 has no PowerShell at all**, so its whole guest
  side is VBScript and batch files (`guest/2003/`, run by the Windows Script Host every 2003 has).
  Nothing is installed into the image.
- They run **standalone only** for now: one machine, workgroup mode, no AD role, no server
  roles/features and no lab LAN. `wtl` refuses an AD or multi-machine lab on them before boot.
  Steps run as `system` or as the machine's logon user; a step `as` another lab user is not
  supported on Server 2003 yet.
  (From Server 2012 R2 on, everything — AD, roles, multi-machine — works as documented.)

The evaluation terms of the Test Drive VHDs are Microsoft's; read the license in the download
and use them within it.

## Checking an ISO

    wtl check-iso                              # every ISO in the ISO folder: build, CPU, editions,
                                               # and which lab key it serves
    wtl check-iso FILE.iso --sha256 HASH       # also: is it byte-for-byte what Microsoft published

Microsoft's download pages list a SHA-256 for every language; compare it before you use an
ISO. For example the English Windows 11 Arm64 ISO (26H2) is
`8992F2D2CFD2FA647E579E9EB31FC21F34DA8333C29B6170FDE17D2ED6623E9C`. `wtl check-iso` needs no
other tool (on a 8.5 GB ISO the hash takes a few minutes).

    wtl doctor                                 # this host: QEMU per CPU, acceleration,
                                               # firmware, disk space, ISO -> lab key

A disc with no `sources/install.wim` (a Features-on-Demand / LOF disc) is not an installation
ISO and is skipped.

## Drivers for arm64 machines (virtio-win)

Windows 11 on Arm ships **no driver for any network card or serial port QEMU can emulate**
(tested in an installed Windows 11 26300: e1000e, rtl8139, igb, vmxnet3, virtio-net and
usb-net network cards; PL011, pci-serial and usb-serial ports; all show up without a driver,
or as usb-net's broken serial function). Without a network and a serial channel `wtl` cannot
drive the machine, so an arm64 build needs two drivers from **virtio-win**, the Windows
drivers for QEMU's virtio devices (built by Red Hat, signed by Microsoft's Windows Hardware
Compatibility Publisher, used by Proxmox, UTM, libvirt):

| driver | folder in virtio-win | what for |
|--------|----------------------|----------|
| NetKVM | `NetKVM/w11/ARM64/` | the network cards (NAT + lab LAN) |
| vioserial | `vioserial/w11/ARM64/` | the guest -> host report channel (instead of COM1) |

Put them under `drivers/virtio-win-<version>/` in the home (or the folder `WTL_DRIVERS_DIR`
names), keeping virtio-win's layout:

    drivers/virtio-win-0.1.302/NetKVM/w11/ARM64/{netkvm.inf,netkvm.sys,netkvm.cat,netkvmp.exe,...}
    drivers/virtio-win-0.1.302/vioserial/w11/ARM64/{vioser.inf,vioser.sys,vioser.cat}

The simplest way: download `virtio-win.iso` (about 840 MB) from Red Hat's Fedora page
https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/ and copy those
two folders out of it (or copy the whole ISO's content). The build copies every file of each
folder except the `.pdb` debug symbols, checks that every file the `.inf` lists is there (a
missing `netkvmp.exe` silently costs the machine its network), and refuses to start an arm64
build without them. x64 machines need none of this.

## Windows 10 / 11 members

A lab member may run Windows 10 or 11 (`"os": "win10"` / `"win11"` / `"win11-arm64"`, see
`labs/win10.json`, `labs/single-win11-arm64.json`); the DC must be a Windows Server. Windows 11 boots
UEFI, so the host needs QEMU's UEFI firmware (Homebrew's and the distros' QEMU packages ship
it; `wtl doctor` shows what it found).
