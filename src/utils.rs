use std::{ffi::c_void, fmt, mem, ptr::null_mut};

use anyhow::{Context, bail};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HMODULE},
        Storage::FileSystem::{
            CreateFileW, FILE_DEVICE_DISK, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ,
            FILE_SHARE_WRITE, GetDriveTypeW, GetFileAttributesW, GetLogicalDrives,
            INVALID_FILE_ATTRIBUTES, OPEN_EXISTING, QueryDosDeviceW,
        },
        System::{
            IO::DeviceIoControl,
            Ioctl::{
                IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_QUERY_PROPERTY,
                PropertyStandardQuery, STORAGE_ADAPTER_DESCRIPTOR, STORAGE_DEVICE_NUMBER,
                STORAGE_PROPERTY_QUERY, StorageAdapterProperty,
            },
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
        },
    },
    core::{PCSTR, PCWSTR},
};
use windows_registry::LOCAL_MACHINE;

const SYMBOLIC_LINK_QUERY: u32 = 0x0001;
const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023u32 as i32;
const STATUS_PROCEDURE_NOT_FOUND: i32 = 0xC000_007Au32 as i32;
const BUS_TYPE_USB: i32 = 7;
const BUS_TYPE_VIRTUAL: i32 = 14;
const BUS_TYPE_FILE_BACKED_VIRTUAL: i32 = 15;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

type NtStatus = i32;
type NtOpenSymbolicLinkObject = unsafe extern "system" fn(
    link_handle: *mut HANDLE,
    desired_access: u32,
    object_attributes: *mut ObjectAttributes,
) -> NtStatus;
type NtQuerySymbolicLinkObject = unsafe extern "system" fn(
    link_handle: HANDLE,
    link_target: *mut UnicodeString,
    returned_length: *mut u32,
) -> NtStatus;
type NtClose = unsafe extern "system" fn(handle: HANDLE) -> NtStatus;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *mut UnicodeString,
    attributes: u32,
    security_descriptor: *mut c_void,
    security_quality_of_service: *mut c_void,
}

#[derive(Debug)]
pub enum SearchError {
    System(anyhow::Error),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// 搜索分组
enum SearchGroup {
    /// 引导卷
    Boot,
    /// 同一磁盘
    SameDisk,
    /// USB卷
    Usb,
    /// 可移动卷
    Removable,
    /// 光盘卷
    Optical,
    /// 固定卷
    Fixed,
    /// 虚拟卷
    VirtualOrRam,
    /// 网络卷
    Network,
}

impl fmt::Display for SearchGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Boot => "boot",
            Self::SameDisk => "same-disk",
            Self::Usb => "usb",
            Self::Removable => "removable",
            Self::Optical => "optical",
            Self::Fixed => "fixed",
            Self::VirtualOrRam => "virtual-or-ram",
            Self::Network => "network",
        };
        formatter.write_str(name)
    }
}

#[derive(Debug)]
/// 卷信息
struct Volume {
    /// 卷根目录
    root: String,
    /// DOS设备名
    dos_device: Option<String>,
    /// 驱动类型
    drive_type: u32,
    /// 设备号
    device_number: Option<STORAGE_DEVICE_NUMBER>,
    /// 总线类型
    bus_type: Option<i32>,
}

/// 验证路径是否为相对路径
///
/// # Arguments
///
/// * `path` - 要验证的路径
///
/// # Returns
///
/// * `Ok(())` - 路径为相对路径
/// * `Err(anyhow::Error)` - 路径不是相对路径，返回错误信息
pub fn validate_relative_path(path: &str) -> anyhow::Result<()> {
    if path.is_empty() || path.starts_with(['\\', '/']) || path.contains(':') {
        bail!("path must be relative to a volume root");
    }

    for component in path.split(['\\', '/']) {
        if component.is_empty() || matches!(component, "." | "..") {
            bail!("path must not contain empty, '.' or '..' components");
        }
    }
    Ok(())
}

