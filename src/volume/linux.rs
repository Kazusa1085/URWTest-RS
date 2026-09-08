use std::collections::HashSet;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{VolumeInfo, VolumeKind};

pub fn list_volumes() -> Vec<VolumeInfo> {
    let mounts = match fs::read_to_string("/proc/mounts") {
        Ok(mounts) => mounts,
        Err(_) => return Vec::new(),
    };

    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }

        let device = parts[0];
        let mount_point = unescape_mount_path(parts[1]);
        let fs_type = parts[2];
        let options = parts[3];

        if !device.starts_with("/dev/") {
            continue;
        }
        if device.starts_with("/dev/loop") {
            continue;
        }
        if is_pseudo_filesystem(fs_type) {
            continue;
        }
        if options.split(',').any(|option| option == "ro") {
            continue;
        }

        let mount_path = PathBuf::from(&mount_point);
        if !mount_path.is_dir() {
            continue;
        }
        if !seen.insert(mount_point.clone()) {
            continue;
        }

        let (total_bytes, free_bytes) = statvfs(&mount_path).unwrap_or((0, 0));
        let removable = is_removable(device);
        let kind = if removable {
            VolumeKind::Removable
        } else {
            VolumeKind::Fixed
        };

        result.push(VolumeInfo {
            mount_point: mount_path,
            device: Some(device.to_string()),
            fs: fs_type.to_string(),
            total_bytes,
            free_bytes,
            kind,
            label: None,
        });
    }

    result.sort_by(|left, right| left.mount_point.cmp(&right.mount_point));
    result
}

pub fn available_space(path: &Path) -> Result<u64> {
    statvfs(path)
        .map(|(_, free)| free)
        .with_context(|| format!("failed to read free space for {}", path.display()))
}

fn statvfs(path: &Path) -> Option<(u64, u64)> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }

    let fragment_size = stat.f_frsize as u64;
    let total = stat.f_blocks as u64 * fragment_size;
    let free = stat.f_bavail as u64 * fragment_size;
    Some((total, free))
}

fn is_removable(device: &str) -> bool {
    let Some(name) = Path::new(device).file_name() else {
        return false;
    };
    let name = name.to_string_lossy();
    let sys_path = PathBuf::from(format!("/sys/class/block/{name}"));

    if let Ok(value) = fs::read_to_string(sys_path.join("removable")) {
        return value.trim() == "1";
    }

    // For a partition such as /dev/sdb1, /sys/class/block/sdb1/removable may
    // not exist. Resolve the symlink and check the parent block device.
    if let Ok(real) = fs::canonicalize(&sys_path)
        && let Some(parent) = real.parent()
        && let Ok(value) = fs::read_to_string(parent.join("removable"))
    {
        return value.trim() == "1";
    }

    false
}

fn is_pseudo_filesystem(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "proc"
            | "sysfs"
            | "tmpfs"
            | "devtmpfs"
            | "devpts"
            | "cgroup"
            | "cgroup2"
            | "overlay"
            | "squashfs"
            | "securityfs"
            | "debugfs"
            | "tracefs"
            | "configfs"
            | "fusectl"
            | "mqueue"
            | "hugetlbfs"
            | "pstore"
            | "bpf"
            | "autofs"
            | "rpc_pipefs"
            | "nsfs"
            | "binfmt_misc"
            | "efivarfs"
            | "ramfs"
            | "fuse.gvfsd-fuse"
            | "fuse.portal"
            | "fuse.snapfuse"
    )
}

fn unescape_mount_path(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0usize;

    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && let Ok(text) = std::str::from_utf8(&bytes[index + 1..index + 4])
            && let Ok(value) = u8::from_str_radix(text, 8)
        {
            output.push(value);
            index += 4;
            continue;
        }
        output.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&output).into_owned()
}
