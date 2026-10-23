# Getting the Windows installer ISOs

win-test-lab installs Windows from Microsoft's **free evaluation ISOs**. They are not
redistributed with this project (they are large and Microsoft's to host). Download the ones
you need and drop them in **`win-test-lab/iso-files/`** (the default `WTL_ISO_DIR`), or point
`WTL_ISO_DIR` at wherever you keep them.

The lab finds the right ISO by reading the Windows build number *inside* it, so the file name
does not matter. Each evaluation is valid for 180 days and needs no product key.

| Lab key | Windows                     | Build  | Where to download |
|---------|-----------------------------|--------|-------------------|
| 2012    | Windows Server 2012 R2      | 9600   | https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2012-r2 |
| 2016    | Windows Server 2016         | 14393  | https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2016 |
| 2019    | Windows Server 2019         | 17763  | https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2019 |
| 2022    | Windows Server 2022         | 20348  | https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2022 |
| 2025    | Windows Server 2025         | 26100  | https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2025 |

`./download-isos.sh 2016` tries to fetch an ISO automatically via Microsoft's download links;
if that fails (the links change), download it from the Evaluation Center page above by hand.

Pick the **ISO** option (not Azure), the English / your-language x64 build, and the
**Standard** or **Datacenter** edition with the *Desktop Experience* (the lab uses the GUI
edition, not Server Core).

## Checking an ISO

    python3 lib/isoinfo.py build /path/to/file.iso     # prints e.g. 9600
    python3 lib/isoinfo.py wim   /path/to/file.iso      # lists the images inside

A disc with no `sources/install.wim` (for example a Features-on-Demand / LOF disc) is not an
installation ISO and is skipped.

## Windows 10 / 11 members

A lab member may run Windows 10 or 11 (`"os": "win10"` / `"win11"`, see `labs/win10.json`);
the DC must be a Windows Server. Use the regular retail x64 ISO (Pro edition is installed
with Microsoft's generic key; it is not activated). Windows 11 boots UEFI, so the host needs
OVMF (`./setup-host.sh` installs it).
