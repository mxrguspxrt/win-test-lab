# Getting the Windows installer ISOs

win-test-lab installs Windows from Microsoft's own ISOs. They are not redistributed with
this project (they are large and Microsoft's to host). Download the ones you need **from
Microsoft's pages below, in your browser**, and put them in the **ISO folder** (`wtl home`
shows it: `iso-files/` in the home), or point `WTL_ISO_DIR` (or `"iso_dir"` in a config file)
at wherever you keep them. Do not use third-party download tools or mirrors.

The lab finds the right ISO by reading the Windows build number **and CPU** *inside* it, so
the file name does not matter.

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

## Checking an ISO

    wtl check-iso                              # every ISO in WTL_ISO_DIR: build, CPU, editions,
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
`labs/win10.json`, `labs/2025-arm64.json`); the DC must be a Windows Server. Windows 11 boots
UEFI, so the host needs QEMU's UEFI firmware (Homebrew's and the distros' QEMU packages ship
it; `wtl doctor` shows what it found).
