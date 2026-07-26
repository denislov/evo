//! Generic image component behavior.

use base64::Engine;
use tui::api::component::{Component, Image};
use tui::api::render::visible_width;
use tui::api::terminal::{CellDimensions, ImageDimensions, ImageProtocol, TerminalCapabilities};

fn png_base64() -> String {
    let mut png = vec![0_u8; 24];
    png[0..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    png[16..20].copy_from_slice(&18_u32.to_be_bytes());
    png[20..24].copy_from_slice(&18_u32.to_be_bytes());
    base64::engine::general_purpose::STANDARD.encode(png)
}

#[test]
fn image_component_renders_fallback_when_protocol_is_unavailable() {
    let mut image = Image::new(png_base64(), "image/png")
        .filename("diagram.png")
        .capabilities(TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: false,
        });

    let lines = image.render(40);

    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("diagram.png"), "{lines:?}");
    assert!(lines[0].contains("image/png"), "{lines:?}");
    assert!(lines[0].contains("18x18"), "{lines:?}");
    assert!(visible_width(&lines[0]) <= 40);
}

#[test]
fn image_component_renders_kitty_sequence_when_supported() {
    let mut image = Image::new(png_base64(), "image/png")
        .dimensions(ImageDimensions {
            width_px: 18,
            height_px: 18,
        })
        .cell_dimensions(CellDimensions {
            width_px: 9,
            height_px: 18,
        })
        .max_width_cells(10)
        .image_id(99)
        .capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });

    let lines = image.render(80);

    assert_eq!(lines.len(), 5);
    assert!(lines[0].starts_with("\x1b_G"));
    assert!(lines[0].contains("c=10,r=5,i=99"), "{lines:?}");
    assert!(lines[1..].iter().all(String::is_empty));
}
