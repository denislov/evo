use tui::api::terminal::{Terminal, TerminalSize};
use tui::api::testing::VirtualTerminal;

use crate::LoadedScenario;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalReplay {
    pub size: TerminalSize,
    pub output: String,
    pub checkpoints: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TerminalReplayError {
    #[error("terminal write failed: {0}")]
    Write(String),
    #[error("scenario {scenario} terminal output did not contain {expected:?}")]
    Missing { scenario: String, expected: String },
}

pub fn replay_terminal(scenario: &LoadedScenario) -> Result<TerminalReplay, TerminalReplayError> {
    let mut terminal = VirtualTerminal::new(100, 30);
    let mut checkpoints = Vec::new();
    for frame in &scenario.scenario.terminal {
        terminal
            .write(&frame.write)
            .map_err(|error| TerminalReplayError::Write(error.to_string()))?;
        let output = terminal.written_output();
        if !output.contains(&frame.expect) {
            return Err(TerminalReplayError::Missing {
                scenario: scenario.scenario.name.clone(),
                expected: frame.expect.clone(),
            });
        }
        checkpoints.push(frame.expect.clone());
    }
    Ok(TerminalReplay {
        size: terminal.size(),
        output: terminal.written_output(),
        checkpoints,
    })
}
