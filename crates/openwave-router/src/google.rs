//! Shared validation for Google Cloud resource paths.
//!
//! These checks sit below both Vertex protocol families. Keeping them apart
//! from OAuth parsing means the Anthropic adapter does not need to depend on
//! service-account key machinery merely to build a safe resource URL.

/// Validate a Google project/location path segment before interpolating it
/// into a Vertex URL.
pub fn valid_resource_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

/// Validate a Vertex location without pinning a list that goes stale whenever
/// Google adds a region.
///
/// Multi-region aliases such as `us` and `eu` are intentionally not accepted:
/// the adapters currently derive only the documented global or regional host
/// shapes, and must not imply broader endpoint support.
pub fn valid_vertex_location(value: &str) -> bool {
    value == "global"
        || (valid_resource_segment(value)
            && value.contains('-')
            && value.ends_with(|character: char| character.is_ascii_digit()))
}
