use std::collections::BTreeSet;

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;

use crate::contract::ToolDefinitionError;

const MAX_SCHEMA_BYTES: usize = 32_768;
const MAX_SCHEMA_DEPTH: usize = 12;
const MAX_SCHEMA_NODES: usize = 512;
const MAX_SCHEMA_PROPERTIES: usize = 256;
const MAX_PROPERTIES_PER_OBJECT: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 1_024;
const MAX_ENUM_VALUES: usize = 64;
const MAX_ANY_OF_BRANCHES: usize = 4;
const MAX_PATTERN_CHARS: usize = 1_024;

#[derive(Default)]
struct SchemaBudget {
    nodes: usize,
    properties: usize,
}

pub(crate) fn schema_for<T: JsonSchema>() -> Result<serde_json::Value, ToolDefinitionError> {
    let settings = SchemaSettings::draft07().with(|settings| {
        settings.meta_schema = None;
        settings.inline_subschemas = true;
    });
    let schema = settings.into_generator().into_root_schema_for::<T>();
    let mut value = serde_json::to_value(schema)
        .map_err(|error| schema_error(format!("generated schema cannot serialize: {error}")))?;
    remove_generation_metadata(&mut value);
    validate_tool_schema(&value)?;
    Ok(value)
}

pub(crate) fn validate_tool_schema(schema: &serde_json::Value) -> Result<(), ToolDefinitionError> {
    let serialized = serde_json::to_vec(schema)
        .map_err(|error| schema_error(format!("schema cannot serialize: {error}")))?;
    require(
        serialized.len() <= MAX_SCHEMA_BYTES,
        "tool parameters schema exceeds 32768 bytes",
    )?;
    let mut budget = SchemaBudget::default();
    validate_node(schema, 0, true, &mut budget)
}

fn remove_generation_metadata(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for key in ["$schema", "title", "default", "examples", "format"] {
                object.remove(key);
            }
            normalize_nullable_type(object);
            for value in object.values_mut() {
                remove_generation_metadata(value);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                remove_generation_metadata(value);
            }
        }
        _ => {}
    }
}

fn normalize_nullable_type(object: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(types) = object.get("type").and_then(serde_json::Value::as_array) else {
        return;
    };
    let non_null = types
        .iter()
        .filter_map(serde_json::Value::as_str)
        .find(|schema_type| *schema_type != "null")
        .map(str::to_owned);
    let nullable = types.iter().any(|schema_type| schema_type == "null");
    if types.len() != 2 || !nullable || non_null.is_none() {
        return;
    }

    let description = object.remove("description");
    object.remove("type");
    let mut typed = std::mem::take(object);
    typed.insert("type".into(), non_null.unwrap().into());
    if let Some(description) = description {
        object.insert("description".into(), description);
    }
    object.insert(
        "anyOf".into(),
        serde_json::Value::Array(vec![
            serde_json::Value::Object(typed),
            serde_json::json!({"type": "null"}),
        ]),
    );
}

