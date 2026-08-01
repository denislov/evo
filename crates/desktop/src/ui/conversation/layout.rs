//! Responsive conversation layout helpers.

pub const CONVERSATION_WIDTH_BUCKET_PX: u32 = 24;

/// Maximum height used only for an explicitly collapsed secondary-detail preview.
pub const TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT: f32 = 680.0;

pub const fn conversation_width_bucket(panel_width: u32) -> u32 {
    let bucket = panel_width / CONVERSATION_WIDTH_BUCKET_PX;
    if bucket == 0 {
        CONVERSATION_WIDTH_BUCKET_PX
    } else {
        bucket * CONVERSATION_WIDTH_BUCKET_PX
    }
}

#[cfg(test)]
mod tests {
    use super::{CONVERSATION_WIDTH_BUCKET_PX, conversation_width_bucket};

    #[test]
    fn width_bucket_has_a_non_zero_floor_and_rounds_down() {
        assert_eq!(conversation_width_bucket(0), CONVERSATION_WIDTH_BUCKET_PX);
        assert_eq!(conversation_width_bucket(23), CONVERSATION_WIDTH_BUCKET_PX);
        assert_eq!(conversation_width_bucket(24), 24);
        assert_eq!(conversation_width_bucket(47), 24);
        assert_eq!(conversation_width_bucket(48), 48);
    }
}
