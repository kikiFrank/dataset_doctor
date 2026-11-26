//! Dataset Doctor process protocol.

pub const PROTOCOL_MAJOR: u32 = 1;

pub mod v1 {
    tonic::include_proto!("datasetdoctor.v1");
}
