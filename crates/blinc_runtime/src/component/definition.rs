//! Definitions: what a registered component looks like.

use std::sync::Arc;

/// One of the primitive types a component's prop can declare.
///
/// Matches what the DSL's `state_type` rule accepts today and
/// what [`crate::signal`] supports for signal storage. New
/// types land here, in `signal`, and as a `$Blinc$<name>`
/// extern together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropType {
    /// `i32` — the DSL's default integer.
    I32,
    /// `f64` — used in guard expressions and progress values.
    F64,
    /// `string` — DSL surface; routes through Zyntax's `str`
    /// primitive at the typed-AST level. String-typed props
    /// don't fully flow at runtime yet (see the broader Blinc
    /// string-return ABI discussion); reserving the variant
    /// so the registry doesn't need to break when they do.
    Str,
}

impl PropType {
    /// User-facing name of the primitive (matches the DSL
    /// `state_type` keyword). Useful for diagnostic messages.
    pub fn as_str(&self) -> &'static str {
        match self {
            PropType::I32 => "i32",
            PropType::F64 => "f64",
            PropType::Str => "string",
        }
    }
}

/// One declared prop on a component.
#[derive(Debug, Clone)]
pub struct PropDef {
    /// The prop's identifier (the binding name visible inside
    /// the component's view body — `initial`, `step`, etc.).
    pub name: Arc<str>,
    pub ty: PropType,
}

/// All registry-level information about a single component.
///
/// `view_symbol` is the JIT-linker-visible name (the suffix
/// Zyntax's inherent-impl mangling produces). It's stored
/// pre-computed so callers don't have to remember the
/// `<Name>$view` convention.
#[derive(Debug, Clone)]
pub struct ComponentDefinition {
    /// User-visible component name (the DSL identifier, e.g.
    /// `"Counter"`).
    pub name: Arc<str>,
    /// The JIT-linker-visible view symbol. Today always
    /// `<Self::name>$view` — pre-computed for ergonomics + so
    /// future mangling changes localise to the publisher.
    pub view_symbol: Arc<str>,
    /// Props in source declaration order. Order matters because
    /// the call-site lowering passes positional args in this
    /// order (see `lower_component_calls` /
    /// `bind_component_props` in `blinc_dsl_core`).
    pub props: Vec<PropDef>,
}

impl ComponentDefinition {
    /// Number of props the component declares. Convenience for
    /// callers that just want an arity check.
    pub fn prop_count(&self) -> usize {
        self.props.len()
    }

    /// Look up a prop by name. Returns `None` if no prop has
    /// that name. Linear scan — prop lists are small in
    /// practice (handful of entries), so a HashMap would be
    /// overkill.
    pub fn prop(&self, name: &str) -> Option<&PropDef> {
        self.props.iter().find(|p| p.name.as_ref() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arc(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    #[test]
    fn prop_lookup_by_name() {
        let def = ComponentDefinition {
            name: arc("Counter"),
            view_symbol: arc("Counter$view"),
            props: vec![
                PropDef {
                    name: arc("initial"),
                    ty: PropType::I32,
                },
                PropDef {
                    name: arc("step"),
                    ty: PropType::I32,
                },
            ],
        };
        assert_eq!(def.prop_count(), 2);
        assert_eq!(def.prop("initial").map(|p| p.ty), Some(PropType::I32));
        assert_eq!(def.prop("step").map(|p| p.ty), Some(PropType::I32));
        assert!(def.prop("missing").is_none());
    }

    #[test]
    fn prop_type_as_str_matches_dsl_surface() {
        assert_eq!(PropType::I32.as_str(), "i32");
        assert_eq!(PropType::F64.as_str(), "f64");
        assert_eq!(PropType::Str.as_str(), "string");
    }
}
