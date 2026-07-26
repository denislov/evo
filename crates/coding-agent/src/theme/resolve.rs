//! Variable resolution — ported from `resolveVarRefs` in `theme.ts`.
//!
//! A `ColorValue::Var` is replaced by its `vars` entry, recursively, until a
//! concrete color (hex / 256 / default) is reached. Circular references and
//! dangling names are reported as [`ResolveError`].

use std::collections::{HashMap, HashSet};

use super::ColorValue;

/// A color value with all variable references resolved away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedColor {
    Default,
    Hex(u8, u8, u8),
    Ansi256(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// A `Var` name not present in `vars`.
    UnknownVar(String),
    /// A `Var` that (transitively) refers back to itself.
    Circular(String),
}

/// Resolve `value` against `vars`, chasing variable references to a concrete
/// color. Mirrors `resolveVarRefs` (including the `visited`-set cycle guard).
pub fn resolve(
    value: &ColorValue,
    vars: &HashMap<String, ColorValue>,
) -> Result<ResolvedColor, ResolveError> {
    resolve_inner(value, vars, &mut HashSet::new())
}

fn resolve_inner(
    value: &ColorValue,
    vars: &HashMap<String, ColorValue>,
    visited: &mut HashSet<String>,
) -> Result<ResolvedColor, ResolveError> {
    match value {
        ColorValue::Default => Ok(ResolvedColor::Default),
        ColorValue::Hex(r, g, b) => Ok(ResolvedColor::Hex(*r, *g, *b)),
        ColorValue::Ansi256(n) => Ok(ResolvedColor::Ansi256(*n)),
        ColorValue::Var(name) => {
            if visited.contains(name) {
                return Err(ResolveError::Circular(name.clone()));
            }
            let referenced = vars
                .get(name)
                .ok_or_else(|| ResolveError::UnknownVar(name.clone()))?;
            visited.insert(name.clone());
            resolve_inner(referenced, vars, visited)
        }
    }
}
