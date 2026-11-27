//! Validate a separately selected annotation-write scope before accepting Final.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use dd_core::workspace::WorkspacePathGrant;
use uuid::Uuid;

use super::{ReviewFinalError, ReviewFinalService, new_annotation_path, verified_parent};
use crate::workspace::ordinary_file;

pub struct ReviewWriteAccess {
    pub authorization_required: bool,
    pub read_only: bool,
}

impl ReviewFinalService {
    pub fn write_access(
        &self,
        session: Uuid,
        review: Uuid,
    ) -> Result<ReviewWriteAccess, ReviewFinalError> {
        let annotation = self.annotation_target(session, review)?;
        let root = self
            .owner
            .storage_root(session)
            .map_err(ReviewFinalError::Storage)?;
        let read_only = match fs::symlink_metadata(&annotation) {
            Ok(metadata) => !ordinary_file(&metadata) || metadata.permissions().readonly(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        Ok(ReviewWriteAccess {
            authorization_required: !annotation.starts_with(root),
            read_only,
        })
    }

    /// Only the trusted native selector may supply this selected directory.
    pub fn authorize_write(
        &self,
        session: Uuid,
        review: Uuid,
        selected: PathBuf,
    ) -> Result<WorkspacePathGrant, ReviewFinalError> {
        if !selected.is_absolute() {
            return Err(ReviewFinalError::WriteAuthorizationRequired);
        }
        let root = selected.canonicalize()?;
        let annotation = self.annotation_target(session, review)?;
        if !root.is_dir() || !annotation.starts_with(&root) {
            return Err(ReviewFinalError::WriteAuthorizationRequired);
        }
        probe_write(&annotation)?;
        self.owner
            .grant_annotation_directory(session, root)
            .map_err(ReviewFinalError::Storage)
    }

    fn annotation_target(&self, session: Uuid, review: Uuid) -> Result<PathBuf, ReviewFinalError> {
        let review = self
            .owner
            .get_review(session, review)
            .map_err(ReviewFinalError::Storage)?;
        let image = self
            .owner
            .registered_image(session, review.dataset_id, review.asset_id)
            .map_err(ReviewFinalError::Storage)?;
        image
            .annotation
            .map(|file| Ok(file.path))
            .unwrap_or_else(|| new_annotation_path(&image.image.path))
    }
}

pub(super) fn probe_write(annotation: &Path) -> Result<(), ReviewFinalError> {
    let parent = verified_parent(annotation)?;
    match fs::symlink_metadata(annotation) {
        Ok(metadata) => {
            if !ordinary_file(&metadata) || metadata.permissions().readonly() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Annotation is read-only or redirected",
                )
                .into());
            }
            let mut options = OpenOptions::new();
            options.read(true).write(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                // Request DELETE as well as read/write access without modifying bytes.
                const DELETE_ACCESS: u32 = 0x0001_0000;
                const GENERIC_READ_WRITE: u32 = 0xC000_0000;
                options.access_mode(DELETE_ACCESS | GENERIC_READ_WRITE);
            }
            drop(options.open(annotation)?);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let mut probe = tempfile::NamedTempFile::new_in(parent)?;
    probe.write_all(b"Dataset Doctor annotation write probe\n")?;
    probe.as_file().sync_all()?;
    Ok(())
}
