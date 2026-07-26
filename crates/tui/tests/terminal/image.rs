//! Terminal image protocol and sizing behavior.

use base64::Engine;
use tui::api::terminal::{
    CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions, TerminalCapabilities,
    calculate_image_cell_size, delete_all_kitty_images, delete_kitty_image,
    detect_terminal_capabilities_from_env, encode_iterm2, encode_kitty,
    image_dimensions_from_base64, is_image_line, render_image,
};

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn detects_terminal_image_and_hyperlink_capabilities() {
    assert_eq!(
        detect_terminal_capabilities_from_env([("KITTY_WINDOW_ID", "1")], || false),
        TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        }
    );
    assert_eq!(
        detect_terminal_capabilities_from_env([("TERM_PROGRAM", "iTerm.app")], || false).images,
        Some(ImageProtocol::ITerm2)
    );
    assert_eq!(
        detect_terminal_capabilities_from_env(
            [("TMUX", "/tmp/tmux"), ("COLORTERM", "truecolor")],
            || true,
        ),
        TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        }
    );
}

#[test]
fn detects_image_escape_sequences_anywhere_in_line() {
    assert!(is_image_line("before \x1b_Ga=T,f=100;abc\x1b\\ after"));
    assert!(is_image_line(
        "before \x1b]1337;File=inline=1:abc\x07 after"
    ));
    assert!(!is_image_line("\x1b[31mred\x1b[0m"));
}

#[test]
fn encodes_kitty_iterm2_and_delete_sequences() {
    let kitty = encode_kitty(
        "abc",
        ImageRenderOptions {
            columns: Some(4),
            rows: Some(2),
            image_id: Some(42),
            move_cursor: false,
            ..Default::default()
        },
    );
    assert!(kitty.starts_with("\x1b_Ga=T,f=100,q=2,C=1,c=4,r=2,i=42;abc"));
    assert_eq!(delete_kitty_image(42), "\x1b_Ga=d,d=I,i=42,q=2\x1b\\");
    assert_eq!(delete_all_kitty_images(), "\x1b_Ga=d,d=A,q=2\x1b\\");

    let iterm = encode_iterm2(
        "abc",
        ImageRenderOptions {
            columns: Some(4),
            rows: Some(2),
            preserve_aspect_ratio: false,
            ..Default::default()
        },
    );
    assert!(
        iterm.starts_with("\x1b]1337;File=inline=1;width=4;height=2;preserveAspectRatio=0:abc")
    );
}

#[test]
fn parses_png_gif_webp_dimensions_from_base64() {
    let mut png = vec![0_u8; 24];
    png[0..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    png[16..20].copy_from_slice(&320_u32.to_be_bytes());
    png[20..24].copy_from_slice(&200_u32.to_be_bytes());
    assert_eq!(
        image_dimensions_from_base64(&b64(&png), "image/png"),
        Some(ImageDimensions {
            width_px: 320,
            height_px: 200,
        })
    );

    let mut gif = b"GIF89a".to_vec();
    gif.extend_from_slice(&16_u16.to_le_bytes());
    gif.extend_from_slice(&9_u16.to_le_bytes());
    assert_eq!(
        image_dimensions_from_base64(&b64(&gif), "image/gif"),
        Some(ImageDimensions {
            width_px: 16,
            height_px: 9,
        })
    );

    let mut webp = vec![0_u8; 30];
    webp[0..4].copy_from_slice(b"RIFF");
    webp[8..12].copy_from_slice(b"WEBP");
    webp[12..16].copy_from_slice(b"VP8X");
    webp[24] = 63;
    webp[27] = 31;
    assert_eq!(
        image_dimensions_from_base64(&b64(&webp), "image/webp"),
        Some(ImageDimensions {
            width_px: 64,
            height_px: 32,
        })
    );
}

#[test]
fn calculates_cell_size_and_renders_for_selected_protocol() {
    let size = calculate_image_cell_size(
        ImageDimensions {
            width_px: 180,
            height_px: 90,
        },
        10,
        Some(4),
        CellDimensions {
            width_px: 9,
            height_px: 18,
        },
    );
    assert_eq!(size.columns, 10);
    assert_eq!(size.rows, 3);

    let rendered = render_image(
        "abc",
        ImageDimensions {
            width_px: 180,
            height_px: 90,
        },
        TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        },
        ImageRenderOptions {
            max_width_cells: Some(10),
            max_height_cells: Some(4),
            image_id: Some(7),
            ..Default::default()
        },
        CellDimensions {
            width_px: 9,
            height_px: 18,
        },
    )
    .expect("kitty render");

    assert_eq!(rendered.rows, 3);
    assert_eq!(rendered.image_id, Some(7));
    assert!(rendered.sequence.contains("c=10,r=3,i=7"));
}
