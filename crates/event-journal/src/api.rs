pub mod error {
    pub use crate::error::{JournalError, JournalErrorKind};
}

pub mod frame {
    pub use crate::frame::{MAX_JOURNAL_PAYLOAD_BYTES, MAX_JOURNAL_RECORD_BYTES};
    pub use crate::frame::{decode_json_record, decode_json_value, encode_json_record};
}

pub mod read {
    pub use crate::read::{JournalReadBudget, JournalTailCursor, JournalTailPage, read_tail};
    pub use crate::read::{decode_utf8_line, read_first_line, visit_lines};
}

pub mod storage {
    pub use crate::storage::{AppendFault, JournalPaths, JournalStore, JournalWriteLease};
}
