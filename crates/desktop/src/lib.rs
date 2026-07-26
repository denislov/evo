//! Native desktop adapter for the coding-agent product runtime.
//!
//! The crate is intentionally an adapter: product facts and mutable session
//! ownership remain in `coding-agent`.

use std::path::{Path, PathBuf};

extern crate self as desktop;

mod actions;
mod app;
mod command_ledger;
mod conversation;
mod file_review;
mod preferences;
mod projection;
mod runtime;
mod shell;

#[cfg(test)]
mod allocation_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct CountingAllocator;

    static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
    static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the system allocator receives the caller-provided layout unchanged.
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() {
                record_allocation(layout.size());
            }
            pointer
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the system allocator receives the caller-provided layout unchanged.
            let pointer = unsafe { System.alloc_zeroed(layout) };
            if !pointer.is_null() {
                record_allocation(layout.size());
            }
            pointer
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            // SAFETY: the pointer and layout originate from this system allocator.
            unsafe { System.dealloc(pointer, layout) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: the pointer and layout originate from this system allocator and the new
            // size is forwarded unchanged.
            let reallocated = unsafe { System.realloc(pointer, layout, new_size) };
            if !reallocated.is_null() {
                record_allocation(new_size);
            }
            reallocated
        }
    }

    fn record_allocation(bytes: usize) {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    #[derive(Debug, Clone, Copy)]
    pub(crate) struct AllocationSnapshot {
        count: u64,
        bytes: u64,
    }

    impl AllocationSnapshot {
        pub(crate) fn delta_since(self, before: Self) -> Self {
            Self {
                count: self.count.saturating_sub(before.count),
                bytes: self.bytes.saturating_sub(before.bytes),
            }
        }

        pub(crate) const fn count(self) -> u64 {
            self.count
        }

        pub(crate) const fn bytes(self) -> u64 {
            self.bytes
        }
    }

    pub(crate) fn snapshot() -> AllocationSnapshot {
        AllocationSnapshot {
            count: ALLOCATION_COUNT.load(Ordering::Relaxed),
            bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        }
    }
}

/// Supported startup inputs for the native desktop application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApplicationOptions {
    cwd: PathBuf,
    session_id: Option<String>,
}

impl DesktopApplicationOptions {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            session_id: None,
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
}

/// Run the native desktop application until its platform event loop exits.
pub fn run(options: DesktopApplicationOptions) {
    app::run(options);
}

#[cfg(test)]
mod tests {
    #[test]
    fn categorized_product_runtime_facade_is_importable() {
        let options = coding_agent::api::runtime::CodingAgentSessionOptions::new();
        assert!(options.cwd().is_none());
        let embedding = coding_agent::api::embedding::CodingAgentEmbeddingOptions::new(".");
        assert_eq!(embedding.cwd(), std::path::Path::new("."));
    }

    #[test]
    fn application_options_preserve_the_explicit_working_directory() {
        let options = super::DesktopApplicationOptions::new("project")
            .with_session_id("session-from-options");
        assert_eq!(options.cwd(), std::path::Path::new("project"));
        assert_eq!(options.session_id(), Some("session-from-options"));
    }
}
