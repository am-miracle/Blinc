//! `#[extern_widget]` — Rust → Blinc DSL widget export.
//!
//! Generates the JIT thunk decoding FFI args + the `ExternWidget`
//! trait impl carrying the spec. Re-exported from `blinc_dsl_core`
//! so users only need one import.

use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;

/// FFI / decode / DSL-type tuple for one widget prop. Built from a
/// `syn::Type` by [`classify_param_type`].
struct ParamKind {
    ffi_ty: proc_macro2::TokenStream,
    decode: proc_macro2::TokenStream,
    prop_type_expr: proc_macro2::TokenStream,
    param_type_expr: proc_macro2::TokenStream,
}

fn classify_param_type(ty: &syn::Type) -> Option<ParamKind> {
    let syn::Type::Path(p) = ty else {
        return None;
    };
    let segment = p.path.segments.last()?;
    if !matches!(segment.arguments, syn::PathArguments::None) {
        return None;
    }
    let ident = &segment.ident;
    match ident.to_string().as_str() {
        "String" => Some(ParamKind {
            ffi_ty: quote! { *const i32 },
            decode: quote! {
                // SAFETY: registered signature pins `String` here so the JIT
                // hands us a length-prefixed UTF-8 buffer.
                unsafe { ::blinc_dsl_core::__extern_widget_internals::decode_string(__arg) }
            },
            prop_type_expr: quote! {
                ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::String
                )
            },
            param_type_expr: quote! {
                ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::String
                )
            },
        }),
        "i32" => Some(ParamKind {
            ffi_ty: quote! { i32 },
            decode: quote! { __arg },
            prop_type_expr: quote! {
                ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I32
                )
            },
            param_type_expr: quote! {
                ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I32
                )
            },
        }),
        "bool" => Some(ParamKind {
            ffi_ty: quote! { i32 },
            decode: quote! { __arg != 0 },
            prop_type_expr: quote! {
                ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::Bool
                )
            },
            param_type_expr: quote! {
                ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I32
                )
            },
        }),
        "i64" => Some(ParamKind {
            ffi_ty: quote! { i64 },
            decode: quote! { __arg },
            prop_type_expr: quote! {
                ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I64
                )
            },
            param_type_expr: quote! {
                ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I64
                )
            },
        }),
        "f64" => Some(ParamKind {
            ffi_ty: quote! { f64 },
            decode: quote! { __arg },
            prop_type_expr: quote! {
                ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::F64
                )
            },
            param_type_expr: quote! {
                ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::F64
                )
            },
        }),
        "Vec" | "Box" | "Option" => None,
        _ => {
            let dsl_type_name = ident.to_string();
            Some(ParamKind {
                ffi_ty: quote! { i64 },
                decode: quote! {
                    {
                        // SAFETY: complex DSL struct props are lowered to
                        // `__new_struct_value__` handles before this thunk is called.
                        let __value = unsafe {
                            ::blinc_dsl_core::__extern_widget_internals::decode_struct(__arg)
                        };
                        <#ty as ::core::convert::TryFrom<
                            ::blinc_dsl_core::__extern_widget_internals::BlincStructValue
                        >>::try_from(__value)
                            .unwrap_or_else(|_| panic!(
                                "failed to decode DSL struct prop `{}` as `{}`",
                                #dsl_type_name,
                                stringify!(#ty)
                            ))
                    }
                },
                prop_type_expr: quote! {
                    ::blinc_dsl_core::__extern_widget_internals::Type::Unresolved(
                        ::blinc_dsl_core::__extern_widget_internals::InternedString::new_global(
                            #dsl_type_name
                        )
                    )
                },
                param_type_expr: quote! {
                    ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                        ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I64
                    )
                },
            })
        }
    }
}

/// Parsed `#[extern_widget(name = "X", namespace = "ns"?, styled?)]` args.
struct ExternWidgetArgs {
    /// Bare DSL-visible name, e.g. `"Button"`. Required.
    name: String,
    /// Optional namespace prefix, e.g. `"cn"`. When set, the
    /// registered DSL name becomes `"<namespace>.<name>"` (e.g.
    /// `"cn.Button"`) and the grammar's dotted-component-call shape
    /// `cn.Button(...)` resolves to this widget. Empty namespace is
    /// equivalent to omitting the field — the widget registers at
    /// the top level under just `<name>`.
    namespace: Option<String>,
    /// When true, the macro wraps the widget in `Styled<W>` and the
    /// spec advertises a `__style` prop the lowering pass populates
    /// from inline DSL styling args.
    styled: bool,
}

