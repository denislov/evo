mod capability;
mod color;
mod image;
mod lifecycle;

pub use capability::{ImageProtocol, TerminalCapabilities, detect_terminal_capabilities_from_env};
pub use color::{
    RgbColor, TerminalColorScheme, is_color_scheme_report, is_osc11_background_color_response,
    parse_color_scheme_report, parse_osc11_background_color, query_background_color,
    query_color_scheme, set_color_scheme_notifications,
};
pub use image::{
    CellDimensions, ImageCellSize, ImageDimensions, ImageRenderOptions, RenderedImage,
    calculate_image_cell_size, delete_all_kitty_images, delete_kitty_image, encode_iterm2,
    encode_kitty, hyperlink, image_dimensions_from_base64, image_dimensions_from_bytes,
    is_image_line, render_image,
};
pub use lifecycle::{
    NegotiationResult, ProcessTerminal, Terminal, TerminalMode, TerminalSize,
    is_apple_terminal_session, normalize_apple_terminal_input,
};
