//! Byte transfers: generic upload and download drivers, their sinks and sources, and
//! same-agent copy and move.

mod bounded_tar;
pub(crate) mod copy;
pub(crate) mod download;
pub(crate) mod edit;
pub(crate) mod r#move;
pub(crate) mod raw_upload;
pub(crate) mod stream;
pub(crate) mod tar_upload;
pub(crate) mod upload;