/// 查找文件或目录
///
/// # Arguments
///
/// * `relative_path` - 相对路径
/// * `verbose` - 是否打印详细信息
///
/// # Returns
///
/// * `Ok(Some(path))` - 找到文件或目录，返回路径
/// * `Ok(None)` - 未找到文件或目录
/// * `Err(SearchError)` - 搜索失败，返回错误信息
pub fn find_marker(relative_path: &str, verbose: bool) -> Result<Option<String>, SearchError> {
    let boot_nt_path = match read_firmware_boot_device() {
        Ok(boot_device) => match query_arc_link(&format!(r"\ArcName\{boot_device}")) {
            Ok(path) => Some(path),
            Err(status) => {
                if verbose {
                    eprintln!(
                        "Warning: ARC link unavailable (NTSTATUS 0x{:08X}); continuing without boot-volume priority.",
                        status as u32
                    );
                }
                None
            }
        },
        Err(error) => {
            if verbose {
                eprintln!(
                    "Warning: cannot read FirmwareBootDevice: {error}; continuing without boot-volume priority."
                );
            }
            None
        }
    };
    let volumes = enumerate_volumes(verbose).map_err(SearchError::System)?;
    let boot_device_number = boot_nt_path.as_deref().and_then(parse_arc_device_number);
    let boot_volume = boot_nt_path.as_deref().and_then(|path| {
        volumes
            .iter()
            .find(|volume| volume_matches_boot(volume, path, boot_device_number))
    });
    let boot_disk = boot_volume
        .and_then(|volume| volume.device_number.map(|number| number.DeviceNumber))
        .or_else(|| boot_device_number.map(|number| number.DeviceNumber));

    if boot_nt_path.is_some() && boot_volume.is_none() && verbose {
        eprintln!(
            "Warning: boot partition {} has no mounted DOS drive; continuing with remaining volumes.",
            boot_nt_path.as_deref().unwrap()
        );
    }

    let mut candidates: Vec<&Volume> = volumes.iter().collect();
    candidates.sort_by_key(|volume| {
        (
            classify(volume, boot_nt_path.as_deref(), boot_disk),
            &volume.root,
        )
    });

    for volume in candidates {
        let group = classify(volume, boot_nt_path.as_deref(), boot_disk);
        let candidate = format!("{}{}", volume.root, relative_path.replace('/', "\\"));
        if verbose {
            eprintln!("[{group}] checking {candidate}");
        }
        if path_exists(&candidate) {
            return Ok(Some(candidate));
        }
    }

    Ok(None)
}

/// 分类卷
///
/// # Arguments
///
/// * `volume` - 卷信息
/// * `boot_nt_path` - 引导NT路径
/// * `boot_disk` - 引导磁盘
///
/// # Returns
///
/// * `SearchGroup` - 卷分类
fn classify(volume: &Volume, boot_nt_path: Option<&str>, boot_disk: Option<u32>) -> SearchGroup {
    if boot_nt_path
        .is_some_and(|path| volume_matches_boot(volume, path, parse_arc_device_number(path)))
    {
        return SearchGroup::Boot;
    }
    if boot_disk.is_some() && volume.device_number.map(|number| number.DeviceNumber) == boot_disk {
        return SearchGroup::SameDisk;
    }
    if volume.bus_type == Some(BUS_TYPE_USB) {
        return SearchGroup::Usb;
    }
    match volume.drive_type {
        DRIVE_REMOVABLE => SearchGroup::Removable,
        DRIVE_CDROM => SearchGroup::Optical,
        DRIVE_REMOTE => SearchGroup::Network,
        DRIVE_RAMDISK => SearchGroup::VirtualOrRam,
        DRIVE_FIXED
            if matches!(
                volume.bus_type,
                Some(BUS_TYPE_VIRTUAL | BUS_TYPE_FILE_BACKED_VIRTUAL)
            ) =>
        {
            SearchGroup::VirtualOrRam
        }
        DRIVE_FIXED => SearchGroup::Fixed,
        _ => SearchGroup::VirtualOrRam,
    }
}

/// 枚举所有卷
///
/// # Arguments
///
/// * `verbose` - 是否打印详细信息
///
/// # Returns
///
/// * `Ok(volumes)` - 所有卷信息
/// * `Err(anyhow::Error)` - 枚举卷失败，返回错误信息
fn enumerate_volumes(verbose: bool) -> anyhow::Result<Vec<Volume>> {
    let drive_mask = unsafe { GetLogicalDrives() };
    if drive_mask == 0 {
        bail!("GetLogicalDrives returned no drives");
    }

    let mut volumes = Vec::new();
    for index in 0..26 {
        if drive_mask & (1 << index) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + index) as char);
        let root_wide = wide(&root);
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_wide.as_ptr())) };
        if drive_type == 0 || drive_type == 1 {
            continue;
        }

        let dos_device = query_dos_device(&root[..2]).ok();
        let device_number = device_number(&format!(r"\\.\{}", &root[..2])).ok();
        let bus_type = device_number.and_then(|number| storage_bus_type(number.DeviceNumber).ok());
        if verbose {
            eprintln!(
                "volume {root} type={drive_type} nt={dos_device:?} device={device_number:?} bus={bus_type:?}"
            );
        }
        volumes.push(Volume {
            root,
            dos_device,
            drive_type,
            device_number,
            bus_type,
        });
    }
    Ok(volumes)
}

