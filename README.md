# FindWinPEProfilePath

[简体中文](README.zh.md) | English

In Windows PE, finds a personalized marker file or directory in the root directory of a volume according to a preset media priority.

This program is suitable for use by PECMD, batch files, or other startup scripts. For example, it locates the `WinPE` directory or the `Version.txt` file on multiple disks and writes the full DOS path of the first match to standard output.

## Usage

```text
FindWinPEProfilePath.exe [--verbose] <relative-path>
```

Example:

```bat
FindWinPEProfilePath.exe WinPE
FindWinPEProfilePath.exe WinPE\Version.txt
FindWinPEProfilePath.exe --verbose WinPE
```

`<relative-path>` must be a path relative to the root directory of the candidate volume:

- Supports files, directories, and multi-level paths, such as `WinPE`, `Version.txt`, `WinPE\Version.txt`.
- Absolute paths, UNC paths, drive letters, and paths starting with `\` or `/` are not supported.
- Empty paths, empty components, `.`, or `..` are not supported to avoid searching beyond the volume root.

## Search Order

The program enumerates all volumes with assigned drive letters, checking each category in ascending order of drive letter. The search ends immediately upon finding the first target.

1. The boot partition corresponding to `FirmwareBootDevice`.
2. Other partitions on the physical disk containing `FirmwareBootDevice`.
3. The Ventoy data partition, verified to contain the ISO currently used for booting.
4. Other USB disks, including USB hard drives marked as fixed disks by Windows.
5. Other removable media.
6. Optical discs.
7. Internal fixed hard drives.
8. RAM disks and virtual disks.
9. Network drives.

> The boot partition is identified through the registry links `HKLM\SYSTEM\CurrentControlSet\Control\FirmwareBootDevice` and `\ArcName`. When the boot partition itself has no drive letter, the program will still prioritize checking other partitions on the same disk that already have drive letters, based on their physical disk numbers. If it cannot read `FirmwareBootDevice` or convert ARC links, the program will skip the first two boot disk priority checks and continue checking the remaining volumes.

Ventoy detection reads its UEFI runtime variables or the runtime parameters in the ACPI `VTOY`/`iBFT` tables. The program also validates the Ventoy disk identifier, data partition number, and current ISO path; only a mounted volume matching all three enters stage 3. No additional priority is given to other partitions on the Ventoy disk.

## Output and Exit Codes

Default mode for script calls:

- Target found: Outputs only the full path to stdout, e.g., `F:\WinPE`, exit code `0`.
- Target not found: No output, exit code `1`.
- Incorrect parameters: By default, outputs usage error messages to stdout, exit code `2`; when `--verbose` is passed, it writes to stderr instead.
- System error preventing continuation: By default, no output, exit code `2`; when `--verbose` is passed, it writes to stderr.

Passing `--verbose` will write volume classification, ARC/registry fallback information, and each checked path to stderr; hit paths will still only be written to stdout.

Batch call example:

```bat
setlocal EnableExtensions EnableDelayedExpansion
set "ProfileRoot="
for /f "usebackq delims=" %%P in (`FindWinPEProfilePath.exe WinPE`) do set "ProfileRoot=%%P"

if errorlevel 1 (
  echo FindWinPEProfilePath failed or the marker was not found.
) else (
  echo Found: !ProfileRoot!
)
```

## Build

Requires the Rust MSVC toolchain to be installed; if you need to build the entire architecture, you also need to install the corresponding target.

```bat
cargo test
cargo build --release
cargo build --release --target i686-pc-windows-msvc
cargo build --release --target aarch64-pc-windows-msvc
```

Alternatively, you can run `build.bat`: it builds x64, x86, and ARM64 versions, and compresses the x64 and x86 artifacts if `upx.exe` exists in the project root directory.

Article location:

```text
target\release\FindWinPEProfilePath.exe
target\i686-pc-windows-msvc\release\FindWinPEProfilePath.exe
target\aarch64-pc-windows-msvc\release\FindWinPEProfilePath.exe
```
## Limitations

- Only searches volumes with DOS drive letters; will not temporarily assign drive letters to hidden partitions or partitions without drive letters.
- Only check specified relative paths under the volume root; do not perform a full recursive search.
- When the target is a re-resolution point, a directory entry is considered a hit as long as it is accessible; its final target is not verified.
- Storage device information is provided by the Windows API. If certain WinPE drivers are missing or restricted, the USB/virtual disk classification may be downgraded, but the volume will still proceed to the next search stage.

## License

MIT License

## Contributions

Contributions are welcome! Please feel free to submit issues or pull requests.