impl ExternWidgetArgs {
    /// Qualified DSL name as registered with the runtime. With a
    /// namespace it's `"<ns>.<name>"`; without, just `"<name>"`.
    fn dsl_name(&self) -> String {
        match &self.namespace {
            Some(ns) if !ns.is_empty() => format!("{ns}.{}", self.name),
            _ => self.name.clone(),
        }
    }

    /// Rust-identifier-safe form of the qualified name — the dot in
    /// `cn.Button` becomes an underscore so the thunk fn / JIT symbol
    /// remain valid Rust idents and well-formed linker symbols.
    /// Equivalent to `dsl_name().replace('.', "_")`.
    fn symbol_safe_name(&self) -> String {
        self.dsl_name().replace('.', "_")
    }
}

fn field_has_children_attr(field: &syn::Field) -> bool {
    field
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("children"))
}

fn field_slot_name(field: &syn::Field) -> Option<String> {
    let attr = field.attrs.iter().find(|a| a.path().is_ident("slot"))?;
    let mut name: Option<String> = None;
    let _ = attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("name") {
            let lit: syn::LitStr = meta.value()?.parse()?;
            name = Some(lit.value());
            Ok(())
        } else {
            Err(meta.error("expected `name = \"...\"`"))
        }
    });
    name
}

impl syn::parse::Parse for ExternWidgetArgs {
    /// Accepts `name = "..."`, optional `namespace = "..."`, and the
    /// bare `styled` flag in any order. `name` is required; everything
    /// else defaults. Trailing commas are tolerated.
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut name: Option<(syn::LitStr, proc_macro2::Span)> = None;
        let mut namespace: Option<String> = None;
        let mut styled = false;

        loop {
            if input.is_empty() {
                break;
            }
            let key: syn::Ident = input.parse()?;
            match key.to_string().as_str() {
                "name" => {
                    let _: syn::Token![=] = input.parse()?;
                    let value: syn::LitStr = input.parse()?;
                    name = Some((value, key.span()));
                }
                "namespace" => {
                    let _: syn::Token![=] = input.parse()?;
                    let value: syn::LitStr = input.parse()?;
                    namespace = Some(value.value());
                }
                "styled" => {
                    styled = true;
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown #[extern_widget] arg `{other}` — expected one of \
                             `name = \"...\"`, `namespace = \"...\"`, `styled`"
                        ),
                    ));
                }
            }
            if input.is_empty() {
                break;
            }
            let _: syn::Token![,] = input.parse()?;
        }

        let Some((name_lit, _)) = name else {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "#[extern_widget] requires `name = \"<DslName>\"`",
            ));
        };
        if let Some(ns) = &namespace {
            let invalid = ns.is_empty()
                || ns.contains('.')
                || !ns.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                || !ns.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if invalid {
                return Err(syn::Error::new(
                    name_lit.span(),
                    "#[extern_widget] `namespace` must be a single lowercase-leading \
                     identifier — e.g. `namespace = \"cn\"`. The DSL grammar disambiguates \
                     namespaced calls (`cn.Button(…)`) from method calls on uppercase \
                     types (`Counter.method(…)`) by requiring a lowercase namespace head, \
                     so this constraint is load-bearing rather than stylistic.",
                ));
            }
        }
        Ok(Self {
            name: name_lit.value(),
            namespace,
            styled,
        })
    }
}

