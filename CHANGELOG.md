# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.0] - 2026-09-11

### Added

- Added detection of the Ventoy data partition used to boot the current ISO.
- Read Ventoy runtime parameters from UEFI variables, with ACPI `VTOY` and `iBFT` table fallbacks.
- Validate the Ventoy disk identifier, data-partition number, and current ISO path before assigning Ventoy search priority.

### Changed

- Search the validated Ventoy data partition after the firmware boot disk and before other USB disks.
- Switched command-line parsing to `clap`, providing its standard help and version output.
- Release builds now include the Windows XP and Windows 7 compatibility thunk; resource compilation no longer requires a manifest.

### Fixed

- Verify that `SeSystemEnvironmentPrivilege` was assigned before using it to read Ventoy UEFI runtime data, and continue with ACPI fallback data when it is unavailable.

## [1.0.0] - 2026-08-23

First version.