/// 检查卷是否匹配引导NT路径或引导设备号
///
/// # Arguments
///
/// * `volume` - 卷信息
/// * `boot_nt_path` - 引导NT路径
/// * `boot_device_number` - 引导设备号
///
/// # Returns
///
/// * `true` - 卷匹配引导NT路径或引导设备号
/// * `false` - 卷不匹配引导NT路径或引导设备号
fn volume_matches_boot(
    volume: &Volume,
    boot_nt_path: &str,
    boot_device_number: Option<STORAGE_DEVICE_NUMBER>,
) -> bool {
    volume.dos_device.as_deref() == Some(boot_nt_path)
        || boot_device_number.is_some_and(|number| volume.device_number == Some(number))
}

/// 解析引导设备号
///
/// # Arguments
///
/// * `path` - 引导NT路径
///
/// # Returns
///
/// * `Some(number)` - 解析成功，返回引导设备号
/// * `None` - 解析失败
pub fn parse_arc_device_number(path: &str) -> Option<STORAGE_DEVICE_NUMBER> {
    let normalized = path.to_ascii_lowercase();
    let suffix = normalized.strip_prefix(r"\device\harddisk")?;
    let (disk, partition) = suffix.split_once(r"\partition")?;
    Some(STORAGE_DEVICE_NUMBER {
        DeviceType: FILE_DEVICE_DISK.0,
        DeviceNumber: disk.parse().ok()?,
        PartitionNumber: partition.parse().ok()?,
    })
}

/// 检查路径是否存在
///
/// # Arguments
///
/// * `path` - 路径
///
/// # Returns
///
/// * `true` - 路径存在
/// * `false` - 路径不存在
pub fn path_exists(path: &str) -> bool {
    let path_wide = wide(path);
    unsafe { GetFileAttributesW(PCWSTR(path_wide.as_ptr())) != INVALID_FILE_ATTRIBUTES }
}

/// 查询设备的DOS名称
///
/// # Arguments
///
/// * `device_name` - 设备名称
///
/// # Returns
///
/// * `Ok(dos_device)` - 查询成功，返回DOS名称
/// * `Err(anyhow::Error)` - 查询失败，返回错误信息
pub fn query_dos_device(device_name: &str) -> anyhow::Result<String> {
    let name = wide(device_name);
    let mut buffer = vec![0u16; 32_768];
    let length = unsafe { QueryDosDeviceW(PCWSTR(name.as_ptr()), Some(&mut buffer)) };
    if length == 0 {
        bail!("QueryDosDeviceW failed for {device_name}");
    }
    let first_nul = buffer[..length as usize]
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(length as usize);
    Ok(String::from_utf16(&buffer[..first_nul])?)
}

/// 查询设备的设备号
///
/// # Arguments
///
/// * `path` - 设备路径
///
/// # Returns
///
/// * `Ok(number)` - 查询成功，返回设备号
/// * `Err(anyhow::Error)` - 查询失败，返回错误信息
pub fn device_number(path: &str) -> anyhow::Result<STORAGE_DEVICE_NUMBER> {
    let handle = open_device(path)?;
    let mut number = STORAGE_DEVICE_NUMBER::default();
    let result = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some(&mut number as *mut _ as *mut c_void),
            mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            None,
            None,
        )
    };
    unsafe { CloseHandle(handle) }?;
    result.context("IOCTL_STORAGE_GET_DEVICE_NUMBER failed")?;
    Ok(number)
}

/// 查询设备的总线类型
///
/// # Arguments
///
/// * `disk_number` - 磁盘编号
///
/// # Returns
///
/// * `Ok(bus_type)` - 查询成功，返回总线类型
/// * `Err(anyhow::Error)` - 查询失败，返回错误信息
pub fn storage_bus_type(disk_number: u32) -> anyhow::Result<i32> {
    let handle = open_device(&format!(r"\\.\PhysicalDrive{disk_number}"))?;
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageAdapterProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut descriptor = STORAGE_ADAPTER_DESCRIPTOR::default();
    let result = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const c_void),
            mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut descriptor as *mut _ as *mut c_void),
            mem::size_of::<STORAGE_ADAPTER_DESCRIPTOR>() as u32,
            None,
            None,
        )
    };
    unsafe { CloseHandle(handle) }?;
    result.context("IOCTL_STORAGE_QUERY_PROPERTY failed")?;
    Ok(descriptor.BusType as i32)
}

