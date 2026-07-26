pub fn resolve_base_url_with<S>(
    template: &str,
    mut lookup: impl FnMut(&str) -> Option<S>,
) -> Result<String, String>
where
    S: AsRef<str>,
{
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let (before, after_start) = rest.split_at(start);
        out.push_str(before);
        let Some(end) = after_start.find('}') else {
            return Err("Unclosed Cloudflare baseUrl placeholder".into());
        };
        let name = &after_start[1..end];
        let value = lookup(name).ok_or_else(|| {
            format!(
                "{} is required for Cloudflare baseUrl but is not set.",
                name
            )
        })?;
        out.push_str(value.as_ref());
        rest = &after_start[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}
