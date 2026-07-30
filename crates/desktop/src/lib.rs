//! Native desktop adapter for the coding-agent product runtime.
//!
//! The crate is intentionally an adapter: product facts and mutable session
//! ownership remain in `coding-agent`.

use std::path::{Path, PathBuf};

extern crate self as desktop;

mod actions;
mod app;
mod application;
mod assets;
mod conversation;
mod file_review;
mod preferences;
mod projection;
mod runtime;
mod shell;

#[cfg(test)]
mod allocation_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::{
        Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    };

    struct CountingAllocator;

    static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
    static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
    static PERFORMANCE_PROBE_LOCK: Mutex<()> = Mutex::new(());

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

    pub(crate) fn serial_guard() -> MutexGuard<'static, ()> {
        PERFORMANCE_PROBE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) use crate::resident_memory::resident_bytes;
}

mod resident_memory {
    #[cfg(target_os = "linux")]
    pub(crate) fn resident_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        parse_linux_resident_bytes(&status)
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn resident_bytes() -> Option<u64> {
        let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info>::uninit();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        #[allow(deprecated)]
        // SAFETY: reading the process-global send right does not transfer or
        // mutate ownership; task_info only borrows it for this syscall.
        let task = unsafe { libc::mach_task_self_ };
        // SAFETY: `info` has the exact layout and count required by
        // MACH_TASK_BASIC_INFO, and remains alive for the duration of the call.
        let result = unsafe {
            libc::task_info(
                task,
                libc::MACH_TASK_BASIC_INFO,
                info.as_mut_ptr().cast::<libc::integer_t>(),
                &mut count,
            )
        };
        if result != libc::KERN_SUCCESS || count < libc::MACH_TASK_BASIC_INFO_COUNT {
            return None;
        }
        // SAFETY: a successful task_info call initialized the complete struct.
        let info = unsafe { info.assume_init() };
        Some(info.resident_size)
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn resident_bytes() -> Option<u64> {
        use windows_sys::Win32::System::ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?,
            ..PROCESS_MEMORY_COUNTERS::default()
        };
        // SAFETY: GetCurrentProcess returns a process-owned pseudo-handle, and
        // counters points to a writable structure whose byte size is supplied.
        let succeeded =
            unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
        if succeeded == 0 {
            return None;
        }
        u64::try_from(counters.WorkingSetSize).ok()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    pub(crate) fn resident_bytes() -> Option<u64> {
        None
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn parse_linux_resident_bytes(status: &str) -> Option<u64> {
        let kibibytes = status.lines().find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })?;
        kibibytes.checked_mul(1024)
    }
}

/// Supported startup inputs for the native desktop application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApplicationOptions {
    cwd: PathBuf,
    projectless: bool,
    session_id: Option<String>,
}

impl DesktopApplicationOptions {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            projectless: false,
            session_id: None,
        }
    }

    /// Start on a user-global scratch workspace instead of treating the
    /// process working directory as an explicitly selected project.
    pub fn projectless() -> Self {
        Self {
            cwd: PathBuf::from("."),
            projectless: true,
            session_id: None,
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn is_projectless(&self) -> bool {
        self.projectless
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
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn resident_memory_probe_reports_the_current_process() {
        assert!(
            super::resident_memory::resident_bytes().is_some_and(|bytes| bytes > 0),
            "supported desktop platforms must report a nonzero resident set"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_resident_memory_parser_requires_vmrss_and_converts_kibibytes() {
        assert_eq!(
            super::resident_memory::parse_linux_resident_bytes(
                "Name:\tdesktop\nVmSize:\t9000 kB\nVmRSS:\t1234 kB\n"
            ),
            Some(1_263_616)
        );
        assert_eq!(
            super::resident_memory::parse_linux_resident_bytes("VmSize:\t9000 kB\n"),
            None
        );
    }

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
        assert!(!options.is_projectless());
        assert_eq!(options.session_id(), Some("session-from-options"));
    }

    #[test]
    fn projectless_options_do_not_claim_the_process_directory_as_a_project() {
        let options = super::DesktopApplicationOptions::projectless();
        assert!(options.is_projectless());
        assert_eq!(options.session_id(), None);
    }
}
