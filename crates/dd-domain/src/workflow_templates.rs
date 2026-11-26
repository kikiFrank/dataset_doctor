#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TemplateIdentity {
    pub id: &'static str,
    pub version: u16,
}

pub const DATASET_HEALTHCHECK: TemplateIdentity = TemplateIdentity {
    id: "industrial_dataset_healthcheck.v1",
    version: 1,
};
pub const VIDEO_TO_DATASET: TemplateIdentity = TemplateIdentity {
    id: "video_to_dataset.v1",
    version: 1,
};
pub const REPAIR_RECHECK: TemplateIdentity = TemplateIdentity {
    id: "repair_recheck.v1",
    version: 1,
};

pub const REGISTERED: [TemplateIdentity; 3] =
    [DATASET_HEALTHCHECK, VIDEO_TO_DATASET, REPAIR_RECHECK];
