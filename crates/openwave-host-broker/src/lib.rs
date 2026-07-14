//! Capability and path-policy core for access to user-approved host resources.
//!
//! This crate defines the policy values and root-opening primitive that the
//! broker will enforce; it does not yet expose agent-facing filesystem
//! operations. A later operation layer will own authorization and the effect so
//! callers cannot separate a successful check from the filesystem access it
//! covered. No desktop runtime or transport is referenced here.

pub mod capability;
pub mod id;
pub mod path_policy;
pub mod relative_path;

pub use capability::{
    Capability, ConsentMethod, ConsentRecord, Grant, GrantError, RootAttachment, Scope,
};
pub use id::{
    ExecutionContext, GrantId, GrantSubject, IdError, OperationId, ParseIdError, RootId,
    SubjectKind,
};
pub use path_policy::{RootPolicy, RootPolicyError, ValidatedRoot};
pub use relative_path::{RelativePath, RelativePathError};
