//! Product-owned resource budgets.
//!
//! Lower layers retain their own protocol and agent-runtime limits. These
//! limits cover product inputs and projections before they cross those
//! boundaries.

pub(crate) const MAX_PROMPT_INPUT_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_CLIENT_DRAFT_ID_BYTES: usize = 256;
pub(crate) const MAX_CLIENT_DRAFT_TEXT_BYTES: usize = MAX_PROMPT_INPUT_BYTES;
pub(crate) const MAX_AT_FILE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_AT_FILE_REFERENCES: usize = 64;
pub(crate) const MAX_INPUT_IMAGES: usize = 16;
pub(crate) const MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_IMAGE_DECODE_DIMENSION: u32 = 16_384;
pub(crate) const MAX_IMAGE_DECODE_ALLOC_BYTES: u64 = 128 * 1024 * 1024;

pub(crate) const MAX_CONTEXT_FILE_BYTES: usize = 256 * 1024;
pub(crate) const MAX_CONTEXT_TOTAL_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_PROFILE_FILE_BYTES: usize = 512 * 1024;
pub(crate) const MAX_PROFILE_FILES_PER_DIRECTORY: usize = 256;
pub(crate) const MAX_THEME_FILE_BYTES: usize = 512 * 1024;
pub(crate) const MAX_THEME_FILES: usize = 256;
pub(crate) const MAX_PRODUCT_RESOURCE_DIAGNOSTICS: usize = 1024;
pub(crate) const MAX_PUBLIC_ERROR_CONTEXT_BYTES: usize = 256;
pub(crate) const MAX_PUBLIC_DIAGNOSTIC_CODE_BYTES: usize = 96;
pub(crate) const MAX_PUBLIC_DIAGNOSTIC_SUMMARY_BYTES: usize = 512;

pub(crate) const MAX_FILE_REVIEW_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_FILE_REVIEW_CONTENT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_FILE_REVIEW_DIFF_BYTES: usize = 256 * 1024;

pub(crate) const MAX_EDIT_RESULT_BYTES: usize = 8 * 1024 * 1024;
