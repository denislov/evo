//! Merge protocol operations (ARC-340).
//!
//! A child worktree in `MergePending` is applied to the parent workspace by an
//! explicit admitted operation, or discarded without touching the parent.
//! Authorization is scoped to the current session's workspace: a worktree
//! whose record does not point at this session's parent workspace root cannot
//! be merged or discarded here.

pub(crate) mod runner;

#[cfg(test)]
#[path = "runner_tests.rs"]
mod runner_tests;
