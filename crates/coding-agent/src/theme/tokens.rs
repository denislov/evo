//! The 51 theme color tokens, split into foreground (45) and background (6)
//! roles, mirroring the TS `ThemeColor` / `ThemeBg` literal unions in
//! `theme.ts` and the `required` list in `theme-schema.json`.

/// Background color tokens (6). All others are foreground ([`ThemeColor`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(
    clippy::enum_variant_names,
    reason = "background variants intentionally mirror the external theme token names"
)]
pub enum ThemeBg {
    SelectedBg,
    UserMessageBg,
    CustomMessageBg,
    ToolPendingBg,
    ToolSuccessBg,
    ToolErrorBg,
}

impl ThemeBg {
    /// Map a JSON key to a token, or `None` if unknown.
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "selectedBg" => Self::SelectedBg,
            "userMessageBg" => Self::UserMessageBg,
            "customMessageBg" => Self::CustomMessageBg,
            "toolPendingBg" => Self::ToolPendingBg,
            "toolSuccessBg" => Self::ToolSuccessBg,
            "toolErrorBg" => Self::ToolErrorBg,
            _ => return None,
        })
    }
}

/// Foreground color tokens (45). See `theme-schema.json` `required` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeColor {
    // Core UI (11)
    Accent,
    Border,
    BorderAccent,
    BorderMuted,
    Success,
    Error,
    Warning,
    Muted,
    Dim,
    Text,
    ThinkingText,
    // Content text (5)
    UserMessageText,
    CustomMessageText,
    CustomMessageLabel,
    ToolTitle,
    ToolOutput,
    // Markdown (10)
    MdHeading,
    MdLink,
    MdLinkUrl,
    MdCode,
    MdCodeBlock,
    MdCodeBlockBorder,
    MdQuote,
    MdQuoteBorder,
    MdHr,
    MdListBullet,
    // Tool diffs (3)
    ToolDiffAdded,
    ToolDiffRemoved,
    ToolDiffContext,
    // Syntax highlighting (9)
    SyntaxComment,
    SyntaxKeyword,
    SyntaxFunction,
    SyntaxVariable,
    SyntaxString,
    SyntaxNumber,
    SyntaxType,
    SyntaxOperator,
    SyntaxPunctuation,
    // Thinking level borders (6)
    ThinkingOff,
    ThinkingMinimal,
    ThinkingLow,
    ThinkingMedium,
    ThinkingHigh,
    ThinkingXhigh,
    // Bash mode (1)
    BashMode,
}

impl ThemeColor {
    /// Map a JSON key to a token, or `None` if unknown.
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            // Core UI
            "accent" => Self::Accent,
            "border" => Self::Border,
            "borderAccent" => Self::BorderAccent,
            "borderMuted" => Self::BorderMuted,
            "success" => Self::Success,
            "error" => Self::Error,
            "warning" => Self::Warning,
            "muted" => Self::Muted,
            "dim" => Self::Dim,
            "text" => Self::Text,
            "thinkingText" => Self::ThinkingText,
            // Content text
            "userMessageText" => Self::UserMessageText,
            "customMessageText" => Self::CustomMessageText,
            "customMessageLabel" => Self::CustomMessageLabel,
            "toolTitle" => Self::ToolTitle,
            "toolOutput" => Self::ToolOutput,
            // Markdown
            "mdHeading" => Self::MdHeading,
            "mdLink" => Self::MdLink,
            "mdLinkUrl" => Self::MdLinkUrl,
            "mdCode" => Self::MdCode,
            "mdCodeBlock" => Self::MdCodeBlock,
            "mdCodeBlockBorder" => Self::MdCodeBlockBorder,
            "mdQuote" => Self::MdQuote,
            "mdQuoteBorder" => Self::MdQuoteBorder,
            "mdHr" => Self::MdHr,
            "mdListBullet" => Self::MdListBullet,
            // Tool diffs
            "toolDiffAdded" => Self::ToolDiffAdded,
            "toolDiffRemoved" => Self::ToolDiffRemoved,
            "toolDiffContext" => Self::ToolDiffContext,
            // Syntax
            "syntaxComment" => Self::SyntaxComment,
            "syntaxKeyword" => Self::SyntaxKeyword,
            "syntaxFunction" => Self::SyntaxFunction,
            "syntaxVariable" => Self::SyntaxVariable,
            "syntaxString" => Self::SyntaxString,
            "syntaxNumber" => Self::SyntaxNumber,
            "syntaxType" => Self::SyntaxType,
            "syntaxOperator" => Self::SyntaxOperator,
            "syntaxPunctuation" => Self::SyntaxPunctuation,
            // Thinking borders
            "thinkingOff" => Self::ThinkingOff,
            "thinkingMinimal" => Self::ThinkingMinimal,
            "thinkingLow" => Self::ThinkingLow,
            "thinkingMedium" => Self::ThinkingMedium,
            "thinkingHigh" => Self::ThinkingHigh,
            "thinkingXhigh" => Self::ThinkingXhigh,
            // Bash mode
            "bashMode" => Self::BashMode,
            _ => return None,
        })
    }
}

/// All 51 required theme token keys, in schema order. Used to validate that
/// a theme defines every token (no optional colors).
pub const REQUIRED_TOKEN_KEYS: &[&str] = &[
    // Core UI
    "accent",
    "border",
    "borderAccent",
    "borderMuted",
    "success",
    "error",
    "warning",
    "muted",
    "dim",
    "text",
    "thinkingText",
    // Backgrounds & content
    "selectedBg",
    "userMessageBg",
    "userMessageText",
    "customMessageBg",
    "customMessageText",
    "customMessageLabel",
    "toolPendingBg",
    "toolSuccessBg",
    "toolErrorBg",
    "toolTitle",
    "toolOutput",
    // Markdown
    "mdHeading",
    "mdLink",
    "mdLinkUrl",
    "mdCode",
    "mdCodeBlock",
    "mdCodeBlockBorder",
    "mdQuote",
    "mdQuoteBorder",
    "mdHr",
    "mdListBullet",
    // Tool diffs
    "toolDiffAdded",
    "toolDiffRemoved",
    "toolDiffContext",
    // Syntax
    "syntaxComment",
    "syntaxKeyword",
    "syntaxFunction",
    "syntaxVariable",
    "syntaxString",
    "syntaxNumber",
    "syntaxType",
    "syntaxOperator",
    "syntaxPunctuation",
    // Thinking borders
    "thinkingOff",
    "thinkingMinimal",
    "thinkingLow",
    "thinkingMedium",
    "thinkingHigh",
    "thinkingXhigh",
    // Bash mode
    "bashMode",
];
