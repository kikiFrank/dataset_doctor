use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::workspace::{ordinary_file, prevent_replacement};

#[cfg(feature = "execution-rpc")]
pub(crate) fn registered_digest(path: &Path, limit: u64) -> io::Result<Option<String>> {
    let file = match open_registered_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut digest = Sha256::new();
    if io::copy(&mut file.take(limit + 1), &mut digest)? > limit {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "Registered file exceeds its size limit",
        ));
    }
    Ok(Some(format!("{:x}", digest.finalize())))
}

#[cfg(feature = "execution-rpc")]
pub(crate) fn replace_verified_file(
    path: &Path,
    before: Option<&str>,
    after: &str,
    bytes: &[u8],
    limit: u64,
) -> io::Result<bool> {
    let actual = registered_digest(path, limit)?;
    if actual.as_deref() == Some(after) {
        return Ok(true);
    }
    if actual.as_deref() != before {
        return Ok(false);
    }
    let parent = path.parent().ok_or(ErrorKind::InvalidData)?;
    if parent.canonicalize()? != parent {
        return Ok(false);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    if registered_digest(path, limit)? != actual {
        return Ok(false);
    }
    let published = if actual.is_some() {
        temporary.persist(path)
    } else {
        temporary.persist_noclobber(path)
    };
    match published {
        Ok(file) => drop(file),
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error.error),
    }
    Ok(registered_digest(path, limit)?.as_deref() == Some(after))
}

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

pub(crate) fn open_registered_file(path: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .filter(|parent| parent.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidData,
                "Expected an absolute registered path",
            )
        })?;
    let name = path.file_name().ok_or(ErrorKind::InvalidData)?;
    if parent.canonicalize()? != parent {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "Registered directory moved",
        ));
    }
    open_scoped_file(parent, Path::new(name))
}

pub(crate) fn scoped_directory(root: &Path, relative: &Path) -> io::Result<PathBuf> {
    if !is_portable_relative_file(relative) {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "Expected a relative directory",
        ));
    }
    let mut directory = root.to_owned();
    for component in relative.components() {
        directory.push(component);
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        if !directory.is_dir() || directory.canonicalize()? != directory {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "Directory is outside its scope",
            ));
        }
    }
    Ok(directory)
}

pub(crate) fn publish_immutable(root: &Path, relative: &Path, bytes: &[u8]) -> io::Result<()> {
    if !is_portable_relative_file(relative) {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "Expected a relative artifact path",
        ));
    }
    let parent = relative
        .parent()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "Missing artifact directory"))?;
    let directory = scoped_directory(root, parent)?;
    let mut staging = tempfile::NamedTempFile::new_in(&directory)?;
    staging.write_all(bytes)?;
    staging.as_file().sync_all()?;
    match staging.persist_noclobber(root.join(relative)) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
            let mut existing = Vec::new();
            open_scoped_file(root, relative)?
                .take(bytes.len() as u64 + 1)
                .read_to_end(&mut existing)?;
            if existing != bytes {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "Existing immutable artifact differs",
                ));
            }
            Ok(())
        }
        Err(error) => Err(error.error),
    }
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

pub(crate) fn copy_and_hash(
    source: &mut File,
    output: &mut impl Write,
) -> io::Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            return Ok((size, format!("{:x}", hash.finalize())));
        }
        output.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
        size += count as u64;
    }
}