/// 打开设备句柄
///
/// # Arguments
///
/// * `path` - 设备路径
///
/// # Returns
///
/// * `Ok(handle)` - 打开成功，返回设备句柄
/// * `Err(anyhow::Error)` - 打开失败，返回错误信息
pub fn open_device(path: &str) -> anyhow::Result<HANDLE> {
    let path_wide = wide(path);
    unsafe {
        CreateFileW(
            PCWSTR(path_wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .with_context(|| format!("cannot open {path}"))
}

/// 将字符串转换为UTF-16编码的向量
///
/// # Arguments
///
/// * `value` - 要转换的字符串
///
/// # Returns
///
/// * `Vec<u16>` - 转换后的UTF-16编码向量
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

/// 读取固件引导设备信息
///
/// # Returns
///
/// * `Ok(firmware_boot_device)` - 读取成功，返回固件引导设备信息
/// * `Err(anyhow::Error)` - 读取失败，返回错误信息
fn read_firmware_boot_device() -> anyhow::Result<String> {
    LOCAL_MACHINE
        .open(r"SYSTEM\CurrentControlSet\Control")
        .with_context(|| r"Cannot open HKLM\SYSTEM\CurrentControlSet\Control".to_string())?
        .get_string("FirmwareBootDevice")
        .with_context(|| {
            r"Cannot read HKLM\SYSTEM\CurrentControlSet\Control\FirmwareBootDevice".to_string()
        })
}

/// 查询符号链接的目标路径
///
/// # Arguments
///
/// * `arc_name` - 符号链接名称
///
/// # Returns
///
/// * `Ok(target)` - 查询成功，返回符号链接的目标路径
/// * `Err(anyhow::Error)` - 查询失败，返回错误信息
pub fn query_arc_link(arc_name: &str) -> anyhow::Result<String, NtStatus> {
    let (nt_open, nt_query, nt_close) =
        ntdll_functions().map_err(|_| STATUS_PROCEDURE_NOT_FOUND)?;
    let mut name_utf16: Vec<u16> = arc_name.encode_utf16().collect();
    let name_byte_len = name_utf16
        .len()
        .checked_mul(2)
        .ok_or(STATUS_BUFFER_TOO_SMALL)?;
    if name_byte_len > u16::MAX as usize {
        return Err(STATUS_BUFFER_TOO_SMALL);
    }
    let mut object_name = UnicodeString {
        length: name_byte_len as u16,
        maximum_length: name_byte_len as u16,
        buffer: name_utf16.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: HANDLE::default(),
        object_name: &mut object_name,
        attributes: 0,
        security_descriptor: null_mut(),
        security_quality_of_service: null_mut(),
    };
    let mut link_handle = HANDLE::default();
    let status = unsafe { nt_open(&mut link_handle, SYMBOLIC_LINK_QUERY, &mut attributes) };
    if status < 0 {
        return Err(status);
    }
    let mut target_buffer = vec![0u16; 32_768];
    let mut target = UnicodeString {
        length: 0,
        maximum_length: (target_buffer.len() * 2 - 2) as u16,
        buffer: target_buffer.as_mut_ptr(),
    };
    let mut returned_length = 0;
    let status = unsafe { nt_query(link_handle, &mut target, &mut returned_length) };
    unsafe { nt_close(link_handle) };
    if status < 0 {
        return Err(status);
    }
    String::from_utf16(&target_buffer[..target.length as usize / 2])
        .map_err(|_| STATUS_BUFFER_TOO_SMALL)
}

/// 获取NTDLL.dll中的函数指针
///
/// # Returns
///
/// * `Ok((nt_open, nt_query, nt_close))` - 获取成功，返回函数指针
/// * `Err(anyhow::Error)` - 获取失败，返回错误信息
fn ntdll_functions()
-> anyhow::Result<(NtOpenSymbolicLinkObject, NtQuerySymbolicLinkObject, NtClose)> {
    let module_name = wide("ntdll.dll");
    let module = unsafe { GetModuleHandleW(PCWSTR(module_name.as_ptr())) }
        .context("ntdll.dll is not loaded")?;
    unsafe {
        Ok((
            procedure(module, b"NtOpenSymbolicLinkObject\0")?,
            procedure(module, b"NtQuerySymbolicLinkObject\0")?,
            procedure(module, b"NtClose\0")?,
        ))
    }
}

/// 获取NTDLL.dll中的函数指针
///
/// # Arguments
///
/// * `module` - 模块句柄
/// * `name` - 函数名
///
/// # Returns
///
/// * `Ok(address)` - 获取成功，返回函数指针
/// * `Err(anyhow::Error)` - 获取失败，返回错误信息
unsafe fn procedure<T: Copy>(module: HMODULE, name: &[u8]) -> anyhow::Result<T> {
    let address = unsafe { GetProcAddress(module, PCSTR(name.as_ptr())) }.ok_or_else(|| {
        anyhow::anyhow!(
            "ntdll export {} is unavailable",
            String::from_utf8_lossy(&name[..name.len() - 1])
        )
    })?;
    Ok(unsafe { mem::transmute_copy(&address) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(
        root: &str,
        drive_type: u32,
        disk_number: Option<u32>,
        bus_type: Option<i32>,
        device: Option<&str>,
    ) -> Volume {
        Volume {
            root: root.into(),
            dos_device: device.map(str::to_owned),
            drive_type,
            device_number: disk_number.map(|device_number| STORAGE_DEVICE_NUMBER {
                DeviceType: FILE_DEVICE_DISK.0,
                DeviceNumber: device_number,
                PartitionNumber: 1,
            }),
            bus_type,
        }
    }

    #[test]
    fn validates_only_root_relative_paths() {
        for path in ["FirPE", "FirPE\\Version.txt", "directory/file"] {
            assert!(validate_relative_path(path).is_ok());
        }
        for path in [
            "",
            "\\FirPE",
            "/FirPE",
            "C:\\FirPE",
            "FirPE\\..\\x",
            "FirPE\\\\x",
            ".",
        ] {
            assert!(validate_relative_path(path).is_err());
        }
    }

    #[test]
    fn classifies_volumes_in_search_order() {
        let boot = r"\Device\HarddiskVolume1";
        let samples = [
            (
                volume("C:\\", DRIVE_FIXED, Some(1), None, Some(boot)),
                SearchGroup::Boot,
            ),
            (
                volume("D:\\", DRIVE_FIXED, Some(1), None, None),
                SearchGroup::SameDisk,
            ),
            (
                volume("E:\\", DRIVE_FIXED, Some(2), Some(BUS_TYPE_USB), None),
                SearchGroup::Usb,
            ),
            (
                volume("F:\\", DRIVE_REMOVABLE, Some(3), None, None),
                SearchGroup::Removable,
            ),
            (
                volume("G:\\", DRIVE_CDROM, None, None, None),
                SearchGroup::Optical,
            ),
            (
                volume("H:\\", DRIVE_FIXED, Some(4), None, None),
                SearchGroup::Fixed,
            ),
            (
                volume("I:\\", DRIVE_RAMDISK, None, None, None),
                SearchGroup::VirtualOrRam,
            ),
            (
                volume("J:\\", DRIVE_REMOTE, None, None, None),
                SearchGroup::Network,
            ),
        ];
        for (volume, expected) in samples {
            assert_eq!(classify(&volume, Some(boot), Some(1)), expected);
        }
    }

    #[test]
    fn search_groups_sort_before_drive_letters() {
        let boot = r"\Device\HarddiskVolume1";
        let mut volumes = vec![
            volume("Z:\\", DRIVE_REMOTE, None, None, None),
            volume("Y:\\", DRIVE_FIXED, Some(2), Some(BUS_TYPE_USB), None),
            volume("X:\\", DRIVE_FIXED, Some(1), None, None),
            volume("W:\\", DRIVE_FIXED, Some(1), None, Some(boot)),
        ];

        volumes.sort_by_key(|volume| (classify(volume, Some(boot), Some(1)), volume.root.clone()));

        assert_eq!(
            volumes
                .into_iter()
                .map(|volume| volume.root)
                .collect::<Vec<_>>(),
            ["W:\\", "X:\\", "Y:\\", "Z:\\"]
        );
    }

    #[test]
    fn parses_arc_disk_and_partition_paths() {
        assert_eq!(
            parse_arc_device_number(r"\Device\Harddisk12\Partition34"),
            Some(STORAGE_DEVICE_NUMBER {
                DeviceType: FILE_DEVICE_DISK.0,
                DeviceNumber: 12,
                PartitionNumber: 34,
            })
        );
        assert_eq!(parse_arc_device_number(r"\Device\HarddiskVolume3"), None);
    }
}
