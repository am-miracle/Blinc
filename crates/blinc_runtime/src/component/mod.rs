//! Runtime-agnostic component registry.
//!
//! Parallels [`crate::fsm`] but for component declarations
//! rather than FSMs. The registry holds:
//!
//! - Component name → view-symbol mapping. Lets widget /
//!   tooling code translate a user-visible name (e.g.
//!   `"Counter"`) into the JIT-linker-visible symbol
//!   (`"Counter$view"`) without manually concatenating the
//!   suffix.
//!
//! - Prop type lists. Components declared in the DSL with
//!   `component Counter (initial: i32, step: i32) { ... }`
//!   publish their props here so introspection tooling
//!   (devtools panes, hot-reload validators, design-time prop
//!   editors) can reason about what each component accepts
//!   without consulting the DSL compiler.
//!
//! Both publishers feed the same shape: the JIT path
//! (`blinc_dsl_core`) walks Class + Impl pairs after
//! `bind_component_props` has injected prop params; a future
//! AOT codegen emits a generated init function that registers
//! the same definitions at startup.
//!
//! ## Why a registry separately from view rendering
//!
//! [`crate::view::ViewRenderer`] only deals with the
//! `symbol -> ops` half — it doesn't know what props a
//! component takes or even which symbols are components. The
//! registry is the side channel that answers "what components
//! exist?" and "what does this one need?". Together they cover
//! the introspection + dispatch story.
//!
//! ## Out of scope today
//!
//! - **Calling a component with props.** The substrate
//!   represents prop types but doesn't yet provide a typed
//!   `render_with_args(name, args)` shape — that requires a
//!   typed-args ABI both backends agree on, which is its own
//!   slice. Callers wire props via the JIT path's grammar-
//!   level lowering (where `Counter(42)` becomes
//!   `Counter$view(42)`) until the substrate grows that surface.
//!
//! - **Nested components / slots.** The flatten pass in
//!   `blinc_dsl_core::lower_component_calls` already inlines
//!   children at the parent's call site, so the registry only
//!   needs to track the top-level component shape, not its
//!   composition tree.

pub mod definition;
pub mod registry;

#[allow(deprecated)]
pub use definition::PropType;
pub use definition::{ComponentDefinition, PropDef, Type};
pub use registry::{
    ComponentId, ComponentRegistry, with_component_registry, with_component_registry_mut,
};
