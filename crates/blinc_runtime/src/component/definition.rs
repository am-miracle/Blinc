//! Definitions: what a registered component looks like.

use std::sync::Arc;

pub use zyntax_typed_ast::type_registry::Type;

/// Re-exported convenience alias for `Type`. Keeps the old
/// `PropType` name available as a deprecated path so existing
/// embedders see a deprecation message rather than a hard
/// compile error during the migration window.
///
/// New code should reach for `Type` directly — it's the full
/// Zyntax type representation (primitives, named structs/enums,
/// tuples, arrays, optionals, generics, ...). The substrate
/// holds whatever shape the DSL declared without needing a
/// parallel enum.
#[deprecated(
    since = "0.5.2",
    note = "use `blinc_runtime::component::Type` (re-exported from \
            `zyntax_typed_ast::type_registry::Type`) directly. Tracks the \
            full Zyntax type system rather than the substrate-local \
            primitive subset."
)]
pub type PropType = Type;

/// One declared prop on a component.
///
/// `ty` carries the full Zyntax type representation —
/// primitives, struct references, enums, tuples, arrays,
/// optionals, etc. The substrate doesn't normalise or restrict
/// what types it stores; consumers that only handle primitives
/// pattern-match on `Type::Primitive(...)` and fall through
/// for anything more complex.
///
/// `reactive_inner` flags `Reactive<T>` props (see
/// [`crate::reactive_value::Reactive`]). When `Some(inner_ty)`,
/// the lowering pass emits TWO args at the call site (`tag: i32`,
/// `payload: i64`) — a wire-format encoded literal, signal id, or
/// derived id — rather than a single value matching `ty`. The
/// thunk decodes them back into a typed `Reactive<T>` enum the
/// wrapper consumes. `ty` itself stays the inner `T` so consumers
/// that don't know about reactive props see the inner type and can
/// type-check call sites uniformly.
#[derive(Debug, Clone)]
pub struct PropDef {
    /// The prop's identifier (the binding name visible inside
    /// the component's view body — `initial`, `step`, etc.).
    pub name: Arc<str>,
    /// Full type representation. Same shape the JIT publisher
    /// (`blinc_dsl_core`) reads off `TypedFunction::params[i].ty`
    /// — keeping the substrate Type identical to Zyntax's
    /// avoids a translation layer between the two.
    pub ty: Type,
    /// `Some(inner)` for `Reactive<inner>` props; `None` for plain
    /// props. `inner` is the inner type (`i32` / `f64` / `bool`)
    /// the lowering pass uses to encode literals.
    pub reactive_inner: Option<Type>,
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
    use zyntax_typed_ast::type_registry::PrimitiveType;

    fn arc(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    fn i32_ty() -> Type {
        Type::Primitive(PrimitiveType::I32)
    }

    fn f64_ty() -> Type {
        Type::Primitive(PrimitiveType::F64)
    }

    /// Prop lookup by name returns the matching `PropDef`, or
    /// `None` for unknown names. Pin the primitive-prop shape.
    #[test]
    fn prop_lookup_by_name() {
        let def = ComponentDefinition {
            name: arc("Counter"),
            view_symbol: arc("Counter$view"),
            props: vec![
                PropDef {
                    name: arc("initial"),
                    ty: i32_ty(),
                    reactive_inner: None,
                },
                PropDef {
                    name: arc("step"),
                    ty: i32_ty(),
                    reactive_inner: None,
                },
            ],
        };
        assert_eq!(def.prop_count(), 2);
        assert_eq!(def.prop("initial").map(|p| &p.ty), Some(&i32_ty()));
        assert_eq!(def.prop("step").map(|p| &p.ty), Some(&i32_ty()));
        assert!(def.prop("missing").is_none());
    }

    /// Complex prop type — substrate stores it as-is. The
    /// consumer (widget code, devtools) can drill into
    /// `Type::Array { element_type, .. }` to introspect.
    #[test]
    fn prop_holds_complex_type() {
        use zyntax_typed_ast::type_registry::NullabilityKind;
        let array_of_f64 = Type::Array {
            element_type: Box::new(f64_ty()),
            size: None,
            nullability: NullabilityKind::NonNull,
        };

        let def = ComponentDefinition {
            name: arc("Histogram"),
            view_symbol: arc("Histogram$view"),
            props: vec![PropDef {
                name: arc("buckets"),
                ty: array_of_f64.clone(),
                reactive_inner: None,
            }],
        };
        let prop = def.prop("buckets").unwrap();
        match &prop.ty {
            Type::Array { element_type, .. } => {
                assert_eq!(**element_type, f64_ty());
            }
            other => panic!("expected Type::Array, got {other:?}"),
        }
    }
}
