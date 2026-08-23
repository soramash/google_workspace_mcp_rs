//! Shared native implementations used by Google Workspace tools.

// Attachment serving and the remaining fetch entry points are wired by the
// native HTTP transport in Task #6. Keep the complete shared API available
// without weakening warnings for the service implementations.
#[allow(dead_code)]
pub mod attachments;
pub mod comments;
#[allow(dead_code)]
pub mod safe_fetch;
#[allow(dead_code)]
pub mod url_safety;
