use super::*;
use crate::host::{
    blinc_format_int, blinc_fsm_runtime_trigger, blinc_fsm_subscribe, blinc_signal_get_f64,
    blinc_signal_get_i32, blinc_signal_get_string, blinc_signal_set_f64, blinc_signal_set_i32,
    blinc_signal_set_string, blinc_string_concat, blinc_text, blinc_text_int,
};
use crate::widget_ffi::{
    blinc_canvas_view, blinc_div_view, blinc_image_view, blinc_motion_view, blinc_new_child_list,
    blinc_new_struct_value, blinc_new_style_overlay, blinc_notch_view, blinc_push_child,
    blinc_rich_text_view, blinc_set_overlay_bg, blinc_set_overlay_border_color,
    blinc_set_overlay_border_width, blinc_set_overlay_corner_radius, blinc_set_overlay_opacity,
    blinc_set_struct_f64, blinc_set_struct_handle, blinc_set_struct_i32, blinc_set_struct_i64,
    blinc_set_struct_string, blinc_stack_view, blinc_svg_view, blinc_text_view,
};

/// Pairs a DSL-visible symbol name with an `extern "C"` fn pointer and signature.
/// Used for runtime registration AND type-system injection (spliced as an extern
/// fn decl into each parsed `TypedProgram` before `compile_typed_program`).
struct BuiltinDescriptor {
    /// Mangled symbol the grammar lowers to (no `@builtin` alias indirection).
    name: &'static str,
    param_types: &'static [Type],
    return_type: Type,
    /// `extern "C"` fn cast to `*const u8` for `register_function`.
    ptr: *const u8,
}

// SAFETY: Only fn pointers and `'static` references inside.
unsafe impl Sync for BuiltinDescriptor {}

/// All host builtins. Ordering irrelevant — registration walks the full table.
fn builtins() -> Vec<BuiltinDescriptor> {
    vec![
        BuiltinDescriptor {
            name: "$Blinc$text",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_text as *const u8,
        },
        BuiltinDescriptor {
            name: "$Blinc$text_int",
            param_types: &[Type::Primitive(PrimitiveType::I32)],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_text_int as *const u8,
        },
        BuiltinDescriptor {
            // `<name>.get()` lowered by `resolve_signal_calls`.
            name: "__signal_get_i32",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::I32),
            ptr: blinc_signal_get_i32 as *const u8,
        },
        BuiltinDescriptor {
            name: "__signal_get_f64",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::F64),
            ptr: blinc_signal_get_f64 as *const u8,
        },
        BuiltinDescriptor {
            name: "__signal_get_string",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::String),
            ptr: blinc_signal_get_string as *const u8,
        },
        BuiltinDescriptor {
            // `<sig> = <expr>` inside a function / closure body
            // lowers via `resolve_signal_calls` to a call here
            // with the LHS's interned name and the (already
            // signal-rewritten) RHS value.
            name: "__signal_set_i32",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I32),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_signal_set_i32 as *const u8,
        },
        BuiltinDescriptor {
            name: "__signal_set_f64",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::F64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_signal_set_f64 as *const u8,
        },
        BuiltinDescriptor {
            name: "__signal_set_string",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::String),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_signal_set_string as *const u8,
        },
        BuiltinDescriptor {
            // `<FsmName>.trigger("State.Event")` lowered by `resolve_fsm_trigger_calls`.
            name: "__fsm_runtime_trigger__",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::String),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_fsm_runtime_trigger as *const u8,
        },
        BuiltinDescriptor {
            // `<FsmName>.subscribe("From.Event", closure)` — third arg is the
            // closure's raw fn ptr smuggled as i64.
            name: "__fsm_subscribe__",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_fsm_subscribe as *const u8,
        },
        BuiltinDescriptor {
            // `__fstring_format__` (i32 only — f64 needs a separate `__fstring_format_f64__`).
            name: "$Blinc$format_int",
            param_types: &[Type::Primitive(PrimitiveType::I32)],
            return_type: Type::Primitive(PrimitiveType::String),
            ptr: blinc_format_int as *const u8,
        },
        BuiltinDescriptor {
            // `string_concat` — chains f-string parts.
            name: "$Blinc$string_concat",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::String),
            ],
            return_type: Type::Primitive(PrimitiveType::String),
            ptr: blinc_string_concat as *const u8,
        },
        BuiltinDescriptor {
            // `Text("hi")` → leaked `WidgetBox::Text(...)` as i64.
            name: "$Blinc$Text$view",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::String),
            ],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_text_view as *const u8,
        },
        BuiltinDescriptor {
            // `Div(children, style, class, on_click)`. `class` = whitespace-sep names,
            // `on_click` = raw fn ptr as i64 (0 = none).
            name: "$Blinc$Div$view",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_div_view as *const u8,
        },
        BuiltinDescriptor {
            name: "$Blinc$Stack$view",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_stack_view as *const u8,
        },
        BuiltinDescriptor {
            name: "$Blinc$Image$view",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_image_view as *const u8,
        },
        BuiltinDescriptor {
            name: "$Blinc$Svg$view",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::String),
            ],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_svg_view as *const u8,
        },
        BuiltinDescriptor {
            name: "$Blinc$Canvas$view",
            param_types: &[Type::Primitive(PrimitiveType::I64)],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_canvas_view as *const u8,
        },
        BuiltinDescriptor {
            name: "$Blinc$RichText$view",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_rich_text_view as *const u8,
        },
        BuiltinDescriptor {
            name: "$Blinc$Motion$view",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_motion_view as *const u8,
        },
        BuiltinDescriptor {
            name: "$Blinc$Notch$view",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_notch_view as *const u8,
        },
        BuiltinDescriptor {
            // `__new_child_list__()` — mint `Vec<WidgetHandle>`, populated by `__push_child__`.
            name: "__new_child_list__",
            param_types: &[],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_new_child_list as *const u8,
        },
        BuiltinDescriptor {
            // `__push_child__(list, child)` — append. List pointer stays live for container.
            name: "__push_child__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_push_child as *const u8,
        },
        // Struct-value builders — complex widget props cross the extern ABI as i64 handles.
        BuiltinDescriptor {
            name: "__new_struct_value__",
            param_types: &[],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_new_struct_value as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_struct_i32__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I32),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_struct_i32 as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_struct_i64__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_struct_i64 as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_struct_f64__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::F64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_struct_f64 as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_struct_string__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::String),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_struct_string as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_struct_handle__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_struct_handle as *const u8,
        },
        // Style-overlay builders — mirror the child-list pattern.
        BuiltinDescriptor {
            name: "__new_style_overlay__",
            param_types: &[],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_new_style_overlay as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_overlay_bg__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_overlay_bg as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_overlay_opacity__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::F64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_overlay_opacity as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_overlay_corner_radius__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::F64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_overlay_corner_radius as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_overlay_border_width__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::F64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_overlay_border_width as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_overlay_border_color__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_overlay_border_color as *const u8,
        },
    ]
}

