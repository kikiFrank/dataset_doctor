use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};

use crate::workspace::{ordinary_file, prevent_replacement};

pub(crate) fn is_portable_relative_file(path: &Path) -> bool {
    path.file_name().is_some()
        && path.components().all(|component| {
            matches!(component, Component::Normal(name) if name.to_str().is_some_and(|name| {
                !name.contains([':', '\\']) && !name.ends_with(['.', ' '])
            }))
        })
}

/// Read a regular file within an already canonicalized, approved directory.
pub(crate) fn open_scoped_file(root: &Path, relative: &Path) -> io::Result<File> {
    if !is_portable_relative_file(relative) {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "Expected a relative file path",
        ));
    }
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    if !ordinary_file(&metadata) || !path.canonicalize()?.starts_with(root) {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "File is outside the approved directory",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    prevent_replacement(&mut options);
    let file = options.open(&path)?;
    if !ordinary_file(&file.metadata()?) || !opened_path(&file, &path)?.starts_with(root) {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "Opened file is outside the approved directory",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn opened_path(file: &File, _: &Path) -> io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    // The device-qualified path uses the same spelling as canonicalize().
    let handle = file.as_raw_handle();
    let required = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, 0) };
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0; required as usize];
    let length = unsafe { GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), required, 0) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length >= required {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "File path changed while opening",
        ));
    }
    Ok(PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
}

#[cfg(not(windows))]
fn opened_path(_: &File, path: &Path) -> io::Result<PathBuf> {
    path.canonicalize()
}
