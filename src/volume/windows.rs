use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use super::{VolumeInfo, VolumeKind};

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

unsafe extern "system" {
    fn GetLogicalDrives() -> u32;
    fn GetDriveTypeW(root_path_name: *const u16) -> u32;
    fn GetDiskFreeSpaceExW(
        directory_name: *const u16,
        free_bytes_available_to_caller: *mut u64,
        total_number_of_bytes: *mut u64,
        total_number_of_free_bytes: *mut u64,
    ) -> i32;
    fn GetVolumeInformationW(
        root_path_name: *const u16,
        volume_name_buffer: *mut u16,
        volume_name_size: u32,
        volume_serial_number: *mut u32,
        maximum_component_length: *mut u32,
        file_system_flags: *mut u32,
        file_system_name_buffer: *mut u16,
        file_system_name_size: u32,
    ) -> i32;
}

pub fn list_volumes() -> Vec<VolumeInfo> {
    let drive_mask = unsafe { GetLogicalDrives() };
    let mut result = Vec::new();

    for index in 0..26u32 {
        if drive_mask & (1 << index) == 0 {
            continue;
        }

        let letter = (b'A' + index as u8) as char;
        let root = format!("{letter}:\\");
        let root_wide = wide(&root);

        let drive_type = unsafe { GetDriveTypeW(root_wide.as_ptr()) };
        let kind = match drive_type {
            DRIVE_REMOVABLE => VolumeKind::Removable,
            DRIVE_FIXED => VolumeKind::Fixed,
            DRIVE_REMOTE => VolumeKind::Network,
            DRIVE_CDROM | DRIVE_RAMDISK => continue,
            _ => VolumeKind::Other,
        };

        let mut free_to_caller = 0u64;
        let mut total_bytes = 0u64;
        let mut total_free_bytes = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                root_wide.as_ptr(),
                &mut free_to_caller,
                &mut total_bytes,
                &mut total_free_bytes,
            )
        };
        if ok == 0 {
            continue;
        }

        let mut label_buffer = vec![0u16; 261];
        let mut fs_buffer = vec![0u16; 261];
        let mut serial = 0u32;
        let mut max_component = 0u32;
        let mut flags = 0u32;
        let info_ok = unsafe {
            GetVolumeInformationW(
                root_wide.as_ptr(),
                label_buffer.as_mut_ptr(),
                label_buffer.len() as u32,
                &mut serial,
                &mut max_component,
                &mut flags,
                fs_buffer.as_mut_ptr(),
                fs_buffer.len() as u32,
            )
        };

        let label = if info_ok != 0 {
            let label = String::from_utf16_lossy(&label_buffer);
            let label = label.trim_end_matches('\0').trim().to_string();
            (!label.is_empty()).then_some(label)
        } else {
            None
        };

        let fs = if info_ok != 0 {
            String::from_utf16_lossy(&fs_buffer)
                .trim_end_matches('\0')
                .to_string()
        } else {
            String::new()
        };

        result.push(VolumeInfo {
            mount_point: PathBuf::from(root),
            device: None,
            fs,
            total_bytes,
            free_bytes: total_free_bytes,
            kind,
            label,
        });
    }

    result
}

pub fn available_space(path: &Path) -> Result<u64> {
    let path_text = path.to_string_lossy();
    let path_wide = wide(&path_text);
    let mut free_to_caller = 0u64;
    let mut total_bytes = 0u64;
    let mut total_free_bytes = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            path_wide.as_ptr(),
            &mut free_to_caller,
            &mut total_bytes,
            &mut total_free_bytes,
        )
    };
    if ok == 0 {
        return Err(anyhow!(
            "GetDiskFreeSpaceExW failed for {}",
            path.display()
        ));
    }
    Ok(total_free_bytes)
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