/// Export a Rust struct as a Blinc DSL widget.
///
/// ```ignore
/// #[extern_widget(name = "FancyText")]
/// pub struct FancyText { pub content: String }
///
/// impl ElementBuilder for FancyText { /* … */ }
/// ```
///
/// Named fields become DSL-visible props; `String` / `bool` / `i32` /
/// `i64` / `f64` are supported scalar types today. Mark a `Vec<Box<dyn
/// ElementBuilder>>` field with `#[children]` to receive the parent's
/// body block, or `#[slot(name = "…")]` for named slots.
///
/// Register at runtime via `dsl.register_extern_widget::<FancyText>()?`.
/// The JIT linker symbol is `$Blinc$<Name>$view`.
#[proc_macro_attribute]
pub fn extern_widget(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as ExternWidgetArgs);
    let mut item_struct = parse_macro_input!(item as syn::ItemStruct);

    let struct_ident = item_struct.ident.clone();
    // `dsl_name` is what flows into the runtime registry — the
    // qualified form (`"cn.Button"`) when a namespace is set; the bare
    // `"Button"` otherwise. The grammar's `__component_call__("cn.Button", …)`
    // lookup is name-based, so a dotted DSL name resolves the same way
    // a bare one does.
    let dsl_name = args.dsl_name();
    // `symbol_safe_name` keeps the dot out of Rust identifiers / JIT
    // linker symbols. `cn.Button` → `cn_Button` for the thunk + view
    // symbol; the registry still sees the dotted form via `dsl_name`.
    let symbol_safe_name = args.symbol_safe_name();
    let styled = args.styled;

    if !item_struct.generics.params.is_empty() {
        return syn::Error::new_spanned(
            &item_struct.generics,
            "#[extern_widget] doesn't support generic widgets yet — drop the type parameters \
             or hand-roll the registration via `BlincDsl::register_extern_widget_spec`",
        )
        .to_compile_error()
        .into();
    }

    let syn::Fields::Named(fields) = &item_struct.fields else {
        return syn::Error::new_spanned(
            &item_struct.fields,
            "#[extern_widget] requires a struct with named fields — tuple and unit structs aren't \
             supported",
        )
        .to_compile_error()
        .into();
    };

    let thunk_ident = syn::Ident::new(
        &format!("__blinc_extern_{symbol_safe_name}_view"),
        proc_macro2::Span::call_site(),
    );
    let view_symbol = format!("$Blinc${symbol_safe_name}$view");

    // FFI order: children → slots → scalars.
    let mut children_field: Option<&syn::Field> = None;
    let mut slot_fields: Vec<(&syn::Field, String)> = Vec::new();
    let mut scalar_fields: Vec<&syn::Field> = Vec::new();
    for field in &fields.named {
        if field_has_children_attr(field) {
            if children_field.is_some() {
                return syn::Error::new_spanned(
                    field,
                    "#[extern_widget] supports at most one `#[children]` field",
                )
                .to_compile_error()
                .into();
            }
            children_field = Some(field);
        } else if let Some(slot_name) = field_slot_name(field) {
            slot_fields.push((field, slot_name));
        } else {
            scalar_fields.push(field);
        }
    }

    let mut thunk_params: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut thunk_decodes: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut struct_inits: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut prop_defs: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut param_types: Vec<proc_macro2::TokenStream> = Vec::new();

    if let Some(field) = children_field {
        if !matches!(field.vis, syn::Visibility::Public(_)) {
            return syn::Error::new_spanned(
                &field.vis,
                "#[extern_widget] `#[children]` field must be `pub`",
            )
            .to_compile_error()
            .into();
        }
        let field_ident = field
            .ident
            .as_ref()
            .expect("named fields always have idents");
        let ffi_arg_ident = syn::Ident::new("__arg_children", field_ident.span());
        thunk_params.push(quote! { #ffi_arg_ident: i64 });
        thunk_decodes.push(quote! {
            // SAFETY: `lower_children_arrays_to_blocks` is the only producer of
            // these pointers; the call site can't forge one.
            let #field_ident = unsafe {
                ::blinc_dsl_core::__extern_widget_internals::decode_children(#ffi_arg_ident)
            };
        });
        struct_inits.push(quote! { #field_ident });
        prop_defs.push(quote! {
            ::blinc_dsl_core::__extern_widget_internals::PropDef {
                name: ::std::sync::Arc::from("children"),
                ty: ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I64
                ),
            }
        });
        param_types.push(quote! {
            ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I64
            )
        });
    }

    for (field, slot_name) in &slot_fields {
        if !matches!(field.vis, syn::Visibility::Public(_)) {
            return syn::Error::new_spanned(
                &field.vis,
                "#[extern_widget] `#[slot]` field must be `pub`",
            )
            .to_compile_error()
            .into();
        }
        let field_ident = field
            .ident
            .as_ref()
            .expect("named fields always have idents");
        let ffi_arg_ident = syn::Ident::new(&format!("__arg_slot_{slot_name}"), field_ident.span());
        let prop_name = format!("slot_{slot_name}");
        thunk_params.push(quote! { #ffi_arg_ident: i64 });
        thunk_decodes.push(quote! {
            // SAFETY: same contract as the default children pointer.
            let #field_ident = unsafe {
                ::blinc_dsl_core::__extern_widget_internals::decode_children(#ffi_arg_ident)
            };
        });
        struct_inits.push(quote! { #field_ident });
        prop_defs.push(quote! {
            ::blinc_dsl_core::__extern_widget_internals::PropDef {
                name: ::std::sync::Arc::from(#prop_name),
                ty: ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I64
                ),
            }
        });
        param_types.push(quote! {
            ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I64
            )
        });
    }

    for (idx, field) in scalar_fields.iter().enumerate() {
        let field_ident = field
            .ident
            .as_ref()
            .expect("named fields always have idents");
        let field_name = field_ident.to_string();

        if !matches!(field.vis, syn::Visibility::Public(_)) {
            return syn::Error::new_spanned(
                &field.vis,
                "#[extern_widget] fields must be `pub` — non-public fields can't be set from DSL \
                 source. Make the field `pub` or move internal state into a wrapper struct.",
            )
            .to_compile_error()
            .into();
        }

        let Some(kind) = classify_param_type(&field.ty) else {
            return syn::Error::new_spanned(
                &field.ty,
                "#[extern_widget] fields must be String, i32, i64, f64, or a non-generic custom \
                 type that implements TryFrom<BlincStructValue> (or use `#[children]` for a \
                 `Vec<Box<dyn ElementBuilder>>` children slot)",
            )
            .to_compile_error()
            .into();
        };

        let ffi_arg_ident = syn::Ident::new(&format!("__arg_{idx}"), field_ident.span());
        let ffi_ty = &kind.ffi_ty;
        let decode = &kind.decode;
        let prop_type_expr = &kind.prop_type_expr;
        let param_type_expr = &kind.param_type_expr;

        thunk_params.push(quote! { #ffi_arg_ident: #ffi_ty });
        thunk_decodes.push(quote! {
            let #field_ident = {
                let __arg = #ffi_arg_ident;
                #decode
            };
        });
        struct_inits.push(quote! { #field_ident });
        prop_defs.push(quote! {
            ::blinc_dsl_core::__extern_widget_internals::PropDef {
                name: ::std::sync::Arc::from(#field_name),
                ty: #prop_type_expr,
            }
        });
        param_types.push(param_type_expr.clone());
    }

    if styled {
        thunk_params.push(quote! { __arg_style: i64 });
        prop_defs.push(quote! {
            ::blinc_dsl_core::__extern_widget_internals::PropDef {
                name: ::std::sync::Arc::from("__style"),
                ty: ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                    ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I64
                ),
            }
        });
        param_types.push(quote! {
            ::blinc_dsl_core::__extern_widget_internals::Type::Primitive(
                ::blinc_dsl_core::__extern_widget_internals::PrimitiveType::I64
            )
        });
    }

    // Strip macro-only field attributes before re-emitting the struct.
    if let syn::Fields::Named(named) = &mut item_struct.fields {
        for field in &mut named.named {
            field
                .attrs
                .retain(|attr| !(attr.path().is_ident("children") || attr.path().is_ident("slot")));
        }
    }

    let widget_construction = if styled {
        quote! {
            // SAFETY: `__arg_style` is `0` or a `__new_style_overlay__` pointer.
            let __overlay = unsafe {
                ::blinc_dsl_core::__extern_widget_internals::decode_overlay(__arg_style)
            };
            let __widget: Box<dyn ::blinc_layout::div::ElementBuilder> = Box::new(
                ::blinc_dsl_core::__extern_widget_internals::Styled::new(
                    #struct_ident { #(#struct_inits),* },
                    __overlay,
                )
            );
        }
    } else {
        quote! {
            let __widget: Box<dyn ::blinc_layout::div::ElementBuilder> =
                Box::new(#struct_ident { #(#struct_inits),* });
        }
    };

    let expanded = quote! {
        #item_struct

        #[doc(hidden)]
        #[allow(non_snake_case)]
        extern "C" fn #thunk_ident(#(#thunk_params),*) -> i64 {
            #(#thunk_decodes)*
            #widget_construction
            ::blinc_dsl_core::__extern_widget_internals::into_handle(__widget)
        }

        impl ::blinc_dsl_core::__extern_widget_internals::ExternWidget for #struct_ident {
            // User-facing qualified form — `"cn.Button"` for namespaced
            // widgets, `"Button"` otherwise. Surfaces in diagnostics and
            // is the form a DSL author writes at the call site.
            const DSL_NAME: &'static str = #dsl_name;

            fn extern_widget_spec()
                -> ::blinc_dsl_core::__extern_widget_internals::ExternWidgetSpec
            {
                use ::blinc_dsl_core::__extern_widget_internals::{
                    ExternWidgetSpec, PrimitiveType, Type,
                };
                ExternWidgetSpec {
                    // Registry key is the mangled form (`cn_Button`).
                    // `lower_component_calls` replaces the dot in the
                    // grammar-emitted dotted name with `_` before doing
                    // its lookup, and `primitive_callee_props` reverses
                    // the linker symbol (`$Blinc$cn_Button$view`) to the
                    // same form. Keeping the registry on the mangled
                    // side means both lookup directions agree without
                    // a second substitution.
                    name: #symbol_safe_name.to_string(),
                    view_symbol: #view_symbol.to_string(),
                    props: vec![#(#prop_defs),*],
                    param_types: vec![#(#param_types),*],
                    return_type: Type::Primitive(PrimitiveType::I64),
                    extern_ptr: #thunk_ident as *const u8,
                }
            }
        }
    };

    TokenStream::from(expanded)
}