fn validate_node(
    value: &serde_json::Value,
    depth: usize,
    root: bool,
    budget: &mut SchemaBudget,
) -> Result<(), ToolDefinitionError> {
    require(
        depth <= MAX_SCHEMA_DEPTH,
        "tool parameters schema exceeds maximum depth",
    )?;
    budget.nodes = budget
        .nodes
        .checked_add(1)
        .ok_or_else(|| schema_error("tool parameters schema node count overflow"))?;
    require(
        budget.nodes <= MAX_SCHEMA_NODES,
        "tool parameters schema contains too many nodes",
    )?;

    let object = value
        .as_object()
        .ok_or_else(|| schema_error("every tool parameters schema node must be an object"))?;
    const ALLOWED_KEYS: &[&str] = &[
        "type",
        "description",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "enum",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "minLength",
        "maxLength",
        "pattern",
        "minItems",
        "maxItems",
        "anyOf",
    ];
    require(
        object
            .keys()
            .all(|key| ALLOWED_KEYS.contains(&key.as_str())),
        "tool parameters schema contains an unsupported keyword",
    )?;

    if let Some(description) = object.get("description") {
        let description = description
            .as_str()
            .ok_or_else(|| schema_error("tool schema description must be a string"))?;
        require(
            description.chars().count() <= MAX_DESCRIPTION_CHARS,
            "tool schema description exceeds 1024 characters",
        )?;
    }

    if let Some(branches) = object.get("anyOf") {
        require(!root, "tool parameters root must be an object schema")?;
        require(
            object
                .keys()
                .all(|key| matches!(key.as_str(), "anyOf" | "description")),
            "anyOf tool schema cannot mix direct validation keywords",
        )?;
        let branches = branches
            .as_array()
            .ok_or_else(|| schema_error("tool schema anyOf must be an array"))?;
        require(
            (1..=MAX_ANY_OF_BRANCHES).contains(&branches.len()),
            "tool schema anyOf must contain 1-4 branches",
        )?;
        for branch in branches {
            validate_node(branch, depth + 1, false, budget)?;
        }
        return Ok(());
    }

    let schema_type = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| schema_error("every tool parameters schema node requires a string type"))?;
    require(
        matches!(
            schema_type,
            "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
        ),
        "tool parameters schema contains an unsupported type",
    )?;
    require(
        !root || schema_type == "object",
        "tool parameters root type must be object",
    )?;

    validate_object_keywords(object, schema_type, depth, budget)?;
    validate_array_keywords(object, schema_type, depth, budget)?;
    validate_scalar_keywords(object, schema_type)
}

fn validate_object_keywords(
    object: &serde_json::Map<String, serde_json::Value>,
    schema_type: &str,
    depth: usize,
    budget: &mut SchemaBudget,
) -> Result<(), ToolDefinitionError> {
    let properties = object.get("properties");
    let required = object.get("required");
    if schema_type != "object" {
        return require(
            properties.is_none()
                && required.is_none()
                && object.get("additionalProperties").is_none(),
            "non-object tool schema cannot declare object keywords",
        );
    }

    let properties = properties
        .map(|properties| {
            properties
                .as_object()
                .ok_or_else(|| schema_error("tool schema properties must be an object"))
        })
        .transpose()?;
    if let Some(properties) = properties {
        require(
            properties.len() <= MAX_PROPERTIES_PER_OBJECT,
            "tool schema object contains more than 64 properties",
        )?;
        budget.properties = budget
            .properties
            .checked_add(properties.len())
            .ok_or_else(|| schema_error("tool parameters property count overflow"))?;
        require(
            budget.properties <= MAX_SCHEMA_PROPERTIES,
            "tool parameters schema contains more than 256 properties",
        )?;
        for (name, schema) in properties {
            require(
                !name.is_empty() && name.chars().count() <= 64,
                "tool schema property name is empty or exceeds 64 characters",
            )?;
            validate_node(schema, depth + 1, false, budget)?;
        }
    }
    if let Some(required) = required {
        let required = required
            .as_array()
            .ok_or_else(|| schema_error("tool schema required must be an array"))?;
        let mut names = BTreeSet::new();
        for name in required {
            let name = name
                .as_str()
                .ok_or_else(|| schema_error("tool schema required entries must be strings"))?;
            require(
                names.insert(name),
                "tool schema required entries must be unique",
            )?;
            require(
                properties.is_some_and(|properties| properties.contains_key(name)),
                "tool schema required entry is not declared in properties",
            )?;
        }
    }
    if let Some(additional) = object.get("additionalProperties") {
        require(
            additional.as_bool().is_some(),
            "tool schema additionalProperties must be boolean",
        )?;
    }
    Ok(())
}

fn validate_array_keywords(
    object: &serde_json::Map<String, serde_json::Value>,
    schema_type: &str,
    depth: usize,
    budget: &mut SchemaBudget,
) -> Result<(), ToolDefinitionError> {
    if schema_type == "array" {
        let items = object
            .get("items")
            .ok_or_else(|| schema_error("array tool schema requires items"))?;
        validate_node(items, depth + 1, false, budget)?;
    } else {
        require(
            object.get("items").is_none(),
            "non-array tool schema cannot declare items",
        )?;
    }
    require(
        !(object.contains_key("minItems") || object.contains_key("maxItems"))
            || schema_type == "array",
        "only array tool schemas can declare item bounds",
    )?;
    validate_unsigned_range(object, "minItems", "maxItems")
}

