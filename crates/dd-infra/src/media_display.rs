//! Session-scoped read grants for registered image bytes displayed by the UI.

use std::{
    collections::HashMap,
    error::Error,
    fmt::{self, Display},
    io::{self, Read},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::dataset_query::{DatasetQueryError, RegisteredInputFile};
use crate::paths::open_registered_file;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
pub struct MediaLocator {
    pub grant_id: Uuid,
    pub resource_digest: String,
}

#[derive(Debug)]
pub struct DisplayImage {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
    pub digest: String,
}

#[derive(Debug)]
pub enum MediaDisplayError {
    Storage(WorkspaceThreadError),
    Io(io::Error),
    UnknownGrant,
    Expired,
    InputChanged,
    UnsupportedMedia,
    Unavailable,
}

impl Display for MediaDisplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => Display::fmt(error, f),
            Self::Io(error) => write!(f, "Read display image: {error}"),
            Self::UnknownGrant => f.write_str("Unknown image display grant"),
            Self::Expired => f.write_str("Image display grant expired"),
            Self::InputChanged => f.write_str("Registered image bytes changed"),
            Self::UnsupportedMedia => f.write_str("Only PNG and JPEG images can be displayed"),
            Self::Unavailable => f.write_str("Image display grant state is unavailable"),
        }
    }
}
impl Error for MediaDisplayError {}

#[derive(Clone)]
struct Grant {
    session: Uuid,
    image: RegisteredInputFile,
    issued_at: Instant,
}

pub struct MediaDisplayService {
    owner: Arc<WorkspaceThread>,
    lifetime: Duration,
    grants: Mutex<HashMap<Uuid, Grant>>,
}

impl MediaDisplayService {
    pub fn new(owner: Arc<WorkspaceThread>, lifetime: Duration) -> Self {
        Self {
            owner,
            lifetime,
            grants: Mutex::new(HashMap::new()),
        }
    }

    pub fn issue(
        &self,
        session: Uuid,
        asset: Uuid,
        now: Instant,
    ) -> Result<MediaLocator, MediaDisplayError> {
        let image = self
            .owner
            .workspace_action(session, move |owner| {
                let dataset: String = owner
                    .connection
                    .query_row(
                        "SELECT dataset_id FROM dataset_assets WHERE asset_id=?1",
                        [asset.to_string()],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(database)?
                    .ok_or(WorkspaceThreadError::Dataset(
                        DatasetQueryError::UnknownAsset,
                    ))?;
                let dataset = Uuid::parse_str(&dataset).map_err(|_| {
                    WorkspaceThreadError::Dataset(DatasetQueryError::InvalidImageBinding)
                })?;
                owner
                    .registered_image(dataset, asset)
                    .map(|binding| binding.image)
                    .map_err(WorkspaceThreadError::Dataset)
            })
            .map_err(MediaDisplayError::Storage)?;
        self.issue_verified_image(session, image, now)
    }

    pub(crate) fn issue_verified_image(
        &self,
        session: Uuid,
        image: RegisteredInputFile,
        now: Instant,
    ) -> Result<MediaLocator, MediaDisplayError> {
        self.require_session(session)?;
        let mut grants = self
            .grants
            .lock()
            .map_err(|_| MediaDisplayError::Unavailable)?;
        grants.retain(|_, grant| {
            grant.session == session
                && now.saturating_duration_since(grant.issued_at) < self.lifetime
        });
        let id = Uuid::new_v4();
        let resource_digest = image.sha256.clone();
        grants.insert(
            id,
            Grant {
                session,
                image,
                issued_at: now,
            },
        );
        Ok(MediaLocator {
            grant_id: id,
            resource_digest,
        })
    }

    pub fn resolve(&self, id: Uuid, now: Instant) -> Result<DisplayImage, MediaDisplayError> {
        let grant = self
            .grants
            .lock()
            .map_err(|_| MediaDisplayError::Unavailable)?
            .get(&id)
            .cloned()
            .ok_or(MediaDisplayError::UnknownGrant)?;
        if now.saturating_duration_since(grant.issued_at) >= self.lifetime {
            return Err(MediaDisplayError::Expired);
        }
        self.require_session(grant.session)?;
        let mut bytes = Vec::new();
        open_registered_file(&grant.image.path)
            .map_err(MediaDisplayError::Io)?
            .take(MAX_IMAGE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(MediaDisplayError::Io)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if bytes.len() as u64 > MAX_IMAGE_BYTES || digest != grant.image.sha256 {
            return Err(MediaDisplayError::InputChanged);
        }
        let media_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            "image/jpeg"
        } else {
            return Err(MediaDisplayError::UnsupportedMedia);
        };
        self.require_session(grant.session)?;
        Ok(DisplayImage {
            bytes,
            media_type,
            digest,
        })
    }

    fn require_session(&self, session: Uuid) -> Result<(), MediaDisplayError> {
        self.owner
            .current()
            .map_err(MediaDisplayError::Storage)?
            .filter(|current| current.workspace_session_id == session)
            .ok_or(MediaDisplayError::UnknownGrant)
            .map(|_| ())
    }
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Dataset(DatasetQueryError::Database(error))
}
