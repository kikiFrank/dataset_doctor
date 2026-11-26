//! Path-free Artifact references stored by Core and exposed through adapters.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "SCREAMING_SNAKE_CASE",
    deny_unknown_fields
)]
pub enum ArtifactOwner {
    Task(Uuid),
    Asset(Uuid),
    Review(Uuid),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReference {
    pub artifact_id: Uuid,
    pub owner: ArtifactOwner,
    pub kind: String,
    pub media_type: String,
    pub sha256: String,
    pub size_bytes: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_id: Option<String>,
}