fn validate_scalar_keywords(
    object: &serde_json::Map<String, serde_json::Value>,
    schema_type: &str,
) -> Result<(), ToolDefinitionError> {
    if let Some(values) = object.get("enum") {
        let values = values
            .as_array()
            .ok_or_else(|| schema_error("tool schema enum must be an array"))?;
        require(
            !values.is_empty() && values.len() <= MAX_ENUM_VALUES,
            "tool schema enum must contain 1-64 values",
        )?;
        let mut unique = BTreeSet::new();
        for value in values {
            require(
                enum_value_matches_type(value, schema_type),
                "tool schema enum value does not match the declared type",
            )?;
            require(
                unique.insert(value.to_string()),
                "tool schema enum values must be unique",
            )?;
        }
    }
    let numeric_bound = ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"]
        .iter()
        .any(|key| object.contains_key(*key));
    require(
        !numeric_bound || matches!(schema_type, "number" | "integer"),
        "only number or integer tool schemas can declare numeric bounds",
    )?;
    for (minimum, maximum) in [
        ("minimum", "maximum"),
        ("exclusiveMinimum", "exclusiveMaximum"),
    ] {
        validate_numeric_range(object, minimum, maximum)?;
    }
    require(
        !(object.contains_key("minLength")
            || object.contains_key("maxLength")
            || object.contains_key("pattern"))
            || schema_type == "string",
        "only string tool schemas can declare string bounds",
    )?;
    validate_unsigned_range(object, "minLength", "maxLength")?;
    if let Some(pattern) = object.get("pattern") {
        require(
            pattern
                .as_str()
                .is_some_and(|pattern| pattern.chars().count() <= MAX_PATTERN_CHARS),
            "tool schema pattern must be a string of at most 1024 characters",
        )?;
    }
    Ok(())
}

fn enum_value_matches_type(value: &serde_json::Value, schema_type: &str) -> bool {
    match schema_type {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "object" | "array" => false,
        _ => false,
    }
}

fn validate_numeric_range(
    object: &serde_json::Map<String, serde_json::Value>,
    minimum: &str,
    maximum: &str,
) -> Result<(), ToolDefinitionError> {
    let minimum = finite_number(object.get(minimum))?;
    let maximum = finite_number(object.get(maximum))?;
    require(
        minimum
            .zip(maximum)
            .is_none_or(|(minimum, maximum)| minimum <= maximum),
        "tool schema minimum cannot exceed maximum",
    )
}

fn finite_number(value: Option<&serde_json::Value>) -> Result<Option<f64>, ToolDefinitionError> {
    value
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| schema_error("tool schema numeric bound must be finite"))
        })
        .transpose()
}

fn validate_unsigned_range(
    object: &serde_json::Map<String, serde_json::Value>,
    minimum: &str,
    maximum: &str,
) -> Result<(), ToolDefinitionError> {
    let minimum = unsigned_integer(object.get(minimum))?;
    let maximum = unsigned_integer(object.get(maximum))?;
    require(
        minimum
            .zip(maximum)
            .is_none_or(|(minimum, maximum)| minimum <= maximum),
        "tool schema minimum size cannot exceed maximum size",
    )
}

fn unsigned_integer(value: Option<&serde_json::Value>) -> Result<Option<u64>, ToolDefinitionError> {
    value
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| schema_error("tool schema size bound must be an unsigned integer"))
        })
        .transpose()
}

fn require(condition: bool, message: &'static str) -> Result<(), ToolDefinitionError> {
    condition.then_some(()).ok_or_else(|| schema_error(message))
}

fn schema_error(message: impl Into<String>) -> ToolDefinitionError {
    ToolDefinitionError::new("parameters", message)
}