/// Map a typed-AST `Type` to the wire-format `TypeTag` for `ZrtlSymbolSig`.
pub(crate) fn type_to_tag(ty: &Type) -> TypeTag {
    match ty {
        Type::Primitive(PrimitiveType::Unit) => TypeTag::VOID,
        Type::Primitive(PrimitiveType::String) => TypeTag::STRING,
        Type::Primitive(PrimitiveType::I32) => TypeTag::I32,
        Type::Primitive(PrimitiveType::I64) => TypeTag::I64,
        Type::Primitive(PrimitiveType::F64) => TypeTag::F64,
        // Panic loudly — silent VOID would break codegen.
        _ => panic!(
            "blinc_dsl_core: no TypeTag mapping for {ty:?} \
             — extend `type_to_tag` when adding new builtin types"
        ),
    }
}

/// Map a typed-AST `Type` to the runtime `NativeType` for `call_function`.
/// Strings cross the FFI as `NativeType::Ptr` (length-prefixed buffer).
pub(crate) fn type_to_native(ty: &Type) -> Result<NativeType, &Type> {
    match ty {
        Type::Primitive(PrimitiveType::Unit) => Ok(NativeType::Void),
        Type::Primitive(PrimitiveType::String) => Ok(NativeType::Ptr),
        Type::Primitive(PrimitiveType::I32) => Ok(NativeType::I32),
        Type::Primitive(PrimitiveType::I64) => Ok(NativeType::I64),
        Type::Primitive(PrimitiveType::F64) => Ok(NativeType::F64),
        other => Err(other),
    }
}

/// Build the ZRTL signature for a builtin (stored in `backend.symbol_signatures`).
fn descriptor_to_sig(b: &BuiltinDescriptor) -> ZrtlSymbolSig {
    assert!(
        b.param_types.len() <= ZRTL_MAX_PARAMS,
        "{}: parameter count {} exceeds ZRTL_MAX_PARAMS ({})",
        b.name,
        b.param_types.len(),
        ZRTL_MAX_PARAMS
    );

    let mut params = [TypeTag::VOID; ZRTL_MAX_PARAMS];
    for (i, ty) in b.param_types.iter().enumerate() {
        params[i] = type_to_tag(ty);
    }

    ZrtlSymbolSig {
        param_count: b.param_types.len() as u8,
        flags: ZrtlSigFlags::NONE,
        return_type: type_to_tag(&b.return_type),
        params,
    }
}

/// Register all `$Blinc$*` builtins on the runtime with full signatures.
pub(crate) fn register_builtins(runtime: &mut ZyntaxRuntime) {
    for b in builtins() {
        let sig = descriptor_to_sig(&b);
        runtime.register_function_typed(b.name, b.ptr, sig);
    }
}
