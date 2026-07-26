//! One connection-local pressure contract for the RPC adapter.
//!
//! Keep protocol framing generic in `protocol::jsonl`; every limit below owns
//! retained or executable state created after a frame reaches the RPC layer.

use crate::protocol::jsonl::DEFAULT_MAX_JSONL_FRAME_BYTES;

pub(super) const RPC_JSONL_FRAME_BYTES: usize = DEFAULT_MAX_JSONL_FRAME_BYTES;
pub(super) const MAX_RPC_JSON_DEPTH: usize = 64;
pub(super) const MAX_RPC_CONTAINER_ITEMS: usize = 4096;
pub(super) const MAX_RPC_ARRAY_ITEMS: usize = 1024;
pub(super) const MAX_RPC_OBJECT_FIELDS: usize = 256;
pub(super) const MAX_RPC_OBJECT_KEY_BYTES: usize = 64;
pub(super) const MAX_RPC_IDENTIFIER_BYTES: usize = 128;
pub(super) const MAX_RPC_AUTHORIZATION_TOKEN_BYTES: usize = 4096;
pub(super) const MAX_RPC_TEXT_BYTES: usize = 1024 * 1024;
pub(super) const MAX_RPC_IMAGES: usize = 16;
pub(super) const MAX_RPC_IMAGE_ENCODED_BYTES: usize = 2 * 1024 * 1024;
pub(super) const MAX_RPC_IMAGE_ENCODED_TOTAL_BYTES: usize = 3 * 1024 * 1024;
pub(super) const MAX_RPC_REPAIR_ATTEMPTS: usize = 16;

pub(super) const RPC_QUEUED_CONTROL_ITEM_LIMIT: usize = 16;
pub(super) const RPC_QUEUED_CONTROL_BYTE_LIMIT: usize = 4 * 1024 * 1024;
pub(super) const RPC_IDEMPOTENCY_RECORD_LIMIT: usize = 64;

// The adapter admits at most four background roots per connection. Product
// admission remains authoritative and may reject a narrower runtime state.
pub(super) const RPC_BACKGROUND_OPERATION_LIMIT: usize = 4;
pub(super) const RPC_EVENT_FLUSH_QUEUE_CAPACITY: usize = RPC_BACKGROUND_OPERATION_LIMIT + 1;

// The adapter retains at most one product replay window and recovers overflow
// through a typed fresh-snapshot lane.
pub(super) const RPC_PRODUCT_EVENT_QUEUE_CAPACITY: usize = 128;
