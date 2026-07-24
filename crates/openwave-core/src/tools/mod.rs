//! Built-in tools owned by `openwave-core`.
//!
//! The current tools operate only on a chat's private scratch directory. Their
//! model-facing definitions are centralized in [`definitions`], while
//! [`private_scratch`] owns the capability-confined filesystem primitives.

mod arguments;
mod create_deliverable;
mod definitions;
mod list_dir;
mod private_scratch;
mod read_file;
mod write_file;

pub use create_deliverable::CreateDeliverable;
pub use list_dir::ListDir;
pub use read_file::ReadFile;
pub use write_file::WriteFile;

#[cfg(test)]
mod tests;
