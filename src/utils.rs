use std::{ffi::c_void, fmt, mem, ptr::null_mut};

use anyhow::{Context, bail};
use windows::{
    Win32::{
        Foundation::{CloseHandle, GENERIC_READ, HANDLE, HMODULE, LUID},
        Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
            SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_DEVICE_DISK, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ,
            FILE_SHARE_WRITE, GetDriveTypeW, GetFileAttributesW, GetLogicalDrives,
            INVALID_FILE_ATTRIBUTES, OPEN_EXISTING, QueryDosDeviceW, ReadFile,
        },
        System::{
            IO::DeviceIoControl,
            Ioctl::{
                IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_QUERY_PROPERTY,
                PropertyStandardQuery, STORAGE_ADAPTER_DESCRIPTOR, STORAGE_DEVICE_NUMBER,
                STORAGE_PROPERTY_QUERY, StorageAdapterProperty,
            },
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
            SystemInformation::{FIRMWARE_TABLE_PROVIDER, GetSystemFirmwareTable},
            Threading::{GetCurrentProcess, OpenProcessToken},
            WindowsProgramming::GetFirmwareEnvironmentVariableA,
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
const VENTOY_OS_PARAM_SIZE: usize = 512;
const VENTOY_DISK_GUID_OFFSET: usize = 17;
const VENTOY_PARTITION_ID_OFFSET: usize = 41;
const VENTOY_IMAGE_PATH_OFFSET: usize = 45;
const VENTOY_IMAGE_PATH_LENGTH: usize = 384;
const VENTOY_DISK_GUID_SECTOR_OFFSET: usize = 0x180;
const VENTOY_MAGIC: [u8; 16] = [
    0x20, 0x20, 0x77, 0x77, 0x77, 0x2e, 0x76, 0x65, 0x6e, 0x74, 0x6f, 0x79, 0x2e, 0x6e, 0x65, 0x74,
];
const VENTOY_VARIABLE_NAME: &[u8] = b"VentoyOsParam\0";
const VENTOY_VARIABLE_GUID: &[u8] = b"{77772020-2e77-6576-6e74-6f792e6e6574}\0";
const SYSTEM_ENVIRONMENT_PRIVILEGE: &str = "SeSystemEnvironmentPrivilege";

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
    /// Ventoy数据卷
    Ventoy,
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
            Self::Ventoy => "ventoy",
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

#[derive(Debug)]
struct VentoyOsParam {
    disk_guid: [u8; 16],
    partition_number: u32,
    image_path: String,
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

    let ventoy_volume = find_ventoy_volume(&volumes, verbose);

    let mut candidates: Vec<&Volume> = volumes.iter().collect();
    candidates.sort_by_key(|volume| {
        (
            classify(
                volume,
                boot_nt_path.as_deref(),
                boot_disk,
                ventoy_volume.map(|volume| volume.root.as_str()),
            ),
            &volume.root,
        )
    });

    for volume in candidates {
        let group = classify(
            volume,
            boot_nt_path.as_deref(),
            boot_disk,
            ventoy_volume.map(|volume| volume.root.as_str()),
        );
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
fn classify(
    volume: &Volume,
    boot_nt_path: Option<&str>,
    boot_disk: Option<u32>,
    ventoy_root: Option<&str>,
) -> SearchGroup {
    if boot_nt_path
        .is_some_and(|path| volume_matches_boot(volume, path, parse_arc_device_number(path)))
    {
        return SearchGroup::Boot;
    }
    if boot_disk.is_some() && volume.device_number.map(|number| number.DeviceNumber) == boot_disk {
        return SearchGroup::SameDisk;
    }
    if Some(volume.root.as_str()) == ventoy_root {
        return SearchGroup::Ventoy;
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

/// 查找Ventoy卷
///
/// # Arguments
///
/// * `volumes` - 所有卷信息
/// * `verbose` - 是否打印详细信息
///
/// # Returns
///
/// * `volume` - Ventoy卷信息
fn find_ventoy_volume<'a>(volumes: &'a [Volume], verbose: bool) -> Option<&'a Volume> {
    let param = match read_ventoy_os_param(verbose) {
        Some(param) => param,
        None => return None,
    };

    let volume = volumes.iter().find(|volume| {
        let Some(device) = volume.device_number else {
            return false;
        };
        device.PartitionNumber == param.partition_number
            && physical_disk_ventoy_guid(device.DeviceNumber)
                .is_ok_and(|guid| guid == param.disk_guid)
            && path_exists(&format!("{}{}", volume.root, param.image_path))
    });

    if verbose {
        match volume {
            Some(volume) => eprintln!(
                "Ventoy data volume {} contains the current ISO {}; assigning Ventoy priority.",
                volume.root, param.image_path
            ),
            None => eprintln!(
                "Warning: Ventoy runtime data was found, but its data volume or ISO {} is not mounted.",
                param.image_path
            ),
        }
    }
    volume
}

/// 读取 Ventoy OS 参数
///
/// # Returns
///
/// * `param` - Ventoy OS参数
fn read_ventoy_os_param(verbose: bool) -> Option<VentoyOsParam> {
    if let Err(error) = enable_system_environment_privilege() {
        if verbose {
            eprintln!(
                "Warning: cannot enable {SYSTEM_ENVIRONMENT_PRIVILEGE} for Ventoy UEFI data: {error}; checking available firmware tables."
            );
        }
    }

    let mut buffer = [0u8; VENTOY_OS_PARAM_SIZE];
    let size = unsafe {
        GetFirmwareEnvironmentVariableA(
            PCSTR(VENTOY_VARIABLE_NAME.as_ptr()),
            PCSTR(VENTOY_VARIABLE_GUID.as_ptr()),
            Some(buffer.as_mut_ptr() as *mut c_void),
            buffer.len() as u32,
        )
    };
    if size as usize == buffer.len() {
        if let Some(param) = parse_ventoy_os_param(&buffer) {
            if verbose {
                eprintln!("Ventoy runtime parameter read from the UEFI variable.");
            }
            return Some(param);
        }
    }

    for table_id in [u32::from_le_bytes(*b"VTOY"), u32::from_le_bytes(*b"iBFT")] {
        let provider = FIRMWARE_TABLE_PROVIDER(u32::from_le_bytes(*b"ACPI"));
        let size = unsafe { GetSystemFirmwareTable(provider, table_id, None) } as usize;
        if size < VENTOY_OS_PARAM_SIZE {
            continue;
        }
        let mut table = vec![0u8; size];
        let bytes_read =
            unsafe { GetSystemFirmwareTable(provider, table_id, Some(&mut table)) } as usize;
        if bytes_read == 0 || bytes_read > table.len() {
            continue;
        }
        if let Some(param) = table[..bytes_read]
            .windows(VENTOY_OS_PARAM_SIZE)
            .find_map(parse_ventoy_os_param)
        {
            if verbose {
                let table_name = if table_id == u32::from_le_bytes(*b"VTOY") {
                    "VTOY"
                } else {
                    "iBFT"
                };
                eprintln!("Ventoy runtime parameter read from ACPI {table_name}.");
            }
            return Some(param);
        }
    }
    if verbose {
        eprintln!("Ventoy runtime parameter was not found in UEFI or ACPI data.");
    }
    None
}

/// 启用 SeSystemEnvironmentPrivilege 权限
fn enable_system_environment_privilege() -> anyhow::Result<()> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    }
    .context("cannot open the current process token")?;

    let privilege_name = wide(SYSTEM_ENVIRONMENT_PRIVILEGE);
    let mut luid = LUID::default();
    let result = (|| {
        unsafe { LookupPrivilegeValueW(None, PCWSTR(privilege_name.as_ptr()), &mut luid) }
            .context("cannot resolve SeSystemEnvironmentPrivilege")?;
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        unsafe { AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None) }
            .context("cannot enable SeSystemEnvironmentPrivilege")
    })();
    let close_result = unsafe { CloseHandle(token) };
    result?;
    close_result.context("cannot close the current process token")?;
    Ok(())
}

/// 解析 Ventoy OS 参数
///
/// # Arguments
///
/// * `buffer` - Ventoy OS参数缓冲区
///
/// # Returns
///
/// * `param` - Ventoy OS参数
fn parse_ventoy_os_param(buffer: &[u8]) -> Option<VentoyOsParam> {
    if buffer.len() != VENTOY_OS_PARAM_SIZE
        || buffer[..VENTOY_MAGIC.len()] != VENTOY_MAGIC
        || buffer.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0
    {
        return None;
    }

    let mut disk_guid = [0u8; 16];
    disk_guid.copy_from_slice(&buffer[VENTOY_DISK_GUID_OFFSET..VENTOY_DISK_GUID_OFFSET + 16]);
    let partition_number = u16::from_le_bytes(
        buffer[VENTOY_PARTITION_ID_OFFSET..VENTOY_PARTITION_ID_OFFSET + 2]
            .try_into()
            .ok()?,
    ) as u32;
    let image_path_bytes =
        &buffer[VENTOY_IMAGE_PATH_OFFSET..VENTOY_IMAGE_PATH_OFFSET + VENTOY_IMAGE_PATH_LENGTH];
    let image_path_end = image_path_bytes.iter().position(|byte| *byte == 0)?;
    let image_path = std::str::from_utf8(&image_path_bytes[..image_path_end])
        .ok()?
        .trim_start_matches(['/', '\\'])
        .replace('/', "\\");
    if partition_number == 0 || validate_relative_path(&image_path).is_err() {
        return None;
    }
    Some(VentoyOsParam {
        disk_guid,
        partition_number,
        image_path,
    })
}

/// 读取物理磁盘的 Ventoy GUID
///
/// # Arguments
///
/// * `disk_number` - 物理磁盘号
///
/// # Returns
///
/// * `guid` - 物理磁盘的 Ventoy GUID
fn physical_disk_ventoy_guid(disk_number: u32) -> anyhow::Result<[u8; 16]> {
    let handle =
        open_device_with_access(&format!(r"\\.\PhysicalDrive{disk_number}"), GENERIC_READ.0)?;
    let mut sector = [0u8; 512];
    let mut bytes_read = 0;
    let read_result = unsafe { ReadFile(handle, Some(&mut sector), Some(&mut bytes_read), None) };
    let close_result = unsafe { CloseHandle(handle) };
    read_result.context("cannot read Ventoy disk header")?;
    close_result.context("cannot close Ventoy disk header handle")?;
    if bytes_read as usize != sector.len() {
        bail!("Ventoy disk header is shorter than one sector");
    }
    let mut disk_guid = [0u8; 16];
    disk_guid.copy_from_slice(
        &sector[VENTOY_DISK_GUID_SECTOR_OFFSET..VENTOY_DISK_GUID_SECTOR_OFFSET + 16],
    );
    Ok(disk_guid)
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
    open_device_with_access(path, 0)
}

/// 以指定访问权限打开设备句柄
///
/// # Arguments
///
/// * `path` - 设备路径
/// * `access` - 访问权限
///
/// # Returns
///
/// * `Ok(handle)` - 打开成功，返回设备句柄
/// * `Err(anyhow::Error)` - 打开失败，返回错误信息
fn open_device_with_access(path: &str, access: u32) -> anyhow::Result<HANDLE> {
    let path_wide = wide(path);
    unsafe {
        CreateFileW(
            PCWSTR(path_wide.as_ptr()),
            access,
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
        for path in ["WinPE", "WinPE\\Version.txt", "directory/file"] {
            assert!(validate_relative_path(path).is_ok());
        }
        for path in [
            "",
            "\\WinPE",
            "/WinPE",
            "C:\\WinPE",
            "WinPE\\..\\x",
            "WinPE\\\\x",
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
            assert_eq!(classify(&volume, Some(boot), Some(1), None), expected);
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

        volumes.sort_by_key(|volume| {
            (
                classify(volume, Some(boot), Some(1), None),
                volume.root.clone(),
            )
        });

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

    #[test]
    fn validates_ventoy_runtime_data() {
        let mut buffer = [0u8; VENTOY_OS_PARAM_SIZE];
        buffer[..VENTOY_MAGIC.len()].copy_from_slice(&VENTOY_MAGIC);
        buffer[VENTOY_DISK_GUID_OFFSET..VENTOY_DISK_GUID_OFFSET + 16].copy_from_slice(&[0xA5; 16]);
        buffer[VENTOY_PARTITION_ID_OFFSET..VENTOY_PARTITION_ID_OFFSET + 2]
            .copy_from_slice(&1u16.to_le_bytes());
        let image_path = b"/ISO/WinPE.iso\0";
        buffer[VENTOY_IMAGE_PATH_OFFSET..VENTOY_IMAGE_PATH_OFFSET + image_path.len()]
            .copy_from_slice(image_path);
        let checksum_index = VENTOY_OS_PARAM_SIZE - 1;
        buffer[checksum_index] = 0u8.wrapping_sub(
            buffer[..checksum_index]
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
        );

        let param = parse_ventoy_os_param(&buffer).unwrap();
        assert_eq!(param.disk_guid, [0xA5; 16]);
        assert_eq!(param.partition_number, 1);
        assert_eq!(param.image_path, "ISO\\WinPE.iso");

        buffer[0] ^= 1;
        assert!(parse_ventoy_os_param(&buffer).is_none());
    }

    #[test]
    fn ventoy_data_volume_follows_firmware_disk() {
        let boot = r"\Device\HarddiskVolume1";
        let firmware_disk = volume("D:\\", DRIVE_FIXED, Some(1), None, None);
        let ventoy_volume = volume("F:\\", DRIVE_REMOVABLE, Some(2), Some(BUS_TYPE_USB), None);

        assert_eq!(
            classify(&firmware_disk, Some(boot), Some(1), Some("F:\\")),
            SearchGroup::SameDisk
        );
        assert_eq!(
            classify(&ventoy_volume, Some(boot), Some(1), Some("F:\\")),
            SearchGroup::Ventoy
        );
    }
}
