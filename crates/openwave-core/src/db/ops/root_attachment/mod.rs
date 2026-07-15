mod begin;
mod codec;
mod finish;
mod persistence;
mod projection;

pub(in crate::db) use begin::begin_root_attachment_change;
pub(in crate::db) use finish::finish_root_attachment_change;
pub(in crate::db) use persistence::{
    get_root_attachment_change, list_pending_root_attachment_changes,
};
