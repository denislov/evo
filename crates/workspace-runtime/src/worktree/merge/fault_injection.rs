use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;

use super::{MergeError, TransactionPhase};

#[derive(Debug, Clone, PartialEq, Eq)]
enum MergeIoFault {
    ApplyAfterEntries(usize),
    JournalWrite(TransactionPhase),
}

struct MergeIoFaultState {
    faults: VecDeque<MergeIoFault>,
    applied_entries: usize,
}

thread_local! {
    static MERGE_IO_FAULTS: RefCell<MergeIoFaultState> = const {
        RefCell::new(MergeIoFaultState {
            faults: VecDeque::new(),
            applied_entries: 0,
        })
    };
}

pub(super) struct MergeIoFaultGuard;

impl Drop for MergeIoFaultGuard {
    fn drop(&mut self) {
        MERGE_IO_FAULTS.with(|state| {
            let mut state = state.borrow_mut();
            state.faults.clear();
            state.applied_entries = 0;
        });
    }
}

fn inject(fault: MergeIoFault) -> MergeIoFaultGuard {
    MERGE_IO_FAULTS.with(|state| {
        let mut state = state.borrow_mut();
        assert!(
            state.faults.is_empty(),
            "merge I/O fault is already installed"
        );
        state.applied_entries = 0;
        state.faults.push_back(fault);
    });
    MergeIoFaultGuard
}

pub(super) fn inject_apply_enospc_after(entries: usize) -> MergeIoFaultGuard {
    inject(MergeIoFault::ApplyAfterEntries(entries))
}

pub(super) fn inject_journal_enospc(phase: TransactionPhase) -> MergeIoFaultGuard {
    inject(MergeIoFault::JournalWrite(phase))
}

pub(super) fn maybe_fail_apply(path: &Path) -> Result<(), MergeError> {
    let injected = MERGE_IO_FAULTS.with(|state| {
        let mut state = state.borrow_mut();
        if matches!(state.faults.front(), Some(MergeIoFault::ApplyAfterEntries(count)) if *count == state.applied_entries)
        {
            state.faults.pop_front();
            true
        } else {
            state.applied_entries += 1;
            false
        }
    });
    if injected {
        return Err(MergeError::ApplyFailed {
            path: path.to_path_buf(),
            message: "No space left on device (injected ENOSPC)".into(),
        });
    }
    Ok(())
}

pub(super) fn maybe_fail_journal_write(phase: TransactionPhase) -> Result<(), MergeError> {
    let injected = MERGE_IO_FAULTS.with(|state| {
        let mut state = state.borrow_mut();
        if matches!(state.faults.front(), Some(MergeIoFault::JournalWrite(expected)) if *expected == phase)
        {
            state.faults.pop_front();
            true
        } else {
            false
        }
    });
    if injected {
        return Err(MergeError::RecoveryFailed {
            message: "No space left on device while writing merge journal (injected ENOSPC)".into(),
        });
    }
    Ok(())
}
