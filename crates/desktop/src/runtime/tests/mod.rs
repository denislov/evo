use std::collections::VecDeque;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use coding_agent::api::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationIdentity, ToolAuthorizationPreview,
    ToolAuthorizationRequest, ToolAuthorizationRisk, ToolAuthorizationScope,
};
use coding_agent::api::client::{
    CodingAgentClientBootstrap, CodingAgentClientId, CodingAgentClientProjection,
    CodingAgentClientProjectionApply, CodingAgentFreshSnapshotRecovery,
    CodingAgentReconnectDelivery, CodingAgentRecoveryPending, CodingAgentRecoveryReason,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingContext, CodingAgentEmbeddingOptions, CodingAgentThinkingLevel,
    CodingAgentWorkspaceSelection,
};
use coding_agent::api::error::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicError,
};
use coding_agent::api::event::{
    CodingAgentProductEvent, CodingAgentProductEventDeliveryClass, CodingAgentRecoveryResolution,
};
use coding_agent::api::review::CodingAgentFileReviewRequest;
use coding_agent::api::view::CodingAgentSessionTranscriptItem;
use tokio::sync::{mpsc, watch};
use tokio::task;

use crate::projection::{
    ContextDirtyFlags, DesktopMessageStatus, DesktopProjection, DesktopProjectionApply,
    DesktopProjectionLifecycle, DesktopToolStatus, MAX_AUTHORIZATION_TEXT_BYTES,
    MAX_DESKTOP_MESSAGE_OVERLAYS, ProjectionEvent,
};
use crate::ui::conversation::model::{MAX_TRANSCRIPT_BLOCKS, MAX_TRANSCRIPT_BYTES};

use super::client::build_desktop_runtime;
use super::protocol::*;
use super::worker::dispatch::{dispatch_active_command, dispatch_command};
use super::worker::*;
use super::*;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct ProcessEnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl ProcessEnvGuard {
    fn isolated(evo_dir: &std::path::Path) -> Self {
        const NAMES: &[&str] = &[
            "EVO_DIR",
            "ANTHROPIC_API_KEY",
            "CLAUDE_API_KEY",
            "ANTHROPIC_KEY",
        ];
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = NAMES
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        unsafe {
            std::env::set_var("EVO_DIR", evo_dir);
            for name in &NAMES[1..] {
                std::env::remove_var(name);
            }
        }
        Self { _lock: lock, saved }
    }
}

impl Drop for ProcessEnvGuard {
    fn drop(&mut self) {
        for (name, previous) in self.saved.iter().rev() {
            unsafe {
                match previous {
                    Some(previous) => std::env::set_var(name, previous),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

include!("fixtures.rs");

mod admission;
mod ordering;
mod overflow;
mod reconnect;
mod recovery;
mod shutdown;
