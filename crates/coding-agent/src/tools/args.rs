use serde_json::Value;

/// Parse a non-negative integer tool argument and clamp it to the advertised
/// runtime maximum. JSON numbers with another representation (negative or
/// floating point) are rejected instead of being silently coerced.
pub(crate) fn bounded_arg(
    args: &Value,
    key: &str,
    default: usize,
    max: usize,
) -> Result<usize, String> {
    let Some(value) = args.get(key) else {
        return Ok(default.min(max));
    };
    let raw = value.as_u64().ok_or_else(|| {
        format!("argument '{key}' must be a non-negative integer no greater than {max}")
    })?;
    Ok(usize::try_from(raw).unwrap_or(usize::MAX).min(max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_argument_uses_bounded_default() {
        assert_eq!(bounded_arg(&json!({}), "limit", 100, 1_000), Ok(100));
        assert_eq!(bounded_arg(&json!({}), "limit", 2_000, 1_000), Ok(1_000));
    }

    #[test]
    fn huge_unsigned_arguments_are_capped_without_panicking() {
        let args = json!({"context": u64::MAX, "limit": u64::MAX});
        assert_eq!(bounded_arg(&args, "context", 0, 20), Ok(20));
        assert_eq!(bounded_arg(&args, "limit", 100, 1_000), Ok(1_000));
    }

    #[test]
    fn invalid_numeric_representations_are_rejected() {
        for value in [json!(-1), json!(1.5), json!("10")] {
            let error = bounded_arg(&json!({"limit": value}), "limit", 100, 1_000)
                .expect_err("invalid representation must fail");
            assert!(error.contains("non-negative integer"));
        }
    }
}
