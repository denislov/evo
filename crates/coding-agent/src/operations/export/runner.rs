use std::path::{Path, PathBuf};

use super::{
    CodingAgentSessionExport, export_from_replay, render_export_html, write_rendered_export_html,
};
use crate::runtime::error::CodingSessionError;
use crate::runtime::facade::context::CodingAgentSessionSummary;
use crate::session::replay::SessionReplay;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportOptions {
    target: ExportTarget,
}

impl ExportOptions {
    pub(crate) fn view() -> Self {
        Self {
            target: ExportTarget::View,
        }
    }

    pub(crate) fn html(path: impl Into<PathBuf>) -> Self {
        Self {
            target: ExportTarget::Html(path.into()),
        }
    }

    fn output_path(&self) -> Option<&Path> {
        match &self.target {
            ExportTarget::View => None,
            ExportTarget::Html(path) => Some(path.as_path()),
        }
    }

    pub(crate) fn writes_html(&self) -> bool {
        matches!(self.target, ExportTarget::Html(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExportTarget {
    View,
    Html(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportOutcome {
    pub(crate) export: CodingAgentSessionExport,
    pub(crate) path: Option<PathBuf>,
}

pub(crate) struct ExportContext {
    options: ExportOptions,
    summary: CodingAgentSessionSummary,
    replay: SessionReplay,
    export: Option<CodingAgentSessionExport>,
    rendered_html: Option<String>,
    written_path: Option<PathBuf>,
}

impl ExportContext {
    pub(crate) fn new(
        options: ExportOptions,
        summary: CodingAgentSessionSummary,
        replay: SessionReplay,
    ) -> Self {
        Self {
            options,
            summary,
            replay,
            export: None,
            rendered_html: None,
            written_path: None,
        }
    }

    pub(crate) fn finish_success(&self) -> Result<ExportOutcome, CodingSessionError> {
        if self.options.writes_html() && self.written_path.is_none() {
            return Err(CodingSessionError::Session {
                message: "export cannot finish without a written html path".into(),
            });
        }
        Ok(ExportOutcome {
            export: self
                .export
                .clone()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "export cannot finish without an export view".into(),
                })?,
            path: self.written_path.clone(),
        })
    }

    fn start_export(&mut self) -> Result<(), CodingSessionError> {
        if self.summary.session_id.is_empty() {
            return Err(CodingSessionError::Session {
                message: "export cannot start without a session id".into(),
            });
        }
        Ok(())
    }

    fn load_session_replay(&mut self) -> Result<(), CodingSessionError> {
        if self.replay.session_id.is_empty() {
            return Err(CodingSessionError::Session {
                message: "export cannot load an unnamed session replay".into(),
            });
        }
        if self.replay.session_id != self.summary.session_id {
            return Err(CodingSessionError::Session {
                message: format!(
                    "export replay session id '{}' does not match summary session id '{}'",
                    self.replay.session_id, self.summary.session_id
                ),
            });
        }
        Ok(())
    }

    fn select_export_view(&mut self) -> Result<(), CodingSessionError> {
        if self.export.is_none() {
            self.export = Some(export_from_replay(
                self.summary.clone(),
                self.replay.clone(),
            ));
        }
        Ok(())
    }

    fn render_export(&mut self) -> Result<(), CodingSessionError> {
        if !self.options.writes_html() {
            return Ok(());
        }
        let export = self
            .export
            .as_ref()
            .ok_or_else(|| CodingSessionError::Session {
                message: "export cannot render without an export view".into(),
            })?;
        self.rendered_html = Some(render_export_html(export));
        Ok(())
    }

    fn write_export(&mut self) -> Result<(), CodingSessionError> {
        let Some(path) = self.options.output_path().map(Path::to_path_buf) else {
            return Ok(());
        };
        let html = self
            .rendered_html
            .clone()
            .ok_or_else(|| CodingSessionError::Session {
                message: "export cannot write without rendered html".into(),
            })?;
        self.written_path = Some(write_rendered_export_html(&html, &path)?);
        Ok(())
    }
}

pub(crate) struct ExportRunner;

impl ExportRunner {
    pub(crate) fn new() -> Result<Self, CodingSessionError> {
        Ok(Self)
    }

    pub(crate) fn run_typed(
        &self,
        ctx: &mut ExportContext,
    ) -> Result<ExportOutcome, CodingSessionError> {
        ctx.start_export()?;
        ctx.load_session_replay()?;
        ctx.select_export_view()?;
        ctx.render_export()?;
        ctx.write_export()?;
        ctx.finish_success()
    }
}
