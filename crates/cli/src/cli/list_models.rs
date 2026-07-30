use crate::error::CliError;
use coding_agent::api::embedding::{CodingAgentModelCatalogEntry, model_catalog};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct ModelRow {
    provider: String,
    model: String,
    name: String,
    context: String,
    max_out: String,
    thinking: bool,
    images: bool,
}

pub fn list_models_output(
    search: Option<&str>,
    provider: Option<&str>,
    json: bool,
) -> Result<String, CliError> {
    let mut models = model_catalog()
        .into_iter()
        .filter(|model| provider.is_none_or(|provider| model.provider == provider))
        .collect::<Vec<_>>();

    if let Some(search) = search {
        let indices = model_filter_indices(&models, search);
        models = indices
            .into_iter()
            .map(|index| models[index].clone())
            .collect();
    }

    let rows = models.iter().map(model_row).collect::<Vec<_>>();
    if json {
        let json = serde_json::to_string_pretty(&rows).map_err(|error| {
            CliError::InvalidInput(format!("failed to serialize model list: {error}"))
        })?;
        return Ok(format!("{json}\n"));
    }

    if rows.is_empty() {
        return Ok(match search {
            Some(search) => format!("No models matching \"{search}\"\n"),
            None => "No models found\n".to_string(),
        });
    }

    Ok(format_table(&rows))
}

fn model_filter_indices(models: &[CodingAgentModelCatalogEntry], query: &str) -> Vec<usize> {
    let tokens = query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return (0..models.len()).collect();
    }

    models
        .iter()
        .enumerate()
        .filter_map(|(index, model)| {
            let searchable = model_search_text(model).to_lowercase();
            tokens
                .iter()
                .all(|token| fuzzy_subsequence(token, &searchable))
                .then_some(index)
        })
        .collect()
}

fn fuzzy_subsequence(query: &str, text: &str) -> bool {
    let mut query = query.chars();
    let Some(mut expected) = query.next() else {
        return true;
    };
    for ch in text.chars() {
        if ch.eq_ignore_ascii_case(&expected) {
            let Some(next) = query.next() else {
                return true;
            };
            expected = next;
        }
    }
    false
}

fn model_search_text(model: &CodingAgentModelCatalogEntry) -> String {
    format!(
        "{} {} {} {}",
        model.provider, model.id, model.name, model.api
    )
}

fn model_row(model: &CodingAgentModelCatalogEntry) -> ModelRow {
    ModelRow {
        provider: model.provider.clone(),
        model: model.id.clone(),
        name: model.name.clone(),
        context: format_token_count(model.context_window),
        max_out: format_token_count(model.max_output_tokens),
        thinking: model.reasoning,
        images: model.supports_images,
    }
}

fn format_token_count(count: u32) -> String {
    if count >= 1_000_000 {
        let millions = count as f64 / 1_000_000.0;
        if count.is_multiple_of(1_000_000) {
            format!("{}M", count / 1_000_000)
        } else {
            format!("{millions:.1}M")
        }
    } else if count >= 1_000 {
        let thousands = count as f64 / 1_000.0;
        if count.is_multiple_of(1_000) {
            format!("{}K", count / 1_000)
        } else {
            format!("{thousands:.1}K")
        }
    } else {
        count.to_string()
    }
}

fn format_table(rows: &[ModelRow]) -> String {
    let headers = [
        "provider", "model", "context", "max-out", "thinking", "images",
    ];
    let provider_width = rows
        .iter()
        .map(|row| row.provider.len())
        .max()
        .unwrap_or(0)
        .max(headers[0].len());
    let model_width = rows
        .iter()
        .map(|row| row.model.len())
        .max()
        .unwrap_or(0)
        .max(headers[1].len());
    let context_width = rows
        .iter()
        .map(|row| row.context.len())
        .max()
        .unwrap_or(0)
        .max(headers[2].len());
    let max_out_width = rows
        .iter()
        .map(|row| row.max_out.len())
        .max()
        .unwrap_or(0)
        .max(headers[3].len());
    let thinking_width = headers[4].len();
    let images_width = headers[5].len();

    let mut out = String::new();
    out.push_str(&format!(
        "{:<provider_width$}  {:<model_width$}  {:<context_width$}  {:<max_out_width$}  {:<thinking_width$}  {:<images_width$}\n",
        headers[0], headers[1], headers[2], headers[3], headers[4], headers[5]
    ));
    for row in rows {
        out.push_str(&format!(
            "{:<provider_width$}  {:<model_width$}  {:<context_width$}  {:<max_out_width$}  {:<thinking_width$}  {:<images_width$}\n",
            row.provider,
            row.model,
            row.context,
            row.max_out,
            yes_no(row.thinking),
            yes_no(row.images),
        ));
    }
    out
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
