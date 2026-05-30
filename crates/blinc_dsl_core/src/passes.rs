// =====================================================================
// FSM dispatch synthesis (post-parse)
// =====================================================================

use super::*;
use std::path::Path;

/// Shape-based recognition for `signal <name>: <T>` decls: extern fn, no body,
/// no params, primitive return, `link_name: None` (so we don't catch host builtins).
pub(crate) fn is_signal_decl(func: &zyntax_typed_ast::typed_ast::TypedFunction) -> bool {
    func.is_external
        && func.body.is_none()
        && func.params.is_empty()
        && func.link_name.is_none()
        && matches!(func.return_type, Type::Primitive(_))
}

/// Run the JIT'd view once and box the resulting widget builder (no reactive wrapper).
pub(crate) fn materialize_view(
    renderer: &std::sync::Arc<dyn blinc_runtime::view::ViewRenderer>,
) -> Box<dyn blinc_layout::div::ElementBuilder> {
    use zyntax_embed::ZyntaxValue;
    let value = blinc_runtime::view::render_main(renderer).expect("render_main");
    let ZyntaxValue::Int(handle) = value else {
        return Box::new(blinc_layout::div::Div::new());
    };
    unsafe { materialize_widget(handle) }
        .map(|w| w.into_element_builder())
        .unwrap_or_else(|| Box::new(blinc_layout::div::Div::new()))
}

/// Detect view decorators and strip the synthetic marker calls.
/// Returns `(saw_stateful, explicit_signal_deps, explicit_fsms)`.
/// Empty `signal_deps` with `saw_stateful=true` means subscribe to all declared signals.
pub(crate) fn detect_and_strip_stateful_views(
    program: &mut TypedProgram,
) -> (bool, Vec<String>, Vec<String>) {
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedExpression, TypedLiteral};

    // Strip a leading marker call matching `expected_callee` and return its string args.
    fn strip_leading_marker(
        body: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        expected_callee: &str,
    ) -> Option<Vec<String>> {
        let matches = body.statements.first().and_then(|s| match &s.node {
            TypedStatement::Expression(e) => match &e.node {
                TypedExpression::Call(c) => match &c.callee.node {
                    TypedExpression::Variable(name)
                        if name.resolve_global().as_deref() == Some(expected_callee) =>
                    {
                        Some(c.positional_args.clone())
                    }
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        });
        let args = matches?;
        let names: Vec<String> = args
            .into_iter()
            .filter_map(|arg| match &arg.node {
                TypedExpression::Literal(TypedLiteral::String(s)) => {
                    s.resolve_global().map(|n| n.to_string())
                }
                _ => None,
            })
            .collect();
        body.statements.remove(0);
        Some(names)
    }

    let mut saw_stateful = false;
    let mut signal_deps: Vec<String> = Vec::new();
    let mut fsms: Vec<String> = Vec::new();

    let mut process = |body: &mut Option<zyntax_typed_ast::typed_ast::TypedBlock>| {
        let Some(body) = body else {
            return;
        };
        // Decorators can stack either way; strip until no more match.
        loop {
            if let Some(names) = strip_leading_marker(body, "__stateful_view__") {
                saw_stateful = true;
                for n in names {
                    if !signal_deps.contains(&n) {
                        signal_deps.push(n);
                    }
                }
                continue;
            }
            if let Some(names) = strip_leading_marker(body, "__fsm_view__") {
                saw_stateful = true;
                for n in names {
                    if !fsms.contains(&n) {
                        fsms.push(n);
                    }
                }
                continue;
            }
            break;
        }
    };

    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => process(&mut func.body),
            TypedDeclaration::Impl(imp) => {
                for method in imp.methods.iter_mut() {
                    process(&mut method.body);
                }
            }
            _ => {}
        }
    }

    (saw_stateful, signal_deps, fsms)
}

/// Snapshot `signal <name>: <T>` and `fsm <Name> { … }` decls. MUST run BEFORE
/// the signal-rewrite / fsm-meta-strip passes — they erase the originating decls.
pub(crate) fn collect_declared(program: &TypedProgram) -> (Vec<(String, Type)>, Vec<String>) {
    use zyntax_typed_ast::typed_ast::TypedDeclaration;
    let mut signals = Vec::new();
    let mut fsms = Vec::new();
    for decl in &program.declarations {
        match &decl.node {
            TypedDeclaration::Function(func) if is_signal_decl(func) => {
                if let Some(name) = func.name.resolve_global() {
                    signals.push((name.to_string(), func.return_type.clone()));
                }
            }
            TypedDeclaration::Impl(imp) => {
                let is_fsm = imp
                    .methods
                    .iter()
                    .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"));
                if is_fsm && let Some(name) = imp.trait_name.resolve_global() {
                    fsms.push(name.to_string());
                }
            }
            _ => {}
        }
    }
    (signals, fsms)
}

/// Extract CSS from `__blinc_stylesheet__` marker fns and remove them from the
/// program. The CSS text is run through [`auto_inject_semicolons`] so `;`-free
/// declarations work.
pub(crate) fn extract_and_strip_stylesheets(program: &mut TypedProgram, out: &mut Vec<String>) {
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedExpression, TypedLiteral, TypedStatement,
    };
    program.declarations.retain(|decl| {
        let TypedDeclaration::Function(func) = &decl.node else {
            return true;
        };
        if func.name.resolve_global().as_deref() != Some("__blinc_stylesheet__") {
            return true;
        }
        let Some(body) = &func.body else {
            return false;
        };
        for stmt in &body.statements {
            let TypedStatement::Expression(expr) = &stmt.node else {
                continue;
            };
            if let TypedExpression::Literal(TypedLiteral::String(s)) = &expr.node
                && let Some(text) = s.resolve_global()
            {
                out.push(auto_inject_semicolons(&text));
            }
        }
        false
    });
}

/// Append `;` inside `{ ... }` blocks where the line's last char doesn't already
/// terminate. Brace depth tracking is naïve — string/comment braces will skew it.
pub(crate) fn auto_inject_semicolons(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + raw.len() / 8);
    let mut depth: i32 = 0;
    for line in raw.split_inclusive('\n') {
        // Separate body from trailing whitespace so we inject `;` before the newline.
        let line_end_idx = line
            .rfind(|c: char| !c.is_whitespace())
            .map(|i| i + line[i..].chars().next().map(char::len_utf8).unwrap_or(0))
            .unwrap_or(line.len());
        let body = &line[..line_end_idx];
        let tail = &line[line_end_idx..];

        let depth_before = depth;
        depth += body.matches('{').count() as i32;
        depth -= body.matches('}').count() as i32;

        out.push_str(body);

        let trimmed = body.trim_start();
        let last_char = body.chars().rev().find(|c| !c.is_whitespace());
        let is_comment_line =
            trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*");
        let inside_block = depth_before > 0;
        let needs_semi = inside_block
            && !is_comment_line
            && match last_char {
                None => false,
                Some(c) => !matches!(c, ';' | '{' | '}' | ',' | '/' | '*'),
            };
        if needs_semi {
            out.push(';');
        }
        out.push_str(tail);
    }
    out
}

/// Rewrite `<sig>.get()` / `<sig>.set(v)` / `<sig> = v` into `__signal_<get|set>_<T>` calls.
/// One entry in the per-compile signal map: the declared type plus the
/// Walk every `__blinc_const_group__` decl, hoist each contained
/// member into its own `__blinc_const__` marker, substitute the
/// member's zero-based index in place of any `__iota__` placeholder
/// in the value expression, then strip the group decl. After this
/// pass runs, `resolve_const_references` sees a flat sequence of
/// individual const decls and treats every group member identically
/// to a standalone `const NAME: T = literal`.
///
/// Group-marker shape (set up by the `const_group` grammar rule):
///   `__blinc_const_group__` function with body =
///     `[Expression(Call(__blinc_const_group_member__,
///                       [StringLiteral(name), value_expr])), …]`
///
/// Iota encoding: `iota` in the grammar lowers to
/// `StringLiteral("__iota__")`. This pass swaps it for an
/// `IntLiteral(index)`. Mixed iota-and-explicit-value members in
/// the same group are supported — only the iota placeholders get
/// substituted; explicit literals pass through unchanged.
///
/// MUST run before [`resolve_const_references`] so the hoisted
/// `__blinc_const__` markers are visible when references are
/// resolved.
pub(crate) fn expand_const_groups(program: &mut TypedProgram) {
    use zyntax_typed_ast::TypedNode;
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedExpression, TypedLiteral};

    fn substitute_iota(expr: &mut TypedNode<TypedExpression>, index: i128) {
        if let TypedExpression::Literal(TypedLiteral::String(s)) = &expr.node
            && let Some(s_arc) = s.resolve_global()
        {
            let s_str: &str = &s_arc;
            if s_str == "__iota__" {
                expr.node = TypedExpression::Literal(TypedLiteral::Integer(index));
                expr.ty = Type::Primitive(PrimitiveType::I32);
                return;
            }
        }
        // Recurse for completeness — iota always sits at the top of
        // the member's value expression today, but future arithmetic
        // (`iota + 1`) would need the descent.
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                substitute_iota(&mut b.left, index);
                substitute_iota(&mut b.right, index);
            }
            TypedExpression::Unary(u) => substitute_iota(&mut u.operand, index),
            TypedExpression::Call(c) => {
                substitute_iota(&mut c.callee, index);
                for a in &mut c.positional_args {
                    substitute_iota(a, index);
                }
            }
            _ => {}
        }
    }

    // Step 1: collect hoisted const decls from each group, recording
    // both the group index (for the diagnostic span hint below) and
    // the spliced const-marker decl. Drop the group markers from the
    // program in the same pass.
    let mut hoisted: Vec<TypedNode<TypedDeclaration>> = Vec::new();
    program.declarations.retain(|decl| {
        let TypedDeclaration::Function(func) = &decl.node else {
            return true;
        };
        if func.name.resolve_global().as_deref() != Some("__blinc_const_group__") {
            return true;
        }
        let Some(body) = &func.body else {
            return false;
        };
        for (index, stmt) in body.statements.iter().enumerate() {
            let TypedStatement::Expression(call_expr) = &stmt.node else {
                continue;
            };
            let TypedExpression::Call(call) = &call_expr.node else {
                continue;
            };
            let TypedExpression::Variable(callee) = &call.callee.node else {
                continue;
            };
            if callee.resolve_global().as_deref() != Some("__blinc_const_group_member__") {
                continue;
            }
            if call.positional_args.len() != 2 {
                continue;
            }
            let TypedExpression::Literal(TypedLiteral::String(name)) =
                &call.positional_args[0].node
            else {
                continue;
            };
            let Some(name_arc) = name.resolve_global() else {
                continue;
            };
            let mut value = call.positional_args[1].clone();
            substitute_iota(&mut value, index as i128);

            // Synthesise a `__blinc_const__` marker decl with the
            // same body shape `resolve_const_references` expects.
            let const_func = zyntax_typed_ast::typed_ast::TypedFunction {
                name: zyntax_typed_ast::InternedString::new_global("__blinc_const__"),
                annotations: Vec::new(),
                effects: Vec::new(),
                with_handlers: Vec::new(),
                type_params: Vec::new(),
                params: Vec::new(),
                return_type: Type::Any,
                body: Some(zyntax_typed_ast::typed_ast::TypedBlock {
                    statements: vec![
                        TypedNode::new(
                            TypedStatement::Expression(Box::new(TypedNode::new(
                                TypedExpression::Literal(TypedLiteral::String(*name)),
                                Type::Primitive(PrimitiveType::String),
                                decl.span,
                            ))),
                            Type::Primitive(PrimitiveType::Unit),
                            decl.span,
                        ),
                        TypedNode::new(
                            TypedStatement::Expression(Box::new(value)),
                            Type::Primitive(PrimitiveType::Unit),
                            decl.span,
                        ),
                    ],
                    span: decl.span,
                }),
                visibility: zyntax_typed_ast::type_registry::Visibility::Private,
                is_async: false,
                is_pure: false,
                is_external: false,
                calling_convention: Default::default(),
                link_name: None,
            };
            let _ = name_arc;
            hoisted.push(TypedNode::new(
                TypedDeclaration::Function(const_func),
                Type::Primitive(PrimitiveType::Unit),
                decl.span,
            ));
        }
        false
    });

    program.declarations.extend(hoisted);
}

/// Extract every `__blinc_const__` marker function, register the
/// declared constants into a `name → literal-expression` map, strip
/// the markers, then rewrite every `TypedExpression::Variable`
/// reference whose name matches a registered const to a clone of the
/// stored literal. This is how `const PI: f64 = 3.14159` followed by
/// a downstream `text(f"{PI}")` reads as if the literal had been
/// inlined at the call site.
///
/// MVP scope: const values are single literal tokens (int / float /
/// string / bool — see `const_literal` in the grammar). No arithmetic,
/// no references to other consts on the RHS. The declared type
/// annotation is informational only — the substituted expression
/// carries the literal's own type.
///
/// Must run before any pass that walks expressions for symbol
/// resolution (`resolve_signal_calls`, the FSM passes, etc.) so the
/// rewritten literals look identical to author-written ones.
pub(crate) fn resolve_const_references(program: &mut TypedProgram) {
    use std::collections::HashMap;
    use zyntax_typed_ast::TypedNode;
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedExpression, TypedLiteral};

    // Step 1: collect every `__blinc_const__` decl into a name→value
    // map, then strip those decls from the program. The marker
    // function's body is `[Expression(StringLiteral(name)),
    // Expression(<value-literal>)]` — see `const_decl` in
    // `grammar/blinc.zyn`.
    let mut consts: HashMap<String, TypedNode<TypedExpression>> = HashMap::new();
    program.declarations.retain(|decl| {
        let TypedDeclaration::Function(func) = &decl.node else {
            return true;
        };
        if func.name.resolve_global().as_deref() != Some("__blinc_const__") {
            return true;
        }
        let Some(body) = &func.body else {
            return false;
        };
        if body.statements.len() < 2 {
            return false;
        }
        let TypedStatement::Expression(name_expr) = &body.statements[0].node else {
            return false;
        };
        let TypedExpression::Literal(TypedLiteral::String(name)) = &name_expr.node else {
            return false;
        };
        let Some(name_str) = name.resolve_global() else {
            return false;
        };
        let TypedStatement::Expression(value_expr) = &body.statements[1].node else {
            return false;
        };
        consts.insert(name_str.to_string(), (**value_expr).clone());
        false
    });

    if consts.is_empty() {
        return;
    }

    // Step 2: rewrite every `Variable(name)` whose `name` is a
    // registered const to a clone of the stored literal. Recurses
    // through all expression / statement shapes that the existing
    // passes touch.
    fn rewrite_expr(
        expr: &mut TypedNode<TypedExpression>,
        consts: &HashMap<String, TypedNode<TypedExpression>>,
    ) {
        if let TypedExpression::Variable(name) = &expr.node
            && let Some(name_str) = name.resolve_global()
        {
            let key: &str = &name_str;
            if let Some(value) = consts.get(key) {
                *expr = value.clone();
                return;
            }
        }
        // Bare-name uppercase identifiers (`PI`, `ANSWER`, etc.) parse
        // as `Call(__component_call__, [StringLiteral("PI")])` via the
        // `component_call_bare` grammar alternative — capitalised names
        // are claimed by the component-call path before
        // `variable_expr`. Detect that shape too so consts named in
        // the conventional UPPERCASE style still substitute.
        if let TypedExpression::Call(call) = &expr.node
            && let TypedExpression::Variable(callee) = &call.callee.node
            && callee.resolve_global().as_deref() == Some("__component_call__")
            && call.positional_args.len() == 1
            && let TypedExpression::Literal(TypedLiteral::String(name)) =
                &call.positional_args[0].node
            && let Some(name_str) = name.resolve_global()
        {
            let key: &str = &name_str;
            if let Some(value) = consts.get(key) {
                *expr = value.clone();
                return;
            }
        }
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, consts);
                rewrite_expr(&mut b.right, consts);
            }
            TypedExpression::Unary(u) => {
                rewrite_expr(&mut u.operand, consts);
            }
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, consts);
                for a in &mut c.positional_args {
                    rewrite_expr(a, consts);
                }
                for na in &mut c.named_args {
                    rewrite_expr(&mut na.value, consts);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, consts);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, consts);
                }
            }
            TypedExpression::Field(f) => {
                rewrite_expr(&mut f.object, consts);
            }
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, consts);
                rewrite_expr(&mut idx.index, consts);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for item in items {
                    rewrite_expr(item, consts);
                }
            }
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, consts);
                rewrite_expr(&mut if_expr.then_branch, consts);
                rewrite_expr(&mut if_expr.else_branch, consts);
            }
            TypedExpression::Block(block) => {
                rewrite_block(block, consts);
            }
            TypedExpression::Lambda(lam) => match &mut lam.body {
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                    rewrite_expr(e, consts);
                }
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                    rewrite_block(block, consts);
                }
            },
            _ => {}
        }
    }

    fn rewrite_stmt(
        stmt: &mut TypedNode<TypedStatement>,
        consts: &HashMap<String, TypedNode<TypedExpression>>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, consts),
            TypedStatement::Return(Some(e)) => rewrite_expr(e, consts),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, consts);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, consts);
                rewrite_block(&mut if_stmt.then_block, consts);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, consts);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, consts);
                rewrite_block(&mut w.body, consts);
            }
            TypedStatement::Block(b) => {
                rewrite_block(b, consts);
            }
            _ => {}
        }
    }

    fn rewrite_block(
        block: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        consts: &HashMap<String, TypedNode<TypedExpression>>,
    ) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, consts);
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_block(body, &consts);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_block(body, &consts);
                    }
                }
            }
            _ => {}
        }
    }
}

/// stable `SignalId.to_raw()` minted via the process-global signal
/// registry. Lives at module scope so the inner helper `fn` items in
/// [`resolve_signal_calls`] can name the type in their signatures.
#[derive(Clone)]
struct SignalEntry {
    ty: Type,
    /// `SignalId.to_raw()` cast to i64 — Cranelift's value-map population
    /// doesn't handle `HirConstant::U64`, so we stay in i64-land.
    id_raw: i64,
}

pub(crate) fn resolve_signal_calls(program: &mut TypedProgram) {
    use std::collections::HashMap;
    use zyntax_typed_ast::InternedString;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};

    // Step 1: collect signal name → (type, id_raw). The id_raw is
    // minted on first encounter via the process-global
    // `blinc_dsl_core::signal_registry` — that calls
    // `blinc_core::reactive::signal(default)` and caches the resulting
    // `SignalId.to_raw()`. Subsequent compiles of the same source reuse
    // the existing id.
    //
    // `SignalEntry` declared above this fn so the helper `fn` items
    // (`rewrite_expr`, `rewrite_block`, `rewrite_stmt`) can name the
    // type in their signatures without lifting it to module scope.
    let mut signals: HashMap<InternedString, SignalEntry> = HashMap::new();
    for decl in &program.declarations {
        let TypedDeclaration::Function(func) = &decl.node else {
            continue;
        };
        if !is_signal_decl(func) {
            continue;
        }
        let Type::Primitive(prim) = &func.return_type else {
            continue;
        };
        let sig_ty = match prim {
            PrimitiveType::I32 => blinc_runtime::signal::SignalType::I32,
            PrimitiveType::F64 => blinc_runtime::signal::SignalType::F64,
            PrimitiveType::String => blinc_runtime::signal::SignalType::String,
            _ => continue,
        };
        let Some(name_str) = func.name.resolve_global() else {
            continue;
        };
        let id_raw_u64 = blinc_runtime::signal::mint_or_get(name_str.as_ref(), sig_ty);
        signals.insert(
            func.name,
            SignalEntry {
                ty: func.return_type.clone(),
                // i64 over the wire — the Cranelift backend lacks a
                // `HirConstant::U64` case in its value_map population
                // (see commit 54dc831b for context). Re-cast back to
                // u64 inside the extern.
                id_raw: id_raw_u64 as i64,
            },
        );
    }

    if signals.is_empty() {
        return;
    }

    // Step 2: rewrite `<sig>.get()` → `__signal_get_by_id_<T>(<id_literal>)`.
    fn typed_signal_extern_name(ty: &Type) -> Option<&'static str> {
        match ty {
            Type::Primitive(PrimitiveType::I32) => Some("__signal_get_by_id_i32"),
            Type::Primitive(PrimitiveType::F64) => Some("__signal_get_by_id_f64"),
            Type::Primitive(PrimitiveType::String) => Some("__signal_get_by_id_string"),
            _ => None,
        }
    }

    fn typed_signal_setter_extern_name(ty: &Type) -> Option<&'static str> {
        match ty {
            Type::Primitive(PrimitiveType::I32) => Some("__signal_set_by_id_i32"),
            Type::Primitive(PrimitiveType::F64) => Some("__signal_set_by_id_f64"),
            Type::Primitive(PrimitiveType::String) => Some("__signal_set_by_id_string"),
            _ => None,
        }
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        signals: &HashMap<InternedString, SignalEntry>,
    ) {
        // MUST intercept `<signal> = <expr>` BEFORE the recursive walk — the
        // LHS `Variable` doesn't otherwise trigger a rewrite.
        if let TypedExpression::Binary(b) = &expr.node
            && b.op == zyntax_typed_ast::typed_ast::BinaryOp::Assign
            && let TypedExpression::Variable(name) = &b.left.node
            && let Some(entry) = signals.get(name).cloned()
            && let Some(setter) = typed_signal_setter_extern_name(&entry.ty)
        {
            // Rewrite RHS first so nested signal reads route through getters.
            let mut rhs = (*b.right).clone();
            rewrite_expr(&mut rhs, signals);

            let id_arg = zyntax_typed_ast::TypedNode::new(
                TypedExpression::Literal(TypedLiteral::Integer(entry.id_raw as i128)),
                Type::Primitive(PrimitiveType::I64),
                expr.span,
            );
            let callee = zyntax_typed_ast::TypedNode::new(
                TypedExpression::Variable(InternedString::new_global(setter)),
                Type::Unknown,
                expr.span,
            );
            expr.node = TypedExpression::Call(TypedCall {
                callee: Box::new(callee),
                positional_args: vec![id_arg, rhs],
                named_args: vec![],
                type_args: vec![],
            });
            expr.ty = Type::Primitive(PrimitiveType::Unit);
            return;
        }

        // Children first so nested signal calls (e.g. `text(count.get())`) are rewritten.
        // EXCEPTION: MethodCall.receiver and Call+Field.object aren't walked
        // when they're a bare `Variable(<signal>)` — the dedicated
        // `count.get()` / `count.set(...)` rewrite below needs to see the
        // receiver as a Variable, not as a pre-rewritten getter-Call.
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, signals);
                rewrite_expr(&mut b.right, signals);
            }
            TypedExpression::Unary(u) => {
                rewrite_expr(&mut u.operand, signals);
            }
            TypedExpression::Call(c) => {
                // If the callee is `Field { object: Variable(<signal>), ... }`,
                // skip rewriting the object so the post-walk MethodCall/Call+Field
                // handler can match `<signal>.<method>(args)`. Args are still walked.
                let preserve_callee = matches!(
                    &c.callee.node,
                    TypedExpression::Field(f)
                        if matches!(
                            &f.object.node,
                            TypedExpression::Variable(n) if signals.contains_key(n)
                        )
                );
                if !preserve_callee {
                    rewrite_expr(&mut c.callee, signals);
                }
                for a in &mut c.positional_args {
                    rewrite_expr(a, signals);
                }
            }
            TypedExpression::Field(f) => {
                rewrite_expr(&mut f.object, signals);
            }
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, signals);
                rewrite_expr(&mut idx.index, signals);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for item in items {
                    rewrite_expr(item, signals);
                }
            }
            TypedExpression::MethodCall(mc) => {
                let preserve_receiver = matches!(
                    &mc.receiver.node,
                    TypedExpression::Variable(n) if signals.contains_key(n)
                );
                if !preserve_receiver {
                    rewrite_expr(&mut mc.receiver, signals);
                }
                for a in &mut mc.positional_args {
                    rewrite_expr(a, signals);
                }
            }
            TypedExpression::Block(block) => {
                rewrite_block(block, signals);
            }
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, signals);
                rewrite_expr(&mut if_expr.then_branch, signals);
                rewrite_expr(&mut if_expr.else_branch, signals);
            }
            TypedExpression::Lambda(lam) => match &mut lam.body {
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                    rewrite_expr(e, signals);
                }
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                    rewrite_block(block, signals);
                }
            },
            _ => {}
        }

        // `.get()` / `.set(x)` lands in two AST shapes:
        //   1. `MethodCall` — expression position (postfix-expr).
        //   2. `Call { callee: Field { ... }, ... }` — statement position.
        // Recognise both.
        let method_call = match &expr.node {
            TypedExpression::MethodCall(mc) => {
                if let TypedExpression::Variable(receiver_name) = &mc.receiver.node {
                    Some((
                        *receiver_name,
                        mc.method,
                        mc.positional_args.clone(),
                        expr.span,
                    ))
                } else {
                    None
                }
            }
            TypedExpression::Call(c) => {
                if let TypedExpression::Field(f) = &c.callee.node {
                    if let TypedExpression::Variable(receiver_name) = &f.object.node {
                        Some((
                            *receiver_name,
                            f.field,
                            c.positional_args.clone(),
                            expr.span,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        let Some((receiver_name, method, args, span)) = method_call else {
            // Bare `Variable(<signal>)` read — not `.get()`, not an
            // Assign LHS (those returned at the top). Rewrite to a
            // getter call so the JIT issues an actual signal load
            // instead of treating the name as an undefined local.
            //
            // SCOPE: only `__fsm_ctx_*` (FSM context-field) signals.
            // User-declared signals are left as bare Variables because
            // `lower_styling_args_to_overlays` (which runs after us)
            // pattern-matches on bare-Variable args to widget props
            // and rewrites them to the `_signal__` overlay variant for
            // LIVE binding at paint time. Forcing a getter call here
            // would freeze the value at compile time and break that
            // feature for `Div(bg = bg_color)`-style code.
            //
            // For ctx-signals the force-wrap is still required —
            // action bodies use them in arithmetic (`ctx.pct + 0.1`)
            // and f-string interpolation. The styling-args pass has
            // its own recognizer for the wrapped
            // `__signal_get_by_id_<T>(id_literal)` shape so
            // `Div(opacity = Ticker.pct)` still binds live.
            if let TypedExpression::Variable(name) = &expr.node
                && let Some(entry) = signals.get(name).cloned()
                && name
                    .resolve_global()
                    .map(|s| s.starts_with("__fsm_ctx_"))
                    .unwrap_or(false)
                && let Some(extern_name) = typed_signal_extern_name(&entry.ty)
            {
                let span = expr.span;
                expr.node = TypedExpression::Call(TypedCall {
                    callee: Box::new(zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Variable(InternedString::new_global(extern_name)),
                        Type::Unknown,
                        span,
                    )),
                    positional_args: vec![zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Literal(TypedLiteral::Integer(entry.id_raw as i128)),
                        Type::Primitive(PrimitiveType::I64),
                        span,
                    )],
                    named_args: vec![],
                    type_args: vec![],
                });
                expr.ty = entry.ty;
            }
            return;
        };
        let Some(entry) = signals.get(&receiver_name).cloned() else {
            return;
        };
        let method_name = method.resolve_global().map(|s| s.to_string());
        match method_name.as_deref() {
            // `count.get()` — read. Zero args, returns the
            // signal's value type.
            Some("get") if args.is_empty() => {
                let Some(extern_name) = typed_signal_extern_name(&entry.ty) else {
                    return;
                };
                expr.node = TypedExpression::Call(TypedCall {
                    callee: Box::new(zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Variable(InternedString::new_global(extern_name)),
                        Type::Unknown,
                        span,
                    )),
                    positional_args: vec![zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Literal(TypedLiteral::Integer(entry.id_raw as i128)),
                        Type::Primitive(PrimitiveType::I64),
                        span,
                    )],
                    named_args: vec![],
                    type_args: vec![],
                });
                expr.ty = entry.ty;
            }
            // `count.set(value)` — write. Arg already child-rewritten.
            Some("set") if args.len() == 1 => {
                let Some(setter) = typed_signal_setter_extern_name(&entry.ty) else {
                    return;
                };
                let value = args.into_iter().next().expect("len == 1 just checked");
                expr.node = TypedExpression::Call(TypedCall {
                    callee: Box::new(zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Variable(InternedString::new_global(setter)),
                        Type::Unknown,
                        span,
                    )),
                    positional_args: vec![
                        zyntax_typed_ast::TypedNode::new(
                            TypedExpression::Literal(TypedLiteral::Integer(entry.id_raw as i128)),
                            Type::Primitive(PrimitiveType::I64),
                            span,
                        ),
                        value,
                    ],
                    named_args: vec![],
                    type_args: vec![],
                });
                expr.ty = Type::Primitive(PrimitiveType::Unit);
            }
            _ => {}
        }
    }

    fn rewrite_block(
        block: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        signals: &HashMap<InternedString, SignalEntry>,
    ) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, signals);
        }
    }

    fn rewrite_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        signals: &HashMap<InternedString, SignalEntry>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, signals),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, signals);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, signals),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, signals);
                rewrite_block(&mut if_stmt.then_block, signals);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, signals);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, signals);
                rewrite_block(&mut w.body, signals);
            }
            TypedStatement::Block(b) => rewrite_block(b, signals),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        let TypedDeclaration::Function(func) = &mut decl.node else {
            continue;
        };
        if let Some(body) = &mut func.body {
            rewrite_block(body, &signals);
        }
    }
    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        for method in &mut imp.methods {
            if let Some(body) = &mut method.body {
                rewrite_block(body, &signals);
            }
        }
    }

    // Step 3: strip signal-marker decls (metadata only; usage was rewritten above).
    program.declarations.retain(|decl| {
        let TypedDeclaration::Function(func) = &decl.node else {
            return true;
        };
        !is_signal_decl(func)
    });
}

/// Rewrite `<FsmName>.trigger(<path>)` → `__fsm_runtime_trigger__("<FsmName>", <path>)`.
///
/// Two sources of "this is a known FSM" are checked at each call
/// site:
///
/// 1. **Local impls in this program** (same-file FSMs): collected
///    up-front into `fsm_names` from `__fsm_meta__`-bearing impls.
/// 2. **Global `FsmRegistry`** (cross-file FSMs imported from
///    previously-compiled modules under the same `module` key):
///    queried per call site when the receiver's name doesn't match
///    a local entry. Lets `MyFsm.trigger("Idle.Start")` in main.blinc
///    resolve to a `MyFsm` (or its module-mangled form like
///    `alpha$MyFsm` after import-rewrite) declared in alpha.blinc.
///
/// Without (2) the early-return at the top of this pass would
/// bail when the entry program has no local FSM impls, leaving
/// cross-file trigger calls unresolved at run time.
pub(crate) fn resolve_fsm_trigger_calls(
    program: &mut TypedProgram,
    module: zyntax_typed_ast::InternedString,
) {
    use std::collections::HashSet;
    use zyntax_typed_ast::InternedString;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};

    // Step 1: collect declared FSM names from `__fsm_meta__`-bearing impls.
    let mut fsm_names: HashSet<InternedString> = HashSet::new();
    for decl in &program.declarations {
        if let TypedDeclaration::Impl(imp) = &decl.node
            && imp.trait_name.resolve_global().is_some()
            && imp
                .methods
                .iter()
                .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
        {
            fsm_names.insert(imp.trait_name);
        }
    }
    // NOTE: don't early-return on `fsm_names.is_empty()` — the entry
    // program in a multi-file project may declare zero local FSMs
    // but still reference imported ones. The per-call global-
    // registry lookup below covers that case.

    // Visitor wrapper keeps `fsm_names` + `module` in scope across
    // the recursive rewrite without threading them through every
    // helper signature.
    struct Rewriter<'a> {
        fsm_names: &'a HashSet<InternedString>,
        module: InternedString,
    }

    impl Rewriter<'_> {
        fn rewrite_expr(&self, expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>) {
            // Recurse children first.
            match &mut expr.node {
                TypedExpression::Binary(b) => {
                    self.rewrite_expr(&mut b.left);
                    self.rewrite_expr(&mut b.right);
                }
                TypedExpression::Unary(u) => self.rewrite_expr(&mut u.operand),
                TypedExpression::Call(c) => {
                    self.rewrite_expr(&mut c.callee);
                    for a in &mut c.positional_args {
                        self.rewrite_expr(a);
                    }
                }
                TypedExpression::Field(f) => self.rewrite_expr(&mut f.object),
                TypedExpression::Index(idx) => {
                    self.rewrite_expr(&mut idx.object);
                    self.rewrite_expr(&mut idx.index);
                }
                TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                    for it in items {
                        self.rewrite_expr(it);
                    }
                }
                TypedExpression::MethodCall(mc) => {
                    self.rewrite_expr(&mut mc.receiver);
                    for a in &mut mc.positional_args {
                        self.rewrite_expr(a);
                    }
                }
                TypedExpression::Block(b) => self.rewrite_block(b),
                TypedExpression::If(if_expr) => {
                    self.rewrite_expr(&mut if_expr.condition);
                    self.rewrite_expr(&mut if_expr.then_branch);
                    self.rewrite_expr(&mut if_expr.else_branch);
                }
                TypedExpression::Lambda(lam) => match &mut lam.body {
                    zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                        self.rewrite_expr(e);
                    }
                    zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                        self.rewrite_block(block);
                    }
                },
                _ => {}
            }
            self.try_rewrite_trigger(expr);
        }

        fn try_rewrite_trigger(&self, expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>) {
            // Match `<FsmName>.trigger(<arg>)` in both AST shapes (MethodCall / Call+Field).
            let trigger_call = match &expr.node {
                TypedExpression::MethodCall(mc) if mc.positional_args.len() == 1 => {
                    if let TypedExpression::Variable(receiver_name) = &mc.receiver.node {
                        Some((
                            *receiver_name,
                            mc.method,
                            mc.positional_args[0].clone(),
                            expr.span,
                        ))
                    } else {
                        None
                    }
                }
                TypedExpression::Call(c) if c.positional_args.len() == 1 => {
                    if let TypedExpression::Field(f) = &c.callee.node {
                        if let TypedExpression::Variable(receiver_name) = &f.object.node {
                            Some((
                                *receiver_name,
                                f.field,
                                c.positional_args[0].clone(),
                                expr.span,
                            ))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            };

            let Some((receiver_name, method, path_arg, span)) = trigger_call else {
                return;
            };
            if method.resolve_global().as_deref() != Some("trigger") {
                return;
            }
            // Local FSMs (current program) take precedence; cross-file
            // FSMs (previously-compiled modules) found in the global
            // registry are accepted second. Receiver names that match
            // neither leave the original `MethodCall` shape alone — the
            // type-checker / linker surfaces them as undefined later.
            if !self.fsm_names.contains(&receiver_name) {
                let Some(name_str_arc) = receiver_name.resolve_global() else {
                    return;
                };
                let name_str: &str = &name_str_arc;
                let found_in_global = crate::fsm_registry::with_fsm_registry(|r| {
                    r.find_by_name(self.module, name_str).is_some()
                });
                if !found_in_global {
                    return;
                }
            }

            let fsm_name_arg = zyntax_typed_ast::TypedNode::new(
                TypedExpression::Literal(TypedLiteral::String(receiver_name)),
                Type::Primitive(PrimitiveType::String),
                span,
            );
            let callee = zyntax_typed_ast::TypedNode::new(
                TypedExpression::Variable(InternedString::new_global("__fsm_runtime_trigger__")),
                Type::Unknown,
                span,
            );
            expr.node = TypedExpression::Call(TypedCall {
                callee: Box::new(callee),
                positional_args: vec![fsm_name_arg, path_arg],
                named_args: vec![],
                type_args: vec![],
            });
            expr.ty = Type::Primitive(PrimitiveType::Unit);
        }

        fn rewrite_block(&self, block: &mut zyntax_typed_ast::typed_ast::TypedBlock) {
            for stmt in &mut block.statements {
                self.rewrite_stmt(stmt);
            }
        }

        fn rewrite_stmt(&self, stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>) {
            match &mut stmt.node {
                TypedStatement::Expression(e) => self.rewrite_expr(e),
                TypedStatement::Let(l) => {
                    if let Some(init) = &mut l.initializer {
                        self.rewrite_expr(init);
                    }
                }
                TypedStatement::Return(Some(e)) => self.rewrite_expr(e),
                TypedStatement::If(if_stmt) => {
                    self.rewrite_expr(&mut if_stmt.condition);
                    self.rewrite_block(&mut if_stmt.then_block);
                    if let Some(else_block) = &mut if_stmt.else_block {
                        self.rewrite_block(else_block);
                    }
                }
                TypedStatement::While(w) => {
                    self.rewrite_expr(&mut w.condition);
                    self.rewrite_block(&mut w.body);
                }
                TypedStatement::Block(b) => self.rewrite_block(b),
                _ => {}
            }
        }
    }

    let rewriter = Rewriter {
        fsm_names: &fsm_names,
        module,
    };

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewriter.rewrite_block(body);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewriter.rewrite_block(body);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Rewrite `<FsmName>.subscribe(<path>, <closure>)` →
/// `__fsm_subscribe__("<FsmName>", <path>, <closure>)`. Path filtering happens
/// host-side in `blinc_runtime::fsm::register_subscriber`.
///
/// Same two-source resolution as [`resolve_fsm_trigger_calls`] —
/// local impls AND global-registry imports both count. See that
/// doc for the rationale.
pub(crate) fn resolve_fsm_subscribe_calls(
    program: &mut TypedProgram,
    module: zyntax_typed_ast::InternedString,
) {
    use std::collections::HashSet;
    use zyntax_typed_ast::InternedString;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};

    let mut fsm_names: HashSet<InternedString> = HashSet::new();
    for decl in &program.declarations {
        if let TypedDeclaration::Impl(imp) = &decl.node
            && imp.trait_name.resolve_global().is_some()
            && imp
                .methods
                .iter()
                .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
        {
            fsm_names.insert(imp.trait_name);
        }
    }
    // Don't early-return on empty local set — cross-file FSMs found
    // via the global registry are still resolvable.

    struct Rewriter<'a> {
        fsm_names: &'a HashSet<InternedString>,
        module: InternedString,
    }

    impl Rewriter<'_> {
        fn rewrite_expr(&self, expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>) {
            match &mut expr.node {
                TypedExpression::Binary(b) => {
                    self.rewrite_expr(&mut b.left);
                    self.rewrite_expr(&mut b.right);
                }
                TypedExpression::Unary(u) => self.rewrite_expr(&mut u.operand),
                TypedExpression::Call(c) => {
                    self.rewrite_expr(&mut c.callee);
                    for a in &mut c.positional_args {
                        self.rewrite_expr(a);
                    }
                }
                TypedExpression::Field(f) => self.rewrite_expr(&mut f.object),
                TypedExpression::Index(idx) => {
                    self.rewrite_expr(&mut idx.object);
                    self.rewrite_expr(&mut idx.index);
                }
                TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                    for it in items {
                        self.rewrite_expr(it);
                    }
                }
                TypedExpression::MethodCall(mc) => {
                    self.rewrite_expr(&mut mc.receiver);
                    for a in &mut mc.positional_args {
                        self.rewrite_expr(a);
                    }
                }
                TypedExpression::Block(b) => self.rewrite_block(b),
                TypedExpression::If(if_expr) => {
                    self.rewrite_expr(&mut if_expr.condition);
                    self.rewrite_expr(&mut if_expr.then_branch);
                    self.rewrite_expr(&mut if_expr.else_branch);
                }
                TypedExpression::Lambda(lam) => match &mut lam.body {
                    zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                        self.rewrite_expr(e);
                    }
                    zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                        self.rewrite_block(block);
                    }
                },
                _ => {}
            }
            self.try_rewrite_subscribe(expr);
        }

        fn try_rewrite_subscribe(&self, expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>) {
            // Two shapes:
            //   * 2-arg `subscribe(path, closure)` → `__fsm_subscribe__(name, path, closure)`
            //   * 1-arg `subscribe(closure)`       → `__fsm_subscribe_all__(name, closure)`
            // Both ASTs (MethodCall / Call+Field) carry through. Tuple is
            // `(receiver, method, args:Vec<expr>, span)` where the args
            // vector's length distinguishes the two forms.
            let subscribe_call = match &expr.node {
                TypedExpression::MethodCall(mc) => {
                    if let TypedExpression::Variable(receiver_name) = &mc.receiver.node {
                        Some((
                            *receiver_name,
                            mc.method,
                            mc.positional_args.clone(),
                            expr.span,
                        ))
                    } else {
                        None
                    }
                }
                TypedExpression::Call(c) => {
                    if let TypedExpression::Field(f) = &c.callee.node {
                        if let TypedExpression::Variable(receiver_name) = &f.object.node {
                            Some((
                                *receiver_name,
                                f.field,
                                c.positional_args.clone(),
                                expr.span,
                            ))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            };

            let Some((receiver_name, method, args, span)) = subscribe_call else {
                return;
            };
            if method.resolve_global().as_deref() != Some("subscribe") {
                return;
            }
            if !self.fsm_names.contains(&receiver_name) {
                let Some(name_str_arc) = receiver_name.resolve_global() else {
                    return;
                };
                let name_str: &str = &name_str_arc;
                let found_in_global = crate::fsm_registry::with_fsm_registry(|r| {
                    r.find_by_name(self.module, name_str).is_some()
                });
                if !found_in_global {
                    return;
                }
            }

            let fsm_name_arg = zyntax_typed_ast::TypedNode::new(
                TypedExpression::Literal(TypedLiteral::String(receiver_name)),
                Type::Primitive(PrimitiveType::String),
                span,
            );

            match args.len() {
                2 => {
                    // Path-filtered form.
                    let path_arg = args[0].clone();
                    let closure_arg = args[1].clone();
                    let callee = zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Variable(InternedString::new_global("__fsm_subscribe__")),
                        Type::Unknown,
                        span,
                    );
                    expr.node = TypedExpression::Call(TypedCall {
                        callee: Box::new(callee),
                        positional_args: vec![fsm_name_arg, path_arg, closure_arg],
                        named_args: vec![],
                        type_args: vec![],
                    });
                    expr.ty = Type::Primitive(PrimitiveType::Unit);
                }
                1 => {
                    // All-paths form. The closure is a one-arg lambda
                    // whose body receives the matched `"From.Event"`
                    // path string each transition.
                    let closure_arg = args[0].clone();
                    let callee = zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Variable(InternedString::new_global(
                            "__fsm_subscribe_all__",
                        )),
                        Type::Unknown,
                        span,
                    );
                    expr.node = TypedExpression::Call(TypedCall {
                        callee: Box::new(callee),
                        positional_args: vec![fsm_name_arg, closure_arg],
                        named_args: vec![],
                        type_args: vec![],
                    });
                    expr.ty = Type::Primitive(PrimitiveType::Unit);
                }
                _ => {
                    // Wrong arity — leave the call shape alone; the
                    // type checker / linker surfaces the error.
                }
            }
        }

        fn rewrite_block(&self, block: &mut zyntax_typed_ast::typed_ast::TypedBlock) {
            for stmt in &mut block.statements {
                self.rewrite_stmt(stmt);
            }
        }

        fn rewrite_stmt(&self, stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>) {
            match &mut stmt.node {
                TypedStatement::Expression(e) => self.rewrite_expr(e),
                TypedStatement::Let(l) => {
                    if let Some(init) = &mut l.initializer {
                        self.rewrite_expr(init);
                    }
                }
                TypedStatement::Return(Some(e)) => self.rewrite_expr(e),
                TypedStatement::If(if_stmt) => {
                    self.rewrite_expr(&mut if_stmt.condition);
                    self.rewrite_block(&mut if_stmt.then_block);
                    if let Some(else_block) = &mut if_stmt.else_block {
                        self.rewrite_block(else_block);
                    }
                }
                TypedStatement::While(w) => {
                    self.rewrite_expr(&mut w.condition);
                    self.rewrite_block(&mut w.body);
                }
                TypedStatement::Block(b) => self.rewrite_block(b),
                _ => {}
            }
        }
    }

    let rewriter = Rewriter {
        fsm_names: &fsm_names,
        module,
    };

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewriter.rewrite_block(body);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewriter.rewrite_block(body);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Rename every user-component (Class + matching Impl) with the
/// module namespace prefix so cross-file declarations don't collide
/// in the JIT symbol table or the component registry.
///
/// Mangling shape: `Counter` declared in module `widgets` becomes
/// `widgets$Counter`. Multi-segment paths use `$` as the separator:
/// `ui/widgets.blinc` → `ui$widgets$Counter`. Matches Zyntax's
/// existing inherent-impl symbol convention (`Class$method`), so the
/// downstream `<Component>$view` symbol naturally lands as
/// `widgets$Counter$view` without any change in the symbol emitter.
///
/// Scope: ONLY user-declared components — a `Class` decl whose name
/// has a matching `Impl` decl pointing at it. Marker classes
/// (`__blinc_*`), structs without sibling impls, FSMs, the synthetic
/// `render_view` function, and substrate primitives are all left
/// untouched.
///
/// Side effects this pass handles atomically:
/// 1. `Class.name` is renamed.
/// 2. Every matching `Impl.for_type` (`Type::Unresolved(name)`
///    pre-resolution; `Type::Named { id, … }` post-resolution) is
///    repointed at the mangled name.
/// 3. Every `__component_call__("local_name")` marker call in the
///    program is rewritten to `__component_call__("mangled_name")`
///    so [`lower_component_calls`] resolves the callee against the
///    same mangled-name entry in the runtime component registry.
///
/// No-op when `namespace` is empty — single-file `compile_source` /
/// `compile_directory` paths keep emitting un-mangled symbols, so
/// existing tests that assert `Counter$view` stay green.
///
/// Cross-module reference rewriting (entry's `Counter()` →
/// `widgets$Counter()` for an import from `./widgets`) happens
/// separately in [`crate::BlincDsl::inject_imported_view_externs`]
/// because that hook already knows each import's source file.
pub(crate) fn apply_module_namespace_prefix(program: &mut TypedProgram, namespace: &str) {
    use std::collections::{HashMap, HashSet};
    use zyntax_typed_ast::InternedString;
    use zyntax_typed_ast::typed_ast::TypedDeclaration;

    if namespace.is_empty() {
        return;
    }

    // Step 1: identify mangleable top-level types. Two categories
    // get mangled:
    //
    // 1. Component classes — a `Class` decl whose name has a
    //    matching `Impl` decl pointing at it. Marker classes
    //    (`__blinc_*`) and structs without impls pass through
    //    un-mangled.
    //
    // 2. FSM state enums — an `Enum` decl whose name has a matching
    //    `Impl` decl whose `__fsm_meta__` marker method identifies
    //    it as an FSM. Same-named cross-file FSMs would otherwise
    //    collide in the global `FsmRegistry`.
    //
    // Both categories share one `to_mangle` map so the downstream
    // call-site rewrites can resolve same-file references against
    // either kind uniformly.
    let class_names: Vec<InternedString> = program
        .declarations
        .iter()
        .filter_map(|d| {
            if let TypedDeclaration::Class(c) = &d.node {
                Some(c.name)
            } else {
                None
            }
        })
        .collect();

    let enum_names: Vec<InternedString> = program
        .declarations
        .iter()
        .filter_map(|d| {
            if let TypedDeclaration::Enum(e) = &d.node {
                Some(e.name)
            } else {
                None
            }
        })
        .collect();

    let impl_targets: HashSet<InternedString> = program
        .declarations
        .iter()
        .filter_map(|d| {
            let TypedDeclaration::Impl(imp) = &d.node else {
                return None;
            };
            match &imp.for_type {
                Type::Unresolved(name) => Some(*name),
                Type::Named { id, .. } => program.type_registry.get_type_by_id(*id).map(|t| t.name),
                _ => None,
            }
        })
        .collect();

    // FSM-impl set: target names for impls that carry a `__fsm_meta__`
    // method. Used to discriminate "this enum is a state enum for an
    // FSM" from "this enum is a plain data enum the user declared".
    let fsm_impl_targets: HashSet<InternedString> = program
        .declarations
        .iter()
        .filter_map(|d| {
            let TypedDeclaration::Impl(imp) = &d.node else {
                return None;
            };
            let has_fsm_meta = imp
                .methods
                .iter()
                .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"));
            if !has_fsm_meta {
                return None;
            }
            match &imp.for_type {
                Type::Unresolved(name) => Some(*name),
                Type::Named { id, .. } => program.type_registry.get_type_by_id(*id).map(|t| t.name),
                _ => None,
            }
        })
        .collect();

    let mut to_mangle: HashMap<InternedString, InternedString> = HashMap::new();

    // Components.
    for name in class_names {
        let Some(name_str_arc) = name.resolve_global() else {
            continue;
        };
        let name_str: &str = &name_str_arc;
        if name_str.starts_with("__blinc_") || name_str.starts_with("__") {
            continue;
        }
        if !impl_targets.contains(&name) {
            continue;
        }
        let mangled_str = format!("{namespace}${name_str}");
        to_mangle.insert(name, InternedString::new_global(&mangled_str));
    }

    // FSMs.
    for name in enum_names {
        let Some(name_str_arc) = name.resolve_global() else {
            continue;
        };
        let name_str: &str = &name_str_arc;
        if name_str.starts_with("__blinc_") || name_str.starts_with("__") {
            continue;
        }
        if !fsm_impl_targets.contains(&name) {
            continue;
        }
        // Skip if already mangled (e.g., a name collision between a
        // component class and an FSM enum — pathological but defensive).
        if to_mangle.contains_key(&name) {
            continue;
        }
        let mangled_str = format!("{namespace}${name_str}");
        to_mangle.insert(name, InternedString::new_global(&mangled_str));
    }

    if to_mangle.is_empty() {
        return;
    }

    // Step 2: rename Class.name / Enum.name + every matching
    // Impl.for_type AND Impl.trait_name. FSM impls use the same
    // string for both trait_name (the inherent-impl convention) and
    // for_type — `populate_fsm_registry_pass` keys off trait_name,
    // so renaming both keeps the registry entry's identity in sync
    // with the type-level rename.
    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Class(c) => {
                if let Some(&new_name) = to_mangle.get(&c.name) {
                    c.name = new_name;
                }
            }
            TypedDeclaration::Enum(e) => {
                if let Some(&new_name) = to_mangle.get(&e.name) {
                    e.name = new_name;
                }
            }
            TypedDeclaration::Impl(imp) => {
                if let Some(&new_name) = to_mangle.get(&imp.trait_name) {
                    imp.trait_name = new_name;
                }
                match &mut imp.for_type {
                    Type::Unresolved(name) => {
                        if let Some(&new_name) = to_mangle.get(name) {
                            *name = new_name;
                        }
                    }
                    Type::Named { .. } => {
                        // Post-resolution shape — the type registry entry's
                        // own `name` is what `publish_components_to_runtime_registry`
                        // reads. The mangling pass runs pre-resolution so
                        // this arm is reached only if some earlier pass
                        // ran the type resolver; we conservatively skip
                        // it to avoid mutating the registry mid-pipeline.
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Step 3: rewrite same-file references to mangled names. Two
    // shapes share the rewrite helper:
    //
    // - Component calls: `__component_call__("Name", …)` markers
    //   (emitted by the `component_call_*` grammar rules) have
    //   their first string-literal arg rewritten.
    // - FSM receivers: `<FsmName>.trigger(...)` / `.subscribe(...)`
    //   parse as MethodCall whose receiver is a `Variable(FsmName)`.
    //   The helper walks receiver positions and rewrites the Variable
    //   name when it matches the mangled set.
    //
    // Cross-file references (imports) are handled separately in
    // `inject_imported_view_externs`, which calls into the same
    // helper with its own (import-local-name → mangled-name) map.
    rewrite_component_calls_in_program(program, &to_mangle);
}

/// Walk every function / impl body in `program` and rewrite every
/// `__component_call__("local", …)` marker call where `local` matches
/// a key in `rewrites` to `__component_call__("rewrites[local]", …)`.
/// Shared between [`apply_module_namespace_prefix`] (which uses it
/// for same-file component renames) and
/// [`crate::BlincDsl::inject_imported_view_externs`] (which uses it
/// for cross-file import renames). The rewrite never touches a
/// component's structural shape — only the leading string-literal
/// name arg.
pub(crate) fn rewrite_component_calls_in_program(
    program: &mut TypedProgram,
    rewrites: &std::collections::HashMap<
        zyntax_typed_ast::InternedString,
        zyntax_typed_ast::InternedString,
    >,
) {
    use std::collections::HashMap;
    use zyntax_typed_ast::InternedString;
    use zyntax_typed_ast::TypedNode;
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedExpression, TypedLiteral};

    fn rewrite_expr(
        expr: &mut TypedNode<TypedExpression>,
        rewrites: &HashMap<InternedString, InternedString>,
    ) {
        if let TypedExpression::Call(call) = &mut expr.node {
            // FSM trigger/subscribe shape via the
            // `method_call_stmt` grammar — `MyFsm.trigger(...)`
            // lowers to `Call(Field(Variable(MyFsm), trigger), …)`.
            // Rewrite the inner Variable to the mangled name before
            // recursing so the `resolve_fsm_*_calls` passes see the
            // mangled receiver. The downstream MethodCall arm below
            // handles the alternate `MethodCall { receiver: Variable,
            // method, args }` AST shape uniformly.
            if let TypedExpression::Field(f) = &mut call.callee.node
                && let TypedExpression::Variable(name) = &f.object.node
                && let Some(&new_name) = rewrites.get(name)
            {
                f.object.node = TypedExpression::Variable(new_name);
            }
            rewrite_expr(&mut call.callee, rewrites);
            if let TypedExpression::Variable(callee) = &call.callee.node
                && callee.resolve_global().as_deref() == Some("__component_call__")
                && let Some(name_arg) = call.positional_args.first_mut()
                && let TypedExpression::Literal(TypedLiteral::String(name)) = &name_arg.node
                && let Some(&new_name) = rewrites.get(name)
            {
                name_arg.node = TypedExpression::Literal(TypedLiteral::String(new_name));
            }
            for a in &mut call.positional_args {
                rewrite_expr(a, rewrites);
            }
            for na in &mut call.named_args {
                rewrite_expr(&mut na.value, rewrites);
            }
            return;
        }
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, rewrites);
                rewrite_expr(&mut b.right, rewrites);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand, rewrites),
            TypedExpression::Field(f) => rewrite_expr(&mut f.object, rewrites),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, rewrites);
                rewrite_expr(&mut idx.index, rewrites);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it, rewrites);
                }
            }
            TypedExpression::MethodCall(mc) => {
                // FSM receiver rewrite: `<FsmName>.trigger(...)` /
                // `.subscribe(...)` parse as MethodCall whose
                // receiver is a `Variable(FsmName)`. After mangling,
                // the local `FsmName` no longer matches the
                // registered FSM identity — rewrite the receiver
                // Variable's name to the mangled form here so the
                // downstream `resolve_fsm_trigger_calls` /
                // `resolve_fsm_subscribe_calls` passes resolve
                // against the same key as the registry.
                if let TypedExpression::Variable(name) = &mc.receiver.node
                    && let Some(&new_name) = rewrites.get(name)
                {
                    mc.receiver.node = TypedExpression::Variable(new_name);
                }
                rewrite_expr(&mut mc.receiver, rewrites);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, rewrites);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    rewrite_stmt(stmt, rewrites);
                }
            }
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, rewrites);
                rewrite_expr(&mut if_expr.then_branch, rewrites);
                rewrite_expr(&mut if_expr.else_branch, rewrites);
            }
            TypedExpression::Lambda(lam) => match &mut lam.body {
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                    rewrite_expr(e, rewrites);
                }
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                    for stmt in &mut block.statements {
                        rewrite_stmt(stmt, rewrites);
                    }
                }
            },
            _ => {}
        }
    }

    fn rewrite_stmt(
        stmt: &mut TypedNode<TypedStatement>,
        rewrites: &HashMap<InternedString, InternedString>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, rewrites),
            TypedStatement::Return(Some(e)) => rewrite_expr(e, rewrites),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, rewrites);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, rewrites);
                for s in &mut if_stmt.then_block.statements {
                    rewrite_stmt(s, rewrites);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for s in &mut else_block.statements {
                        rewrite_stmt(s, rewrites);
                    }
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, rewrites);
                for s in &mut w.body.statements {
                    rewrite_stmt(s, rewrites);
                }
            }
            TypedStatement::Block(b) => {
                for s in &mut b.statements {
                    rewrite_stmt(s, rewrites);
                }
            }
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    for stmt in &mut body.statements {
                        rewrite_stmt(stmt, rewrites);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        for stmt in &mut body.statements {
                            rewrite_stmt(stmt, rewrites);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Derive a module namespace from a file path relative to a source
/// root. Returns an empty string when `entry` isn't inside
/// `source_root` (defensive — single-file compile paths use the
/// empty-namespace branch in `apply_module_namespace_prefix`).
///
/// Shape: `widgets.blinc` → `"widgets"`,
/// `ui/widgets.blinc` → `"ui$widgets"`. Hyphens / dots inside path
/// segments survive; the `.blinc` extension is stripped.
pub(crate) fn module_namespace_from_path(entry: &Path, source_root: &Path) -> String {
    let rel = entry.strip_prefix(source_root).unwrap_or(entry);
    let mut segments: Vec<String> = Vec::new();
    for component in rel.components() {
        if let std::path::Component::Normal(os) = component
            && let Some(s) = os.to_str()
        {
            let stem = s.strip_suffix(".blinc").unwrap_or(s);
            if !stem.is_empty() {
                segments.push(stem.to_string());
            }
        }
    }
    segments.join("$")
}

/// Lower explicit `struct` constructor calls (`MyData(field = value)`) into
/// native Zyntax struct literals. Component and widget calls keep flowing
/// through the normal `__component_call__` path.
pub(crate) fn lower_struct_literals(program: &mut TypedProgram) -> Result<(), Vec<String>> {
    use std::collections::{HashMap, HashSet};
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedExpression, TypedFieldInit, TypedLiteral, TypedStructLiteral,
    };

    #[derive(Clone)]
    struct StructInfo {
        name: zyntax_typed_ast::InternedString,
        fields: Vec<zyntax_typed_ast::typed_ast::TypedField>,
    }

    fn marker_struct_name(decl: &zyntax_typed_ast::TypedNode<TypedDeclaration>) -> Option<String> {
        let TypedDeclaration::Function(func) = &decl.node else {
            return None;
        };
        if func.name.resolve_global().as_deref() != Some("__blinc_struct_type__") {
            return None;
        }
        let body = func.body.as_ref()?;
        let first = body.statements.first()?;
        let TypedStatement::Expression(expr) = &first.node else {
            return None;
        };
        let TypedExpression::Literal(TypedLiteral::String(name)) = &expr.node else {
            return None;
        };
        name.resolve_global().map(|s| s.to_string())
    }

    let explicit_structs: HashSet<String> = program
        .declarations
        .iter()
        .filter_map(marker_struct_name)
        .collect();

    if explicit_structs.is_empty() {
        return Ok(());
    }

    let mut structs: HashMap<String, StructInfo> = HashMap::new();
    for decl in &program.declarations {
        if let TypedDeclaration::Class(class) = &decl.node {
            let Some(name) = class.name.resolve_global() else {
                continue;
            };
            if explicit_structs.contains::<str>(name.as_ref()) {
                structs.insert(
                    name.to_string(),
                    StructInfo {
                        name: class.name,
                        fields: class.fields.clone(),
                    },
                );
            }
        }
    }

    program
        .declarations
        .retain(|decl| marker_struct_name(decl).is_none());

    let mut errors = Vec::new();

    fn named_marker_arg(
        arg: &zyntax_typed_ast::TypedNode<TypedExpression>,
    ) -> Option<(
        zyntax_typed_ast::InternedString,
        zyntax_typed_ast::TypedNode<TypedExpression>,
    )> {
        let TypedExpression::Call(inner) = &arg.node else {
            return None;
        };
        let TypedExpression::Variable(inner_callee) = &inner.callee.node else {
            return None;
        };
        if inner_callee.resolve_global().as_deref() != Some("__named__") {
            return None;
        }
        let [name_node, value_node] = inner.positional_args.as_slice() else {
            return None;
        };
        let TypedExpression::Literal(TypedLiteral::String(arg_name)) = &name_node.node else {
            return None;
        };
        Some((*arg_name, value_node.clone()))
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        structs: &HashMap<String, StructInfo>,
        errors: &mut Vec<String>,
    ) {
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, structs, errors);
                rewrite_expr(&mut b.right, structs, errors);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand, structs, errors),
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, structs, errors);
                for a in &mut c.positional_args {
                    rewrite_expr(a, structs, errors);
                }
                for n in &mut c.named_args {
                    rewrite_expr(&mut n.value, structs, errors);
                }
            }
            TypedExpression::Field(f) => rewrite_expr(&mut f.object, structs, errors),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, structs, errors);
                rewrite_expr(&mut idx.index, structs, errors);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it, structs, errors);
                }
            }
            TypedExpression::Struct(s) => {
                for field in &mut s.fields {
                    rewrite_expr(&mut field.value, structs, errors);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, structs, errors);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, structs, errors);
                }
            }
            TypedExpression::Block(b) => rewrite_block(b, structs, errors),
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, structs, errors);
                rewrite_expr(&mut if_expr.then_branch, structs, errors);
                rewrite_expr(&mut if_expr.else_branch, structs, errors);
            }
            _ => {}
        }

        let TypedExpression::Call(call) = &expr.node else {
            return;
        };
        let TypedExpression::Variable(callee_name) = &call.callee.node else {
            return;
        };
        if callee_name.resolve_global().as_deref() != Some("__component_call__") {
            return;
        }
        let Some(name_node) = call.positional_args.first() else {
            return;
        };
        let TypedExpression::Literal(TypedLiteral::String(type_name)) = &name_node.node else {
            return;
        };
        let Some(type_name_str) = type_name.resolve_global().map(|s| s.to_string()) else {
            return;
        };
        let Some(info) = structs.get(&type_name_str) else {
            return;
        };

        let mut values: HashMap<String, zyntax_typed_ast::TypedNode<TypedExpression>> =
            HashMap::new();
        let mut seen = HashSet::new();

        for arg in call.positional_args.iter().skip(1) {
            if matches!(arg.node, TypedExpression::Block(_)) {
                errors.push(format!(
                    "struct `{}` constructors do not accept child bodies; use `{}(field = value)`",
                    type_name_str, type_name_str
                ));
                continue;
            }

            let Some((field_name, value)) = named_marker_arg(arg) else {
                errors.push(format!(
                    "struct `{}` constructors require named fields, e.g. `{}(field = value)`",
                    type_name_str, type_name_str
                ));
                continue;
            };
            let Some(field_name_str) = field_name.resolve_global().map(|s| s.to_string()) else {
                continue;
            };
            if !seen.insert(field_name_str.clone()) {
                errors.push(format!(
                    "struct `{}` field `{}` is specified more than once",
                    type_name_str, field_name_str
                ));
                continue;
            }
            values.insert(field_name_str, value);
        }

        let declared: HashSet<String> = info
            .fields
            .iter()
            .filter_map(|f| f.name.resolve_global().map(|s| s.to_string()))
            .collect();
        for supplied in values.keys() {
            if !declared.contains(supplied) {
                errors.push(format!(
                    "struct `{}` has no field named `{}`",
                    type_name_str, supplied
                ));
            }
        }

        let mut lowered_fields = Vec::with_capacity(info.fields.len());
        for field in &info.fields {
            let Some(field_name) = field.name.resolve_global().map(|s| s.to_string()) else {
                continue;
            };
            let Some(value) = values.remove(&field_name) else {
                errors.push(format!(
                    "struct `{}` constructor is missing field `{}`",
                    type_name_str, field_name
                ));
                continue;
            };
            lowered_fields.push(TypedFieldInit {
                name: field.name,
                value: Box::new(value),
            });
        }

        if errors.is_empty() {
            expr.ty = Type::Unresolved(info.name);
            expr.node = TypedExpression::Struct(TypedStructLiteral {
                name: info.name,
                fields: lowered_fields,
            });
        }
    }

    fn rewrite_block(
        block: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        structs: &HashMap<String, StructInfo>,
        errors: &mut Vec<String>,
    ) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, structs, errors);
        }
    }

    fn rewrite_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        structs: &HashMap<String, StructInfo>,
        errors: &mut Vec<String>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, structs, errors),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, structs, errors);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, structs, errors),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, structs, errors);
                rewrite_block(&mut if_stmt.then_block, structs, errors);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, structs, errors);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, structs, errors);
                rewrite_block(&mut w.body, structs, errors);
            }
            TypedStatement::Block(b) => rewrite_block(b, structs, errors),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_block(body, &structs, &mut errors);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_block(body, &structs, &mut errors);
                    }
                }
            }
            _ => {}
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate every `__component_call__("Name", ...)` marker references a known
/// component. Catches typos before Zyntax's less-helpful unresolved-symbol error.
/// Does NOT rewrite markers — that contract is consumed by `lower_component_calls`.
pub(crate) fn validate_component_calls(program: &TypedProgram) -> Result<(), Vec<String>> {
    use std::collections::HashSet;
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedExpression, TypedLiteral};

    let mut known: HashSet<String> = HashSet::new();
    for decl in &program.declarations {
        if let TypedDeclaration::Class(c) = &decl.node
            && let Some(name) = c.name.resolve_global()
        {
            known.insert(name.to_string());
        }
        // Named imports — whitelist so the validator (pre import-resolution) doesn't flag them.
        if let TypedDeclaration::Import(import) = &decl.node {
            for item in &import.items {
                if let zyntax_typed_ast::TypedImportItem::Named { name, .. } = item
                    && let Some(s) = name.resolve_global()
                {
                    known.insert(s.to_string());
                }
            }
        }
    }
    // Pull pre-registered primitives (`Div`, `Text`, …) from the substrate registry.
    blinc_runtime::component::with_component_registry(|r| {
        for (_, def) in r.iter() {
            known.insert(def.name.as_ref().to_string());
        }
    });

    let mut errors: Vec<String> = Vec::new();

    fn check_expr(
        expr: &zyntax_typed_ast::TypedNode<TypedExpression>,
        known: &HashSet<String>,
        errors: &mut Vec<String>,
    ) {
        match &expr.node {
            TypedExpression::Binary(b) => {
                check_expr(&b.left, known, errors);
                check_expr(&b.right, known, errors);
            }
            TypedExpression::Unary(u) => check_expr(&u.operand, known, errors),
            TypedExpression::Call(c) => {
                check_expr(&c.callee, known, errors);
                for a in &c.positional_args {
                    check_expr(a, known, errors);
                }

                // Check `__component_call__("Name", ...)` against known set.
                // Namespaced calls (`cn.Button`) round-trip the same way the
                // call-site resolver does: dotted form coming out of the
                // grammar maps to the underscore-mangled registry key
                // (`cn_Button`). Check both so the dotted surface and the
                // mangled-key registration agree without an extra
                // duplicate entry.
                if let TypedExpression::Variable(callee_name) = &c.callee.node
                    && callee_name.resolve_global().as_deref() == Some("__component_call__")
                    && let Some(name_node) = c.positional_args.first()
                    && let TypedExpression::Literal(TypedLiteral::String(name)) = &name_node.node
                {
                    let name_str = name.resolve_global().unwrap_or_default();
                    let name_ref: &str = name_str.as_ref();
                    let mangled = name_ref.replace('.', "_");
                    if !known.contains(name_ref) && !known.contains(&mangled) {
                        errors.push(format!(
                            "unknown component `{}` — declare it with \
                                         `component {} {{ ... }}` before use",
                            name_str, name_str
                        ));
                    }
                }
            }
            TypedExpression::Field(f) => check_expr(&f.object, known, errors),
            TypedExpression::Index(idx) => {
                check_expr(&idx.object, known, errors);
                check_expr(&idx.index, known, errors);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    check_expr(it, known, errors);
                }
            }
            TypedExpression::Struct(s) => {
                for field in &s.fields {
                    check_expr(&field.value, known, errors);
                }
            }
            TypedExpression::MethodCall(mc) => {
                check_expr(&mc.receiver, known, errors);
                for a in &mc.positional_args {
                    check_expr(a, known, errors);
                }
            }
            TypedExpression::Block(b) => check_block(b, known, errors),
            TypedExpression::If(if_expr) => {
                check_expr(&if_expr.condition, known, errors);
                check_expr(&if_expr.then_branch, known, errors);
                check_expr(&if_expr.else_branch, known, errors);
            }
            _ => {}
        }
    }

    fn check_block(
        block: &zyntax_typed_ast::typed_ast::TypedBlock,
        known: &HashSet<String>,
        errors: &mut Vec<String>,
    ) {
        for stmt in &block.statements {
            check_stmt(stmt, known, errors);
        }
    }

    fn check_stmt(
        stmt: &zyntax_typed_ast::TypedNode<TypedStatement>,
        known: &HashSet<String>,
        errors: &mut Vec<String>,
    ) {
        match &stmt.node {
            TypedStatement::Expression(e) => check_expr(e, known, errors),
            TypedStatement::Let(l) => {
                if let Some(init) = &l.initializer {
                    check_expr(init, known, errors);
                }
            }
            TypedStatement::Return(Some(e)) => check_expr(e, known, errors),
            TypedStatement::If(if_stmt) => {
                check_expr(&if_stmt.condition, known, errors);
                check_block(&if_stmt.then_block, known, errors);
                if let Some(else_block) = &if_stmt.else_block {
                    check_block(else_block, known, errors);
                }
            }
            TypedStatement::While(w) => {
                check_expr(&w.condition, known, errors);
                check_block(&w.body, known, errors);
            }
            TypedStatement::Block(b) => check_block(b, known, errors),
            _ => {}
        }
    }

    for decl in &program.declarations {
        match &decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &func.body {
                    check_block(body, &known, &mut errors);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &imp.methods {
                    if let Some(body) = &method.body {
                        check_block(body, &known, &mut errors);
                    }
                }
            }
            _ => {}
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Stable per-call-site instance ID derived from `(filename, byte_offset)`.
/// Plain byte-offset hash. Used by tests + the simpler call sites where
/// component name + class info isn't readily available.
///
/// `DefaultHasher` (SipHash) is deterministic per process but not across
/// processes — that's fine here since instance IDs are scoped to a single
/// run of the JIT runtime.
pub(crate) fn call_site_instance_id(filename: &str, span_start: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    filename.hash(&mut h);
    span_start.hash(&mut h);
    h.finish()
}

/// Path-based per-call-site instance ID — incorporates the component
/// name, an optional CSS class (when present as a string-literal arg
/// at the call site), and the source-location offset. The path string
/// has the shape `ComponentName[.className]:hex_offset` and is then
/// hashed.
///
/// Including the class name as part of identity is a deliberate design
/// choice ([previous design discussion]):
/// - Two `Button(class="hero")` calls at the same source position
///   collapse to the same identity (intended — they're the same widget).
/// - Two `Button(class="hero")` and `Button(class="cta")` calls at the
///   same source position diverge — class is part of identity.
/// - Two `Button(class="hero")` calls at DIFFERENT source positions
///   also diverge — offset is part of identity.
///
/// Two-input redundancy: class alone or offset alone would each be
/// sufficient discriminators in most realistic source files. Combining
/// them adds belt-and-suspenders robustness against pathological
/// reformatting (e.g. an auto-formatter that shuffles named-args).
pub(crate) fn call_site_path_id(
    filename: &str,
    span_start: usize,
    component_name: &str,
    class_name: Option<&str>,
    id_name: Option<&str>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut path = String::with_capacity(component_name.len() + 32);
    path.push_str(component_name);
    if let Some(id) = id_name {
        path.push('#');
        path.push_str(id);
    }
    if let Some(class) = class_name {
        path.push('.');
        path.push_str(class);
    }
    use std::fmt::Write as _;
    let _ = write!(&mut path, ":{span_start:x}");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    filename.hash(&mut h);
    path.hash(&mut h);
    h.finish()
}

/// Rewrite `__component_call__("Name", positionals, __named__(...), body)` markers
/// into `Call(Variable("Name"), positionals, named_args, body)`, then wrap each
/// rewritten call in a `__push_call_id__(ID) ; … ; __pop_call_id__()` bracket
/// so widget FFI can key per-instance state to the source span.
/// MUST run after `validate_component_calls`. Slot markers inside body Blocks
/// are left alone.
pub(crate) fn lower_component_calls(program: &mut TypedProgram, filename: &str) {
    use zyntax_typed_ast::typed_ast::{
        TypedCall, TypedDeclaration, TypedExpression, TypedLiteral, TypedNamedArg,
    };

    // Bracket-wrap injection (push_call_id(ID) ; ORIGINAL_CALL) around
    // each lowered view call is deferred to a follow-up. The scaffolding
    // (`call_site_instance_id` helper + `__push_call_id__` /
    // `__pop_call_id__` / `__current_call_id__` ABI fns) is wired up
    // already; the open question is how to materialise the wrap without
    // tripping Zyntax's SSA value-map on `TypedExpression::Block`-as-
    // expression at the trailing-statement position.

    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>, filename: &str) {
        // Recurse bottom-up so nested marker calls also lower.
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, filename);
                rewrite_expr(&mut b.right, filename);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand, filename),
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, filename);
                for a in &mut c.positional_args {
                    rewrite_expr(a, filename);
                }
                for n in &mut c.named_args {
                    rewrite_expr(&mut n.value, filename);
                }
            }
            TypedExpression::Field(f) => rewrite_expr(&mut f.object, filename),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, filename);
                rewrite_expr(&mut idx.index, filename);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it, filename);
                }
            }
            TypedExpression::Struct(s) => {
                for field in &mut s.fields {
                    rewrite_expr(&mut field.value, filename);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, filename);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, filename);
                }
            }
            TypedExpression::Block(b) => rewrite_block(b, filename),
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, filename);
                rewrite_expr(&mut if_expr.then_branch, filename);
                rewrite_expr(&mut if_expr.else_branch, filename);
            }
            _ => {}
        }

        // Only act on `__component_call__` markers.
        let TypedExpression::Call(call) = &expr.node else {
            return;
        };
        let TypedExpression::Variable(callee_name) = &call.callee.node else {
            return;
        };
        if callee_name.resolve_global().as_deref() != Some("__component_call__") {
            return;
        }

        // args[0] is the component name as a StringLiteral. If
        // it's missing or shaped wrong the parser would have caught
        // it; this is a defensive bail.
        let Some(name_arg) = call.positional_args.first() else {
            return;
        };
        let TypedExpression::Literal(TypedLiteral::String(component_name)) = &name_arg.node else {
            return;
        };
        let component_name = *component_name;
        let span = expr.span;

        // Split the remaining args into positional + named. A
        // `__named__("name", value)` marker call lifts into a
        // `TypedNamedArg`. Everything else stays positional.
        let mut new_positional: Vec<zyntax_typed_ast::TypedNode<TypedExpression>> = Vec::new();
        let mut new_named: Vec<TypedNamedArg> = Vec::new();

        for arg in call.positional_args.iter().skip(1) {
            // Is this arg a `__named__("name", value)` marker call?
            if let TypedExpression::Call(inner) = &arg.node
                && let TypedExpression::Variable(inner_callee) = &inner.callee.node
                && inner_callee.resolve_global().as_deref() == Some("__named__")
            {
                let name_node = &inner.positional_args[0];
                let value_node = &inner.positional_args[1];
                let TypedExpression::Literal(TypedLiteral::String(arg_name)) = &name_node.node
                else {
                    // Ill-formed marker — fall through as positional.
                    new_positional.push(arg.clone());
                    continue;
                };
                new_named.push(TypedNamedArg {
                    name: *arg_name,
                    value: Box::new(value_node.clone()),
                    span: arg.span,
                });
                continue;
            }
            new_positional.push(arg.clone());
        }

        // Carry pre-existing named_args through (defensive — grammar doesn't emit them).
        new_named.extend(call.named_args.iter().cloned());

        // Resolve callee to the registry's `view_symbol` (substrate primitives use
        // `$Blinc$<Name>$view`; user components use `<Name>$view`).
        //
        // Namespace mangling: dotted DSL names from the grammar
        // (`cn.Button`) lookup against the registry as the mangled form
        // (`cn_Button`). The dot is invalid in Cranelift symbols / Rust
        // idents, so the macro registers the widget under the mangled
        // key and the symbol-name derivation strips the dot too.
        // Keeping the registry on the mangled side means
        // `primitive_callee_props` (which reverses
        // `$Blinc$<key>$view` → `<key>`) finds the same entry without
        // a second character substitution.
        let component_name_str = component_name.resolve_global().unwrap_or_default();
        let component_name_str: &str = component_name_str.as_ref();
        let registry_key = component_name_str.replace('.', "_");
        let view_symbol = blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(&registry_key)
                .map(|def| def.view_symbol.as_ref().to_string())
        })
        .unwrap_or_else(|| format!("{registry_key}$view"));
        let new_callee = zyntax_typed_ast::TypedNode::new(
            TypedExpression::Variable(zyntax_typed_ast::InternedString::new_global(&view_symbol)),
            Type::Any,
            span,
        );

        expr.node = TypedExpression::Call(TypedCall {
            callee: Box::new(new_callee),
            positional_args: new_positional,
            named_args: new_named,
            type_args: vec![],
        });

        // Compute the call-site instance ID for follow-up wrap-injection
        // passes. The hash is stable per `(filename, span.start)` —
        // anything downstream that wants to key per-call-site state can
        // compute the same value from these inputs.
        let _instance_id = call_site_instance_id(filename, span.start);
    }

    fn rewrite_block(block: &mut zyntax_typed_ast::typed_ast::TypedBlock, filename: &str) {
        let old_stmts = std::mem::take(&mut block.statements);
        let mut new_stmts: Vec<zyntax_typed_ast::TypedNode<TypedStatement>> =
            Vec::with_capacity(old_stmts.len());
        for mut stmt in old_stmts {
            rewrite_stmt(&mut stmt, filename);
            collect_children_into(&mut new_stmts, stmt);
        }
        block.statements = new_stmts;
    }

    /// Handle body-bearing component calls. Substrate primitives: body block
    /// becomes `children: [Widget]` (plus `slot_<Name>` per slot pair).
    /// User components: flatten body statements after the call. MUST keep slot
    /// markers in place for the primitive-partition path.
    fn collect_children_into(
        out: &mut Vec<zyntax_typed_ast::TypedNode<TypedStatement>>,
        mut stmt: zyntax_typed_ast::TypedNode<TypedStatement>,
    ) {
        if let TypedStatement::Expression(expr_node) = &mut stmt.node
            && let TypedExpression::Call(call) = &mut expr_node.node
        {
            let has_body_block = matches!(
                call.positional_args.last().map(|a| &a.node),
                Some(TypedExpression::Block(_))
            );
            if has_body_block {
                if callee_is_substrate_primitive(call) {
                    let block_arg = call.positional_args.pop().unwrap();
                    let block_span = block_arg.span;
                    let TypedExpression::Block(body_block) = block_arg.node else {
                        unreachable!("just confirmed Block via the matches! above");
                    };

                    // Partition body statements: unnamed body
                    // entries → default `children`; entries
                    // inside `__slot_open__("X") … __slot_close__`
                    // marker pairs → `slot_X` named arg.
                    let mut default_children: Vec<zyntax_typed_ast::TypedNode<TypedExpression>> =
                        Vec::new();
                    let mut slot_buckets: Vec<(
                        String,
                        Vec<zyntax_typed_ast::TypedNode<TypedExpression>>,
                    )> = Vec::new();
                    let mut current_slot: Option<String> = None;

                    for s in body_block.statements {
                        if let Some(name) = slot_open_name(&s) {
                            current_slot = Some(name);
                            continue;
                        }
                        if is_slot_close_stmt(&s) {
                            current_slot = None;
                            continue;
                        }
                        let TypedStatement::Expression(e) = s.node else {
                            continue;
                        };
                        match &current_slot {
                            None => default_children.push(*e),
                            Some(name) => {
                                if let Some(bucket) =
                                    slot_buckets.iter_mut().find(|(n, _)| n == name)
                                {
                                    bucket.1.push(*e);
                                } else {
                                    slot_buckets.push((name.clone(), vec![*e]));
                                }
                            }
                        }
                    }

                    if !default_children.is_empty() {
                        call.named_args.push(zyntax_typed_ast::TypedNamedArg {
                            name: zyntax_typed_ast::InternedString::new_global("children"),
                            value: Box::new(zyntax_typed_ast::TypedNode::new(
                                TypedExpression::Array(default_children),
                                Type::Any,
                                block_span,
                            )),
                            span: block_span,
                        });
                    }
                    for (name, exprs) in slot_buckets {
                        let arg_name = format!("slot_{name}");
                        call.named_args.push(zyntax_typed_ast::TypedNamedArg {
                            name: zyntax_typed_ast::InternedString::new_global(&arg_name),
                            value: Box::new(zyntax_typed_ast::TypedNode::new(
                                TypedExpression::Array(exprs),
                                Type::Any,
                                block_span,
                            )),
                            span: block_span,
                        });
                    }

                    out.push(stmt);
                    return;
                }

                // User-declared component with a body — fall
                // back to flatten: push the body-less call,
                // then inline each child statement at the
                // outer level. Slot markers are dropped here
                // (user-component view methods don't accept
                // named slots yet).
                let block_arg = call.positional_args.pop().unwrap();
                let TypedExpression::Block(body_block) = block_arg.node else {
                    unreachable!("just confirmed Block via the matches! above");
                };
                out.push(stmt);
                for inner in body_block.statements {
                    if is_slot_marker_stmt(&inner) {
                        continue;
                    }
                    collect_children_into(out, inner);
                }
                return;
            }
        }

        out.push(stmt);
    }

    /// Match `__slot_open__("name")` and return `"name"`.
    fn slot_open_name(stmt: &zyntax_typed_ast::TypedNode<TypedStatement>) -> Option<String> {
        let TypedStatement::Expression(e) = &stmt.node else {
            return None;
        };
        let TypedExpression::Call(c) = &e.node else {
            return None;
        };
        let TypedExpression::Variable(callee) = &c.callee.node else {
            return None;
        };
        if callee.resolve_global().as_deref() != Some("__slot_open__") {
            return None;
        }
        let arg = c.positional_args.first()?;
        let TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::String(name)) = &arg.node
        else {
            return None;
        };
        name.resolve_global().map(|s| s.to_string())
    }

    /// Match `__slot_close__()` — ends the active slot bucket.
    fn is_slot_close_stmt(stmt: &zyntax_typed_ast::TypedNode<TypedStatement>) -> bool {
        let TypedStatement::Expression(e) = &stmt.node else {
            return false;
        };
        let TypedExpression::Call(c) = &e.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &c.callee.node else {
            return false;
        };
        callee.resolve_global().as_deref() == Some("__slot_close__")
    }

    /// Callee is a substrate primitive (mangled name begins with `$Blinc$`).
    fn callee_is_substrate_primitive(call: &TypedCall) -> bool {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        callee
            .resolve_global()
            .as_deref()
            .is_some_and(|s| s.starts_with("$Blinc$"))
    }

    /// `Expression(Call(Variable("__slot_open__" | "__slot_close__"), _))`.
    fn is_slot_marker_stmt(stmt: &zyntax_typed_ast::TypedNode<TypedStatement>) -> bool {
        let TypedStatement::Expression(expr_node) = &stmt.node else {
            return false;
        };
        let TypedExpression::Call(call) = &expr_node.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        matches!(
            callee.resolve_global().as_deref(),
            Some("__slot_open__") | Some("__slot_close__")
        )
    }

    fn rewrite_stmt(stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>, filename: &str) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, filename),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, filename);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, filename),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, filename);
                rewrite_block(&mut if_stmt.then_block, filename);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, filename);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, filename);
                rewrite_block(&mut w.body, filename);
            }
            TypedStatement::Block(b) => rewrite_block(b, filename),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_block(body, filename);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_block(body, filename);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Prepend `__instance_id__: u64` as the first parameter of every
/// user-component `view` method. Pairs with the
/// [`inject_call_site_keys`] pass: that pass injects a `u64` literal
/// (or an XOR with the enclosing view's `__instance_id__`) as the
/// leading arg at every user-component call site, which slots into
/// this auto-prepended param.
///
/// MUST run AFTER [`bind_component_props`] (so prop params are in
/// place — `__instance_id__` goes BEFORE them) and BEFORE
/// `publish_components_to_runtime_registry` would have an issue — but
/// the registry should NOT see this synthetic param. To keep the prop
/// list clean for downstream code that consults the registry (e.g.
/// [`resolve_extern_widget_named_args`]), we ALSO skip the param
/// during registry publication. The actual filter lives in
/// `runtime_bridge.rs`.
///
/// Idempotent: if the first param is already `__instance_id__`, skip.
pub(crate) fn inject_user_view_instance_id_params(program: &mut TypedProgram) {
    use zyntax_typed_ast::Mutability;
    use zyntax_typed_ast::typed_ast::{ParameterKind, TypedDeclaration, TypedMethodParam};

    for decl in program.declarations.iter_mut() {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        for method in imp.methods.iter_mut() {
            if method.name.resolve_global().as_deref() != Some("view") {
                continue;
            }
            // Idempotence — bail if already injected.
            if method
                .params
                .first()
                .and_then(|p| p.name.resolve_global())
                .as_deref()
                == Some("__instance_id__")
            {
                continue;
            }
            let param = TypedMethodParam {
                name: zyntax_typed_ast::InternedString::new_global("__instance_id__"),
                ty: Type::Primitive(PrimitiveType::U64),
                mutability: Mutability::Immutable,
                is_self: false,
                kind: ParameterKind::Regular,
                default_value: None,
                attributes: vec![],
                span: method.span,
            };
            method.params.insert(0, param);
        }
    }
}

/// Lift `__component_props__` marker params onto every other method in the impl,
/// then strip the marker. Idempotent.
pub(crate) fn bind_component_props(program: &mut TypedProgram) {
    use zyntax_typed_ast::typed_ast::TypedDeclaration;

    for decl in program.declarations.iter_mut() {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };

        let prop_params = imp
            .methods
            .iter_mut()
            .find(|m| m.name.resolve_global().as_deref() == Some("__component_props__"))
            .map(|m| std::mem::take(&mut m.params));

        let Some(prop_params) = prop_params else {
            continue;
        };

        // Props MUST come first — call site lowers `Counter(1, 2)` to `Counter$view(1, 2)`.
        for method in imp.methods.iter_mut() {
            if method.name.resolve_global().as_deref() == Some("__component_props__") {
                continue;
            }
            let mut new_params = prop_params.clone();
            new_params.extend(std::mem::take(&mut method.params));
            method.params = new_params;
        }

        // Strip the marker so compile doesn't expose a `Counter$__component_props__`.
        imp.methods
            .retain(|m| m.name.resolve_global().as_deref() != Some("__component_props__"));
    }
}

/// Wrap each `__fsm_meta__` body with `__fsm_begin__("Name")` / `__fsm_end__()`
/// so inner marker calls know which fsm they're configuring. Idempotent.
pub(crate) fn inject_fsm_context_markers(program: &mut TypedProgram) {
    use zyntax_typed_ast::typed_ast::{
        TypedCall, TypedDeclaration, TypedExpression, TypedLiteral, TypedStatement,
    };
    use zyntax_typed_ast::{InternedString, TypedNode};

    fn make_marker_call(callee: &str, str_args: &[&str]) -> TypedNode<TypedStatement> {
        let args: Vec<TypedNode<TypedExpression>> = str_args
            .iter()
            .map(|s| {
                TypedNode::new(
                    TypedExpression::Literal(TypedLiteral::String(InternedString::new_global(s))),
                    Type::Primitive(PrimitiveType::String),
                    Span::default(),
                )
            })
            .collect();

        let call = TypedExpression::Call(TypedCall {
            callee: Box::new(TypedNode::new(
                TypedExpression::Variable(InternedString::new_global(callee)),
                Type::Unknown,
                Span::default(),
            )),
            positional_args: args,
            named_args: vec![],
            type_args: vec![],
        });

        TypedNode::new(
            TypedStatement::Expression(Box::new(TypedNode::new(
                call,
                Type::Primitive(PrimitiveType::Unit),
                Span::default(),
            ))),
            Type::Primitive(PrimitiveType::Unit),
            Span::default(),
        )
    }

    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        let Some(fsm_name) = imp.trait_name.resolve_global() else {
            continue;
        };

        for method in &mut imp.methods {
            if method.name.resolve_global().as_deref() != Some("__fsm_meta__") {
                continue;
            }
            let Some(body) = method.body.as_mut() else {
                continue;
            };

            // Skip if already wrapped (defensive against double-application).
            let already_wrapped = body
                .statements
                .first()
                .map(|s| {
                    let TypedStatement::Expression(e) = &s.node else {
                        return false;
                    };
                    let TypedExpression::Call(c) = &e.node else {
                        return false;
                    };
                    let TypedExpression::Variable(callee) = &c.callee.node else {
                        return false;
                    };
                    callee.resolve_global().as_deref() == Some("__fsm_begin__")
                })
                .unwrap_or(false);
            if already_wrapped {
                continue;
            }

            let begin = make_marker_call("__fsm_begin__", &[&fsm_name]);
            let end = make_marker_call("__fsm_end__", &[]);
            body.statements.insert(0, begin);
            body.statements.push(end);
        }
    }
}

/// Populate the global `FsmRegistry` from each fsm's `__fsm_meta__` body and
/// strip the meta method. Three phases: scan, pin TypeIds, strip markers.
pub(crate) fn populate_fsm_registry_pass(
    program: &mut TypedProgram,
    module: zyntax_typed_ast::InternedString,
) {
    use zyntax_typed_ast::InternedString;
    use zyntax_typed_ast::type_registry::{
        TypeDefinition, TypeId, TypeKind, VariantDef, VariantFields, Visibility,
    };
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedExpression, TypedLiteral, TypedVariantFields,
    };

    // Step 1: scan. Collect (fsm_name, FsmDefinition) tuples.
    let mut found: Vec<(InternedString, FsmDefinition)> = Vec::new();
    let mut guards_to_lift: Vec<(
        InternedString,
        zyntax_typed_ast::TypedNode<zyntax_typed_ast::TypedExpression>,
    )> = Vec::new();

    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };
        let Some(meta) = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
        else {
            continue;
        };
        let Some(body) = meta.body.as_ref() else {
            continue;
        };

        let fsm_name = imp.trait_name;
        let mut def = FsmDefinition {
            name: Some(fsm_name),
            ..Default::default()
        };

        for stmt_node in &body.statements {
            let TypedStatement::Expression(expr_node) = &stmt_node.node else {
                continue;
            };
            let TypedExpression::Call(call) = &expr_node.node else {
                continue;
            };
            let TypedExpression::Variable(callee_id) = &call.callee.node else {
                continue;
            };
            let callee = callee_id.resolve_global().unwrap_or_default();

            // Helper: pull a string-literal arg at index `idx`.
            let str_arg = |idx: usize| -> Option<InternedString> {
                call.positional_args.get(idx).and_then(|a| {
                    if let TypedExpression::Literal(TypedLiteral::String(s)) = &a.node {
                        Some(*s)
                    } else {
                        None
                    }
                })
            };

            match callee.as_str() {
                "__fsm_initial__" => {
                    if let Some(state) = str_arg(0) {
                        def.initial = Some(state);
                    }
                }
                "__fsm_context_field__" => {
                    // arg 0 = name (StringLiteral)
                    // arg 1 = type-name (StringLiteral, e.g. "i32")
                    // arg 2 = default literal expression
                    use zyntax_typed_ast::typed_ast::TypedLiteral as TL;
                    let (Some(field_name), Some(field_ty_name)) = (str_arg(0), str_arg(1)) else {
                        continue;
                    };
                    let Some(default_node) = call.positional_args.get(2) else {
                        continue;
                    };
                    let Some(field_ty_str) = field_ty_name.resolve_global() else {
                        continue;
                    };
                    let default = match (&field_ty_str[..], &default_node.node) {
                        ("i32", TypedExpression::Literal(TL::Integer(n))) => {
                            crate::fsm_registry::ContextDefault::I32(*n as i32)
                        }
                        ("f64", TypedExpression::Literal(TL::Float(f))) => {
                            crate::fsm_registry::ContextDefault::F64(*f)
                        }
                        ("f64", TypedExpression::Literal(TL::Integer(n))) => {
                            crate::fsm_registry::ContextDefault::F64(*n as f64)
                        }
                        ("bool", TypedExpression::Literal(TL::Bool(b))) => {
                            crate::fsm_registry::ContextDefault::Bool(*b)
                        }
                        ("string", TypedExpression::Literal(TL::String(s)))
                        | ("str", TypedExpression::Literal(TL::String(s))) => {
                            crate::fsm_registry::ContextDefault::String(*s)
                        }
                        _ => {
                            // Default for unsupported (ty, literal) combos —
                            // emit a zero-of-type so downstream init stays
                            // sane and the lowered signal still exists.
                            match &field_ty_str[..] {
                                "i32" => crate::fsm_registry::ContextDefault::I32(0),
                                "f64" => crate::fsm_registry::ContextDefault::F64(0.0),
                                "bool" => crate::fsm_registry::ContextDefault::Bool(false),
                                _ => crate::fsm_registry::ContextDefault::String(
                                    InternedString::new_global(""),
                                ),
                            }
                        }
                    };
                    def.context_fields.push(crate::fsm_registry::ContextField {
                        name: field_name,
                        ty: field_ty_name,
                        default,
                    });
                }
                "__fsm_transition__" => {
                    if let (Some(from), Some(event), Some(to)) =
                        (str_arg(0), str_arg(1), str_arg(2))
                    {
                        // Optional 4th positional arg: lifted action
                        // symbol name (a StringLiteral the early
                        // `synthesize_fsm_context_and_actions` pass
                        // wrote in place of the original Block body).
                        let actions = if let Some(action_sym) = str_arg(3) {
                            action_sym
                                .resolve_global()
                                .map(|s| {
                                    vec![blinc_runtime::fsm::TransitionAction::Symbol(
                                        std::sync::Arc::from(s.as_ref()),
                                    )]
                                })
                                .unwrap_or_default()
                        } else {
                            vec![]
                        };
                        def.transitions.push(EventTransition {
                            from,
                            event,
                            to,
                            actions,
                        });
                    }
                }
                "__fsm_tick__" => {
                    // args: 0=from, 1=guard expr, 2=to. Lift guard into a top-level fn
                    // so it survives `__fsm_meta__` stripping.
                    if let (Some(from), Some(to)) = (str_arg(0), str_arg(2)) {
                        let idx = def.tick_guards.len();
                        let fsm_name_str = fsm_name.resolve_global().unwrap_or_default();
                        let guard_fn_name = format!("__fsm_tick_guard_{fsm_name_str}_{idx}__");
                        let guard_fn = InternedString::new_global(&guard_fn_name);

                        // Clone the guard expression to escape the read borrow on `program`.
                        if let Some(expr_node) = call.positional_args.get(1) {
                            guards_to_lift.push((guard_fn, expr_node.clone()));
                        }

                        def.tick_guards.push(TickGuard {
                            from,
                            to,
                            guard_fn: Some(guard_fn),
                        });
                    }
                }
                _ => {}
            }
        }

        found.push((fsm_name, def));
    }

    // Step 2: pin TypeIds + populate the registry. Pre-register so Zyntax's
    // compile path short-circuits and respects our id.
    for (fsm_name, def) in &found {
        let type_id = TypeId::next();

        // Pin `decl.ty` so Zyntax's enum-registration check respects our id.
        let named_ty = program.type_registry.make_type(type_id, Vec::new());
        for decl in &mut program.declarations {
            let TypedDeclaration::Enum(enum_decl) = &decl.node else {
                continue;
            };
            if enum_decl.name == *fsm_name {
                decl.ty = named_ty.clone();
                break;
            }
        }

        // Pre-register so Zyntax skips double-registration with a fresh TypeId.
        if let Some(enum_decl) = program.declarations.iter().find_map(|d| match &d.node {
            TypedDeclaration::Enum(e) if e.name == *fsm_name => Some(e),
            _ => None,
        }) {
            let variants: Vec<VariantDef> = enum_decl
                .variants
                .iter()
                .enumerate()
                .map(|(i, v)| VariantDef {
                    name: v.name,
                    fields: match &v.fields {
                        TypedVariantFields::Unit => VariantFields::Unit,
                        TypedVariantFields::Tuple(types) => VariantFields::Tuple(types.clone()),
                        TypedVariantFields::Named(_) => VariantFields::Unit,
                    },
                    discriminant: Some(i as i64),
                    span: v.span,
                })
                .collect();

            let type_def = TypeDefinition {
                id: type_id,
                name: enum_decl.name,
                kind: TypeKind::Enum { variants },
                type_params: Vec::new(),
                constraints: Vec::new(),
                fields: Vec::new(),
                methods: Vec::new(),
                constructors: Vec::new(),
                metadata: Default::default(),
                span: enum_decl.span,
            };
            let _: TypeId = program.type_registry.register_type(type_def);
            let _ = Visibility::Public; // silence unused-import in case the path changes upstream
        }

        let id = FsmId { module, type_id };
        with_fsm_registry_mut(|r| r.upsert(id, def.clone()));
    }

    // Step 3: lift each captured tick-guard expression into a top-level fn
    // returning i32 (1 if guard fires, 0 otherwise). i32 chosen because bool-return
    // ABI marshaling through `runtime.call::<bool>` is untested upstream.
    use zyntax_typed_ast::typed_ast::{TypedFunction, TypedIf};
    for (fn_name, guard_expr) in guards_to_lift {
        let i32_ty = Type::Primitive(PrimitiveType::I32);

        // `return 1`
        let return_one = zyntax_typed_ast::TypedNode::new(
            TypedStatement::Return(Some(Box::new(zyntax_typed_ast::TypedNode::new(
                TypedExpression::Literal(zyntax_typed_ast::typed_ast::TypedLiteral::Integer(1)),
                i32_ty.clone(),
                Span::default(),
            )))),
            i32_ty.clone(),
            Span::default(),
        );

        let then_block = zyntax_typed_ast::typed_ast::TypedBlock {
            statements: vec![return_one],
            span: Span::default(),
        };

        // `if <guard> { return 1 }`
        let if_stmt = zyntax_typed_ast::TypedNode::new(
            TypedStatement::If(TypedIf {
                condition: Box::new(guard_expr),
                then_block,
                else_block: None,
                span: Span::default(),
            }),
            Type::Primitive(PrimitiveType::Unit),
            Span::default(),
        );

        // `return 0`
        let return_zero = zyntax_typed_ast::TypedNode::new(
            TypedStatement::Return(Some(Box::new(zyntax_typed_ast::TypedNode::new(
                TypedExpression::Literal(zyntax_typed_ast::typed_ast::TypedLiteral::Integer(0)),
                i32_ty.clone(),
                Span::default(),
            )))),
            i32_ty.clone(),
            Span::default(),
        );

        let body = zyntax_typed_ast::typed_ast::TypedBlock {
            statements: vec![if_stmt, return_zero],
            span: Span::default(),
        };

        let func = TypedFunction {
            name: fn_name,
            return_type: i32_ty.clone(),
            body: Some(body),
            ..Default::default()
        };
        let decl_node = zyntax_typed_ast::TypedNode::new(
            TypedDeclaration::Function(func),
            Type::Unknown,
            Span::default(),
        );
        program.declarations.push(decl_node);
    }

    // Step 4: strip `__fsm_meta__` so compile doesn't try to resolve markers.
    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        imp.methods
            .retain(|m| m.name.resolve_global().as_deref() != Some("__fsm_meta__"));
    }
}

/// Synthesise a sibling `<FSM>Event` enum for every fsm with transitions.
/// Variants are the unique event names in declaration order. Tick transitions
/// don't have user-facing event names and never appear here.
pub(crate) fn synthesize_fsm_event_enums(program: &mut TypedProgram) {
    use std::collections::HashSet;
    use zyntax_typed_ast::type_registry::Visibility;
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedEnum, TypedExpression, TypedLiteral, TypedVariant,
        TypedVariantFields,
    };
    use zyntax_typed_ast::{InternedString, TypedNode};

    let mut event_enums: Vec<TypedNode<TypedDeclaration>> = Vec::new();

    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };

        // Find the synthesised `__fsm_meta__` method.
        let Some(meta) = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
        else {
            continue;
        };
        let Some(body) = meta.body.as_ref() else {
            continue;
        };

        // Collect unique event names from `__fsm_transition__(_, event,
        // _)` markers, preserving declaration order so the runtime
        // discriminant assignment is stable.
        let mut events: Vec<InternedString> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for stmt_node in &body.statements {
            let TypedStatement::Expression(expr_node) = &stmt_node.node else {
                continue;
            };
            let TypedExpression::Call(call) = &expr_node.node else {
                continue;
            };
            let TypedExpression::Variable(callee) = &call.callee.node else {
                continue;
            };
            if callee.resolve_global().as_deref() != Some("__fsm_transition__") {
                continue;
            }
            let Some(event_arg) = call.positional_args.get(1) else {
                continue;
            };
            let TypedExpression::Literal(TypedLiteral::String(name)) = &event_arg.node else {
                continue;
            };
            let key = name.resolve_global().unwrap_or_default();
            if !key.is_empty() && seen.insert(key) {
                events.push(*name);
            }
        }

        if events.is_empty() {
            // Tick-only fsm — nothing to synthesise.
            continue;
        }

        // Use `trait_name` (bare ident) rather than `for_type` (Type::Named).
        let fsm_name = imp.trait_name.resolve_global().unwrap_or_default();
        let event_enum_name = InternedString::new_global(&format!("{fsm_name}Event"));

        let variants: Vec<TypedVariant> = events
            .into_iter()
            .map(|name| TypedVariant {
                name,
                fields: TypedVariantFields::Unit,
                discriminant: None,
                span: Span::default(),
            })
            .collect();

        let event_enum = TypedDeclaration::Enum(TypedEnum {
            name: event_enum_name,
            type_params: vec![],
            variants,
            visibility: Visibility::Public,
            span: Span::default(),
        });

        event_enums.push(TypedNode::new(event_enum, Type::Unknown, Span::default()));
    }

    // Append at the end so `find_map` lookups still return user-declared decls first.
    program.declarations.extend(event_enums);
}

/// Desugar `match` marker-statement quads into `if/else if/.../else` chains
/// over string equality. Wildcard arm becomes the trailing `else`.
pub(crate) fn lower_match_blocks(program: &mut TypedProgram) {
    use zyntax_typed_ast::TypedNode;
    use zyntax_typed_ast::typed_ast::{
        BinaryOp, TypedBinary, TypedBlock, TypedDeclaration, TypedExpression, TypedIfExpr,
        TypedLiteral,
    };

    fn is_call_to(stmt: &TypedNode<TypedStatement>, name: &str) -> bool {
        let TypedStatement::Expression(expr) = &stmt.node else {
            return false;
        };
        let TypedExpression::Call(call) = &expr.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        callee.resolve_global().as_deref() == Some(name)
    }

    fn call_first_arg(stmt: &TypedNode<TypedStatement>) -> Option<&TypedNode<TypedExpression>> {
        let TypedStatement::Expression(expr) = &stmt.node else {
            return None;
        };
        let TypedExpression::Call(call) = &expr.node else {
            return None;
        };
        call.positional_args.first()
    }

    /// Lower every `__match_begin__ … __match_end__` span in `stmts`.
    /// MUST recurse into nested blocks first so inner matches lower before outers see them.
    fn rewrite_stmts(stmts: &mut Vec<TypedNode<TypedStatement>>) {
        for stmt in stmts.iter_mut() {
            recurse_into_stmt(stmt);
        }

        let mut i = 0;
        while i < stmts.len() {
            if !is_call_to(&stmts[i], "__match_begin__") {
                i += 1;
                continue;
            }
            let Some(scrutinee_expr) = call_first_arg(&stmts[i]).cloned() else {
                i += 1;
                continue;
            };

            let mut end_idx = i + 1;
            while end_idx < stmts.len() && !is_call_to(&stmts[end_idx], "__match_end__") {
                end_idx += 1;
            }
            if end_idx >= stmts.len() {
                // Malformed — no end marker.
                i += 1;
                continue;
            }

            // Each arm at i+1..end_idx is a Block whose first stmt is `__match_arm__(pat)`.
            // `pat` is one of:
            //   - `StringLiteral("__wildcard__")` — the `_` arm
            //   - `StringLiteral("literal")`      — a string pattern
            //   - `Call(__struct_pattern__, [StringLiteral(name), StringLiteral(f1), …])`
            //     — a struct destructure pattern, binds each field as a
            //     `let` at the start of the arm body. See
            //     `pattern_struct` in `grammar/blinc.zyn` for the
            //     producer-side rationale.
            enum ArmPattern {
                Literal(String),
                Wildcard,
                Struct {
                    #[allow(dead_code)]
                    name: String,
                    fields: Vec<String>,
                },
            }
            let mut arms: Vec<(Option<ArmPattern>, TypedBlock)> = Vec::new();
            for arm in stmts[(i + 1)..end_idx].iter() {
                let TypedStatement::Block(arm_block) = &arm.node else {
                    continue;
                };
                if arm_block.statements.is_empty() {
                    continue;
                }
                if !is_call_to(&arm_block.statements[0], "__match_arm__") {
                    continue;
                }
                let pat_expr = call_first_arg(&arm_block.statements[0]);
                let pat = pat_expr.and_then(|expr| {
                    // Literal string pattern (and the `__wildcard__` sentinel).
                    if let TypedExpression::Literal(TypedLiteral::String(s)) = &expr.node {
                        let s_arc = s.resolve_global()?;
                        let s_str: &str = &s_arc;
                        return Some(if s_str == "__wildcard__" {
                            ArmPattern::Wildcard
                        } else {
                            ArmPattern::Literal(s_str.to_string())
                        });
                    }
                    // Struct destructure: Call(__struct_pattern__, [name, field1, …]).
                    if let TypedExpression::Call(call) = &expr.node
                        && let TypedExpression::Variable(callee) = &call.callee.node
                        && callee.resolve_global().as_deref() == Some("__struct_pattern__")
                    {
                        let mut args = call.positional_args.iter();
                        let name = args.next().and_then(|a| {
                            if let TypedExpression::Literal(TypedLiteral::String(s)) = &a.node {
                                s.resolve_global().map(|s| s.to_string())
                            } else {
                                None
                            }
                        })?;
                        let fields = args
                            .filter_map(|a| {
                                if let TypedExpression::Literal(TypedLiteral::String(s)) = &a.node {
                                    s.resolve_global().map(|s| s.to_string())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>();
                        return Some(ArmPattern::Struct { name, fields });
                    }
                    None
                });
                let body = TypedBlock {
                    statements: arm_block.statements[1..].to_vec(),
                    span: arm_block.span,
                };
                arms.push((pat, body));
            }

            // Build the if/else-if/else chain. The first arm that
            // unconditionally matches (wildcard OR struct pattern in
            // this MVP) becomes the trailing `else`; subsequent
            // always-match arms are dropped. Struct arms prepend
            // `let <field> = <scrutinee>.<field>` bindings to their
            // body so the arm body sees the destructured locals.
            fn wrap_struct_body(
                body: TypedBlock,
                fields: &[String],
                scrutinee: &TypedNode<TypedExpression>,
            ) -> TypedBlock {
                let span = body.span;
                let mut bindings: Vec<TypedNode<TypedStatement>> = fields
                    .iter()
                    .map(|field| {
                        let field_access = TypedNode::new(
                            TypedExpression::Field(zyntax_typed_ast::typed_ast::TypedFieldAccess {
                                object: Box::new(scrutinee.clone()),
                                field: zyntax_typed_ast::InternedString::new_global(field),
                            }),
                            Type::Any,
                            span,
                        );
                        TypedNode::new(
                            TypedStatement::Let(zyntax_typed_ast::typed_ast::TypedLet {
                                name: zyntax_typed_ast::InternedString::new_global(field),
                                ty: Type::Any,
                                mutability: zyntax_typed_ast::Mutability::Immutable,
                                initializer: Some(Box::new(field_access)),
                                span,
                            }),
                            Type::Primitive(PrimitiveType::Unit),
                            span,
                        )
                    })
                    .collect();
                bindings.extend(body.statements);
                TypedBlock {
                    statements: bindings,
                    span,
                }
            }

            let mut else_block: Option<TypedBlock> = None;
            let mut chain_arms: Vec<(String, TypedBlock)> = Vec::new();
            for (pat, body) in arms {
                match pat {
                    Some(ArmPattern::Wildcard) if else_block.is_none() => {
                        else_block = Some(body);
                    }
                    Some(ArmPattern::Wildcard) => {}
                    Some(ArmPattern::Struct { fields, .. }) if else_block.is_none() => {
                        else_block = Some(wrap_struct_body(body, &fields, &scrutinee_expr));
                    }
                    Some(ArmPattern::Struct { .. }) => {
                        // Already have an else — subsequent always-match
                        // arms are unreachable in this MVP.
                    }
                    Some(ArmPattern::Literal(p)) => {
                        chain_arms.push((p, body));
                    }
                    None => {}
                }
            }

            // Fold from last to first into an *expression*-form if/else
            // chain (`TypedExpression::If` wrapping `TypedExpression::Block`
            // branches). The expression form is the only one Zyntax's SSA
            // creates fresh successor blocks for on demand — the statement
            // form (`TypedStatement::If`) relies on pre-built CFG
            // successors, which `translate_closure` doesn't construct.
            // Inside a closure body the statement form silently skips
            // both branches (then + else), so the match arms never fire.
            // The expression form works in both top-level and closure
            // contexts.
            let unit = || Type::Primitive(PrimitiveType::Unit);
            let block_expr = |b: TypedBlock| -> TypedNode<TypedExpression> {
                let s = b.span;
                TypedNode::new(TypedExpression::Block(b), unit(), s)
            };
            let mut tail_else_expr: TypedNode<TypedExpression> = match else_block {
                Some(b) => block_expr(b),
                None => block_expr(TypedBlock {
                    statements: vec![],
                    span: scrutinee_expr.span,
                }),
            };
            for (pat, body) in chain_arms.into_iter().rev() {
                let span = body.span;
                let pat_literal = TypedNode::new(
                    TypedExpression::Literal(TypedLiteral::String(
                        zyntax_typed_ast::InternedString::new_global(&pat),
                    )),
                    Type::Primitive(PrimitiveType::String),
                    span,
                );
                let condition = TypedNode::new(
                    TypedExpression::Binary(TypedBinary {
                        op: BinaryOp::Eq,
                        left: Box::new(scrutinee_expr.clone()),
                        right: Box::new(pat_literal),
                    }),
                    Type::Primitive(PrimitiveType::Bool),
                    span,
                );
                let then_expr = block_expr(body);
                let if_expr = TypedExpression::If(TypedIfExpr {
                    condition: Box::new(condition),
                    then_branch: Box::new(then_expr),
                    else_branch: Box::new(tail_else_expr),
                });
                tail_else_expr = TypedNode::new(if_expr, unit(), span);
            }

            // Splice the chain in place of the marker span. Wrap the
            // expression-form if-chain in a single `TypedStatement::Expression`.
            let chain_span = tail_else_expr.span;
            let chain_stmt = TypedNode::new(
                TypedStatement::Expression(Box::new(tail_else_expr)),
                unit(),
                chain_span,
            );
            stmts.splice(i..=end_idx, [chain_stmt]);
            i += 1;
        }
    }

    fn recurse_into_stmt(stmt: &mut TypedNode<TypedStatement>) {
        match &mut stmt.node {
            TypedStatement::Block(b) => {
                rewrite_stmts(&mut b.statements);
            }
            TypedStatement::If(if_stmt) => {
                rewrite_stmts(&mut if_stmt.then_block.statements);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_stmts(&mut else_block.statements);
                }
            }
            TypedStatement::While(w) => {
                rewrite_stmts(&mut w.body.statements);
            }
            TypedStatement::Expression(expr) => {
                recurse_into_expr(expr);
            }
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    recurse_into_expr(init);
                }
            }
            _ => {}
        }
    }

    fn recurse_into_expr(expr: &mut TypedNode<TypedExpression>) {
        // Lambda bodies need this: `<Fsm>.subscribe(..., || { match … })` must
        // lower before any downstream pass walks the lambda HIR.
        match &mut expr.node {
            TypedExpression::Lambda(lam) => match &mut lam.body {
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                    recurse_into_expr(e);
                }
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                    rewrite_stmts(&mut block.statements);
                }
            },
            TypedExpression::Block(block) => {
                rewrite_stmts(&mut block.statements);
            }
            TypedExpression::Call(call) => {
                recurse_into_expr(&mut call.callee);
                for arg in &mut call.positional_args {
                    recurse_into_expr(arg);
                }
            }
            TypedExpression::Binary(b) => {
                recurse_into_expr(&mut b.left);
                recurse_into_expr(&mut b.right);
            }
            TypedExpression::If(if_expr) => {
                recurse_into_expr(&mut if_expr.condition);
                recurse_into_expr(&mut if_expr.then_branch);
                recurse_into_expr(&mut if_expr.else_branch);
            }
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_stmts(&mut body.statements);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_stmts(&mut body.statements);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Mint a placeholder `Interface { name: <FsmName> }` for each FSM impl so
/// Zyntax's compiler doesn't log "Trait not found" and drop the impl's methods.
pub(crate) fn synthesize_fsm_trait_interfaces(program: &mut TypedProgram) {
    use std::collections::HashSet;
    use zyntax_typed_ast::type_registry::Visibility;
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedInterface};
    use zyntax_typed_ast::{InternedString, TypedNode};

    let mut fsm_names: HashSet<InternedString> = HashSet::new();
    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };
        if imp
            .methods
            .iter()
            .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
        {
            fsm_names.insert(imp.trait_name);
        }
    }

    let interfaces: Vec<TypedNode<TypedDeclaration>> = fsm_names
        .into_iter()
        .map(|name| {
            let iface = TypedInterface {
                name,
                type_params: vec![],
                extends: vec![],
                methods: vec![],
                associated_types: vec![],
                visibility: Visibility::Public,
                span: Span::default(),
            };
            TypedNode::new(
                TypedDeclaration::Interface(iface),
                Type::Unknown,
                Span::default(),
            )
        })
        .collect();

    program.declarations.extend(interfaces);
}

/// Rewrite view fns to value-returning (`I64` widget handle) when their body
/// ends in a `$Blinc$<X>$view` call. MUST run before [`ensure_unit_return`].
/// Pinning return to a concrete `I64` (not `Any`) keeps Zyntax's body
/// classifier on the well-trodden specialised-call path.
pub(crate) fn lower_view_to_value_returning(
    program: &mut TypedProgram,
    value_returning_symbols: &mut std::collections::HashSet<String>,
) {
    use zyntax_typed_ast::{TypedDeclaration, TypedExpression};

    fn is_view_name(name: zyntax_typed_ast::InternedString) -> bool {
        matches!(
            name.resolve_global().as_deref(),
            Some("render_view") | Some("view")
        )
    }

    /// Match `Expression(Call(Variable("<X>$view"), ...))` where `<X>` is a
    /// substrate primitive or a user component whose own view is value-returning.
    fn is_primitive_view_call_stmt(
        stmt: &TypedStatement,
        value_returning_symbols: &std::collections::HashSet<String>,
    ) -> bool {
        let TypedStatement::Expression(expr) = stmt else {
            return false;
        };
        let TypedExpression::Call(call) = &expr.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        let Some(name) = callee.resolve_global() else {
            return false;
        };
        let s: &str = name.as_ref();
        // Substrate primitives are always i64-returning.
        if s.starts_with("$Blinc$") && s.ends_with("$view") {
            return true;
        }
        // User components: only if promoted by an earlier pass.
        value_returning_symbols.contains(s)
    }

    /// Rewrite trailing primitive-call to `Return(Some(call))` and return whether converted.
    fn try_convert_trailing(
        body: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        value_returning_symbols: &std::collections::HashSet<String>,
    ) -> bool {
        let Some(last) = body.statements.last() else {
            return false;
        };
        if !is_primitive_view_call_stmt(&last.node, value_returning_symbols) {
            return false;
        }
        let last = body
            .statements
            .last_mut()
            .expect("just confirmed last exists above");
        let placeholder = TypedStatement::Continue;
        let original = std::mem::replace(&mut last.node, placeholder);
        let TypedStatement::Expression(expr) = original else {
            unreachable!("just confirmed Expression shape above");
        };
        last.node = TypedStatement::Return(Some(expr));
        true
    }

    let widget_handle_type = Type::Primitive(PrimitiveType::I64);

    // Pass 1: impl `view` methods. Pass 2: free-standing view fns referencing them.
    for decl in program.declarations.iter_mut() {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        // `<TypeName>$<method>` mangling — pull the type name once per impl.
        let type_name: Option<String> = match &imp.for_type {
            Type::Unresolved(name) => name.resolve_global().map(|s| s.to_string()),
            Type::Named { id, .. } => program
                .type_registry
                .get_type_by_id(*id)
                .and_then(|t| t.name.resolve_global())
                .map(|s| s.to_string()),
            _ => None,
        };
        for method in &mut imp.methods {
            if !is_view_name(method.name) {
                continue;
            }
            let Some(body) = method.body.as_mut() else {
                continue;
            };
            if try_convert_trailing(body, value_returning_symbols) {
                method.return_type = widget_handle_type.clone();
                if let (Some(t), Some(m)) = (type_name.as_ref(), method.name.resolve_global()) {
                    value_returning_symbols.insert(format!("{t}${m}"));
                }
            }
        }
    }

    for decl in program.declarations.iter_mut() {
        let TypedDeclaration::Function(func) = &mut decl.node else {
            continue;
        };
        if func.is_external {
            continue;
        }
        if !is_view_name(func.name) {
            continue;
        }
        let Some(body) = func.body.as_mut() else {
            continue;
        };
        if try_convert_trailing(body, value_returning_symbols) {
            func.return_type = widget_handle_type.clone();
            if let Some(name) = func.name.resolve_global() {
                value_returning_symbols.insert(name.to_string());
            }
        }
    }
}

/// Expand substrate primitive calls carrying `children = Array([...])` (and slot
/// arrays) into Block expansions backed by `__new_child_list__` / `__push_child__`.
/// Post-order recursive. MUST run after `lower_view_to_value_returning` and
/// before `ensure_unit_return`.
pub(crate) fn lower_children_arrays_to_blocks(program: &mut TypedProgram) {
    use zyntax_typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedNamedArg};

    /// Counter for unique `__blinc_children_<N>` idents.
    fn next_id(counter: &mut u32) -> u32 {
        let id = *counter;
        *counter += 1;
        id
    }

    /// Walk a statement and rewrite any nested primitive calls.
    fn walk_stmt(stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>, counter: &mut u32) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, counter),
            TypedStatement::Return(Some(e)) => rewrite_expr(e, counter),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, counter);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, counter);
                for s in &mut if_stmt.then_block.statements {
                    walk_stmt(s, counter);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for s in &mut else_block.statements {
                        walk_stmt(s, counter);
                    }
                }
            }
            _ => {}
        }
    }

    /// Post-order — recurse before rewriting `expr`.
    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>, counter: &mut u32) {
        match &mut expr.node {
            TypedExpression::Call(call) => {
                rewrite_expr(&mut call.callee, counter);
                for arg in &mut call.positional_args {
                    rewrite_expr(arg, counter);
                }
                for named in &mut call.named_args {
                    rewrite_expr(&mut named.value, counter);
                }
            }
            TypedExpression::Array(items) => {
                for item in items {
                    rewrite_expr(item, counter);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    walk_stmt(stmt, counter);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, counter);
                rewrite_expr(&mut b.right, counter);
            }
            _ => {}
        }

        // For primitives with child-slot props (`children`, `slot_<Name>`), gather each
        // Array into a `__new_child_list__` Block. The final call carries the lists as
        // named args; `resolve_extern_widget_named_args` later positionalises them.
        let span = expr.span;
        let i64_ty = Type::Primitive(PrimitiveType::I64);
        let unit_ty = Type::Primitive(PrimitiveType::Unit);

        let TypedExpression::Call(call) = &mut expr.node else {
            return;
        };
        let Some(slot_prop_names) = callee_slot_prop_names(call) else {
            return;
        };

        let mut prelude: Vec<zyntax_typed_ast::TypedNode<TypedStatement>> = Vec::new();
        let mut had_real_slot = false;

        for slot_name in &slot_prop_names {
            let na_idx = call
                .named_args
                .iter()
                .position(|na| na.name.resolve_global().as_deref() == Some(slot_name.as_str()));
            let Some(idx) = na_idx else {
                // Slot not supplied — inject a `0` literal so
                // the registry-driven resolution finds something
                // at this slot's named position.
                call.named_args.push(TypedNamedArg {
                    name: zyntax_typed_ast::InternedString::new_global(slot_name),
                    value: Box::new(typed_node(
                        TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
                        i64_ty.clone(),
                        span,
                    )),
                    span,
                });
                continue;
            };
            let mut na = call.named_args.remove(idx);
            let TypedExpression::Array(child_exprs) = std::mem::replace(
                &mut na.value.node,
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
            ) else {
                // Already a non-Array value (e.g., user passed
                // a raw i64 list pointer). Leave it alone.
                call.named_args.push(na);
                continue;
            };
            had_real_slot = true;

            let id = next_id(counter);
            let list_ident =
                zyntax_typed_ast::InternedString::new_global(&format!("__blinc_children_{id}"));

            // let __blinc_children_<id> = __new_child_list__()
            prelude.push(typed_node(
                TypedStatement::Let(zyntax_typed_ast::typed_ast::TypedLet {
                    name: list_ident,
                    ty: i64_ty.clone(),
                    mutability: zyntax_typed_ast::Mutability::Immutable,
                    initializer: Some(Box::new(typed_node(
                        TypedExpression::Call(TypedCall {
                            callee: Box::new(typed_node(
                                TypedExpression::Variable(
                                    zyntax_typed_ast::InternedString::new_global(
                                        "__new_child_list__",
                                    ),
                                ),
                                Type::Any,
                                span,
                            )),
                            positional_args: vec![],
                            named_args: vec![],
                            type_args: vec![],
                        }),
                        i64_ty.clone(),
                        span,
                    ))),
                    span,
                }),
                unit_ty.clone(),
                span,
            ));

            // for each child: __push_child__(__list, child)
            for child_expr in child_exprs {
                let push_call = TypedExpression::Call(TypedCall {
                    callee: Box::new(typed_node(
                        TypedExpression::Variable(zyntax_typed_ast::InternedString::new_global(
                            "__push_child__",
                        )),
                        Type::Any,
                        span,
                    )),
                    positional_args: vec![
                        typed_node(TypedExpression::Variable(list_ident), i64_ty.clone(), span),
                        child_expr,
                    ],
                    named_args: vec![],
                    type_args: vec![],
                });
                prelude.push(typed_node(
                    TypedStatement::Expression(Box::new(typed_node(
                        push_call,
                        unit_ty.clone(),
                        span,
                    ))),
                    unit_ty.clone(),
                    span,
                ));
            }

            // Re-attach the slot as a named arg pointing at the ident.
            call.named_args.push(TypedNamedArg {
                name: zyntax_typed_ast::InternedString::new_global(slot_name),
                value: Box::new(typed_node(
                    TypedExpression::Variable(list_ident),
                    i64_ty.clone(),
                    span,
                )),
                span,
            });
        }

        if !had_real_slot {
            // No body-supplied slots — `0`-literal fills already on call.
            return;
        }

        // Wrap call in a trailing-expression Block.
        let final_call = TypedExpression::Call(TypedCall {
            callee: call.callee.clone(),
            positional_args: std::mem::take(&mut call.positional_args),
            named_args: std::mem::take(&mut call.named_args),
            type_args: std::mem::take(&mut call.type_args),
        });
        prelude.push(typed_node(
            TypedStatement::Expression(Box::new(typed_node(final_call, i64_ty.clone(), span))),
            i64_ty.clone(),
            span,
        ));

        expr.node = TypedExpression::Block(zyntax_typed_ast::typed_ast::TypedBlock {
            statements: prelude,
            span,
        });
    }

    /// Return child-slot prop names (`children`, `slot_<Name>`) in registry order.
    /// `None` for leaf primitives or non-primitives.
    fn callee_slot_prop_names(call: &TypedCall) -> Option<Vec<String>> {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return None;
        };
        let sym = callee.resolve_global()?;
        let sym: &str = &sym;
        let name = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))?;
        let slots: Vec<String> = blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name)
                .map(|def| {
                    def.props
                        .iter()
                        .filter_map(|p| {
                            let n = p.name.as_ref();
                            (n == "children" || n.starts_with("slot_")).then(|| n.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        if slots.is_empty() { None } else { Some(slots) }
    }

    /// Legacy — unused after refactor.
    #[allow(dead_code)]
    fn callee_takes_children(call: &TypedCall) -> bool {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        let Some(sym) = callee.resolve_global() else {
            return false;
        };
        let sym: &str = &sym;
        let Some(name) = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))
        else {
            return false;
        };
        blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name)
                .map(|def| def.props.iter().any(|p| p.name.as_ref() == "children"))
                .unwrap_or(false)
        })
    }

    let mut counter: u32 = 0;
    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = func.body.as_mut() {
                    for stmt in &mut body.statements {
                        walk_stmt(stmt, &mut counter);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = method.body.as_mut() {
                        for stmt in &mut body.statements {
                            walk_stmt(stmt, &mut counter);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Marshal complex extern-widget prop struct literals into opaque `i64` handles.
/// The widget registry keeps the DSL-facing named type; the generated extern
/// thunk accepts a stable handle ABI.
pub(crate) fn lower_struct_widget_props_to_handles(
    program: &mut TypedProgram,
) -> Result<(), Vec<String>> {
    use std::collections::HashMap;
    use zyntax_typed_ast::{TypedCall, TypedDeclaration, TypedExpression};

    type FieldMap = HashMap<String, Type>;
    type StructFields = HashMap<String, FieldMap>;

    fn is_complex_type(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Unresolved(_) | Type::Named { .. } | Type::Struct { .. }
        )
    }

    fn collect_struct_fields(program: &TypedProgram) -> StructFields {
        let mut out = HashMap::new();
        for decl in &program.declarations {
            let TypedDeclaration::Class(class) = &decl.node else {
                continue;
            };
            let Some(class_name) = class.name.resolve_global().map(|s| s.to_string()) else {
                continue;
            };
            let mut fields = HashMap::new();
            for field in &class.fields {
                if let Some(name) = field.name.resolve_global() {
                    fields.insert(name.to_string(), field.ty.clone());
                }
            }
            out.insert(class_name, fields);
        }
        out
    }

    fn next_id(counter: &mut u32) -> u32 {
        let id = *counter;
        *counter += 1;
        id
    }

    fn setter_for_type(ty: &Type) -> &'static str {
        match ty {
            Type::Primitive(PrimitiveType::Bool) => "__set_struct_bool__",
            Type::Primitive(PrimitiveType::I32) => "__set_struct_i32__",
            Type::Primitive(PrimitiveType::F64) => "__set_struct_f64__",
            Type::Primitive(PrimitiveType::String) => "__set_struct_string__",
            Type::Unresolved(_) | Type::Named { .. } | Type::Struct { .. } => {
                "__set_struct_handle__"
            }
            _ => "__set_struct_i64__",
        }
    }

    fn string_expr(
        value: &str,
        span: zyntax_typed_ast::Span,
    ) -> zyntax_typed_ast::TypedNode<TypedExpression> {
        typed_node(
            TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::String(
                zyntax_typed_ast::InternedString::new_global(value),
            )),
            Type::Primitive(PrimitiveType::String),
            span,
        )
    }

    fn bool_literal_as_i32(
        expr: zyntax_typed_ast::TypedNode<TypedExpression>,
    ) -> zyntax_typed_ast::TypedNode<TypedExpression> {
        let span = expr.span;
        let expr_ty = expr.ty.clone();
        match expr.node {
            TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Bool(value)) => typed_node(
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(if value {
                    1
                } else {
                    0
                })),
                Type::Primitive(PrimitiveType::I32),
                span,
            ),
            other if matches!(&expr_ty, Type::Primitive(PrimitiveType::Bool)) => {
                let bool_expr = typed_node(other, Type::Primitive(PrimitiveType::Bool), span);
                typed_node(
                    TypedExpression::If(zyntax_typed_ast::typed_ast::TypedIfExpr {
                        condition: Box::new(bool_expr),
                        then_branch: Box::new(typed_node(
                            TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(1)),
                            Type::Primitive(PrimitiveType::I32),
                            span,
                        )),
                        else_branch: Box::new(typed_node(
                            TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
                            Type::Primitive(PrimitiveType::I32),
                            span,
                        )),
                    }),
                    Type::Primitive(PrimitiveType::I32),
                    span,
                )
            }
            other => typed_node(other, expr_ty, span),
        }
    }

    fn call_expr(
        callee: &str,
        args: Vec<zyntax_typed_ast::TypedNode<TypedExpression>>,
        ty: Type,
        span: zyntax_typed_ast::Span,
    ) -> zyntax_typed_ast::TypedNode<TypedExpression> {
        typed_node(
            TypedExpression::Call(TypedCall {
                callee: Box::new(typed_node(
                    TypedExpression::Variable(zyntax_typed_ast::InternedString::new_global(callee)),
                    Type::Any,
                    span,
                )),
                positional_args: args,
                named_args: vec![],
                type_args: vec![],
            }),
            ty,
            span,
        )
    }

    fn lower_struct_to_handle(
        expr: zyntax_typed_ast::TypedNode<TypedExpression>,
        structs: &StructFields,
        prelude: &mut Vec<zyntax_typed_ast::TypedNode<TypedStatement>>,
        counter: &mut u32,
    ) -> zyntax_typed_ast::TypedNode<TypedExpression> {
        let span = expr.span;
        let i64_ty = Type::Primitive(PrimitiveType::I64);
        let unit_ty = Type::Primitive(PrimitiveType::Unit);
        let TypedExpression::Struct(struct_lit) = expr.node else {
            return expr;
        };

        let struct_name = struct_lit
            .name
            .resolve_global()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let id = next_id(counter);
        let ident =
            zyntax_typed_ast::InternedString::new_global(&format!("__blinc_struct_value_{id}"));

        prelude.push(typed_node(
            TypedStatement::Let(zyntax_typed_ast::typed_ast::TypedLet {
                name: ident,
                ty: i64_ty.clone(),
                mutability: zyntax_typed_ast::Mutability::Immutable,
                initializer: Some(Box::new(call_expr(
                    "__new_struct_value__",
                    vec![],
                    i64_ty.clone(),
                    span,
                ))),
                span,
            }),
            unit_ty.clone(),
            span,
        ));

        let field_types = structs.get(&struct_name);
        for field in struct_lit.fields {
            let field_name = field
                .name
                .resolve_global()
                .map(|s| s.to_string())
                .unwrap_or_default();
            let field_ty = field_types
                .and_then(|fields| fields.get(&field_name))
                .cloned()
                .unwrap_or_else(|| field.value.ty.clone());
            let setter = setter_for_type(&field_ty);
            let mut value = *field.value;
            if is_complex_type(&field_ty) && matches!(value.node, TypedExpression::Struct(_)) {
                value = lower_struct_to_handle(value, structs, prelude, counter);
            }
            if matches!(field_ty, Type::Primitive(PrimitiveType::Bool)) {
                value = bool_literal_as_i32(value);
            } else {
                value.ty = field_ty;
            }

            prelude.push(typed_node(
                TypedStatement::Expression(Box::new(call_expr(
                    setter,
                    vec![
                        typed_node(TypedExpression::Variable(ident), i64_ty.clone(), span),
                        string_expr(&field_name, span),
                        value,
                    ],
                    unit_ty.clone(),
                    span,
                ))),
                unit_ty.clone(),
                span,
            ));
        }

        typed_node(TypedExpression::Variable(ident), i64_ty, span)
    }

    fn lower_arg_if_needed(
        arg: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        prop_ty: &Type,
        structs: &StructFields,
        prelude: &mut Vec<zyntax_typed_ast::TypedNode<TypedStatement>>,
        counter: &mut u32,
    ) {
        if !is_complex_type(prop_ty) || !matches!(arg.node, TypedExpression::Struct(_)) {
            return;
        }
        let span = arg.span;
        let old = std::mem::replace(
            arg,
            typed_node(
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
                Type::Primitive(PrimitiveType::I64),
                span,
            ),
        );
        *arg = lower_struct_to_handle(old, structs, prelude, counter);
    }

    fn extern_props(call: &TypedCall) -> Option<Vec<(String, Type)>> {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return None;
        };
        let sym = callee.resolve_global()?;
        let sym: &str = &sym;
        let name = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))?;
        blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name).map(|def| {
                def.props
                    .iter()
                    .map(|p| (p.name.to_string(), p.ty.clone()))
                    .collect()
            })
        })
    }

    fn walk_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        structs: &StructFields,
        counter: &mut u32,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, structs, counter),
            TypedStatement::Return(Some(e)) => rewrite_expr(e, structs, counter),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, structs, counter);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, structs, counter);
                for s in &mut if_stmt.then_block.statements {
                    walk_stmt(s, structs, counter);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for s in &mut else_block.statements {
                        walk_stmt(s, structs, counter);
                    }
                }
            }
            TypedStatement::Block(block) => {
                for s in &mut block.statements {
                    walk_stmt(s, structs, counter);
                }
            }
            _ => {}
        }
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        structs: &StructFields,
        counter: &mut u32,
    ) {
        match &mut expr.node {
            TypedExpression::Call(call) => {
                rewrite_expr(&mut call.callee, structs, counter);
                for arg in &mut call.positional_args {
                    rewrite_expr(arg, structs, counter);
                }
                for na in &mut call.named_args {
                    rewrite_expr(&mut na.value, structs, counter);
                }
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for item in items {
                    rewrite_expr(item, structs, counter);
                }
            }
            TypedExpression::Struct(s) => {
                for field in &mut s.fields {
                    rewrite_expr(&mut field.value, structs, counter);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    walk_stmt(stmt, structs, counter);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, structs, counter);
                rewrite_expr(&mut b.right, structs, counter);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand, structs, counter),
            TypedExpression::Field(f) => rewrite_expr(&mut f.object, structs, counter),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, structs, counter);
                rewrite_expr(&mut idx.index, structs, counter);
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, structs, counter);
                for arg in &mut mc.positional_args {
                    rewrite_expr(arg, structs, counter);
                }
            }
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, structs, counter);
                rewrite_expr(&mut if_expr.then_branch, structs, counter);
                rewrite_expr(&mut if_expr.else_branch, structs, counter);
            }
            _ => {}
        }

        let span = expr.span;
        let Some(props) = (match &expr.node {
            TypedExpression::Call(call) => extern_props(call),
            _ => None,
        }) else {
            return;
        };
        let TypedExpression::Call(call) = &mut expr.node else {
            return;
        };
        let mut prelude = Vec::new();

        for (i, arg) in call.positional_args.iter_mut().enumerate() {
            if let Some((_, prop_ty)) = props.get(i) {
                lower_arg_if_needed(arg, prop_ty, structs, &mut prelude, counter);
            }
        }
        for named in &mut call.named_args {
            let Some(name) = named.name.resolve_global() else {
                continue;
            };
            let Some((_, prop_ty)) = props.iter().find(|(prop_name, _)| prop_name == &*name) else {
                continue;
            };
            lower_arg_if_needed(&mut named.value, prop_ty, structs, &mut prelude, counter);
        }

        if prelude.is_empty() {
            return;
        }

        let final_call = TypedExpression::Call(TypedCall {
            callee: call.callee.clone(),
            positional_args: std::mem::take(&mut call.positional_args),
            named_args: std::mem::take(&mut call.named_args),
            type_args: std::mem::take(&mut call.type_args),
        });
        prelude.push(typed_node(
            TypedStatement::Expression(Box::new(typed_node(
                final_call,
                Type::Primitive(PrimitiveType::I64),
                span,
            ))),
            Type::Primitive(PrimitiveType::I64),
            span,
        ));
        expr.node = TypedExpression::Block(zyntax_typed_ast::typed_ast::TypedBlock {
            statements: prelude,
            span,
        });
    }

    let structs = collect_struct_fields(program);
    let mut counter = 0;
    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = func.body.as_mut() {
                    for stmt in &mut body.statements {
                        walk_stmt(stmt, &structs, &mut counter);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = method.body.as_mut() {
                        for stmt in &mut body.statements {
                            walk_stmt(stmt, &structs, &mut counter);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Inline styling props recognised on DSL primitive call sites. Each maps to
/// an overlay-setter extern (`__set_overlay_*__`).
const STYLING_PROP_NAMES: &[(&str, &str, StylingValueKind)] = &[
    ("bg", "__set_overlay_bg__", StylingValueKind::IntColor),
    (
        "opacity",
        "__set_overlay_opacity__",
        StylingValueKind::Float,
    ),
    (
        "corner_radius",
        "__set_overlay_corner_radius__",
        StylingValueKind::Float,
    ),
    (
        "border_width",
        "__set_overlay_border_width__",
        StylingValueKind::Float,
    ),
    (
        "border_color",
        "__set_overlay_border_color__",
        StylingValueKind::IntColor,
    ),
];

#[derive(Clone, Copy)]
enum StylingValueKind {
    IntColor,
    Float,
}

/// Gather inline styling args (`bg`, `opacity`, …) into a `__new_style_overlay__`
/// Block and attach overlay pointer as `__style` named arg. MUST run after
/// `lower_children_arrays_to_blocks` and before `resolve_extern_widget_named_args`.
pub(crate) fn lower_styling_args_to_overlays(program: &mut TypedProgram) {
    use zyntax_typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedNamedArg};

    fn callee_is_styled_primitive(call: &TypedCall) -> bool {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        let Some(sym) = callee.resolve_global() else {
            return false;
        };
        let sym: &str = &sym;
        let Some(name) = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))
        else {
            return false;
        };
        blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name)
                .map(|def| def.props.iter().any(|p| p.name.as_ref() == "__style"))
                .unwrap_or(false)
        })
    }

    /// If `value` is a bare identifier naming a DSL-declared signal whose
    /// registered type matches `expected_ty`, return its raw `SignalId`.
    /// Used by the styling-arg lowering to redirect prop setters like
    /// `__set_overlay_opacity__` to their `_signal` counterparts so the
    /// overlay reads the live value through the reactive primitive at
    /// paint time instead of baking a snapshot.
    fn signal_id_for_variable(
        value: &zyntax_typed_ast::TypedNode<TypedExpression>,
        expected_ty: blinc_runtime::signal::SignalType,
    ) -> Option<u64> {
        // Shape A: bare Variable. User-declared top-level signals
        // (`signal foo: T` + `Div(opacity = foo)`) reach the styling
        // pass in this form because `resolve_signal_calls` leaves
        // user signals alone.
        if let TypedExpression::Variable(name) = &value.node {
            let name_str = name.resolve_global()?;
            let (id_raw, ty) = blinc_runtime::signal::lookup(&name_str)?;
            if ty != expected_ty {
                return None;
            }
            return Some(id_raw);
        }
        // Shape B: `__signal_get_by_id_<T>(<id_literal>)` call.
        // FSM-context signals (`Ticker.pct`) reach the styling pass
        // in this form because `resolve_signal_calls` force-wraps
        // every bare ctx-signal Variable into a typed getter call —
        // needed for action-body arithmetic + f-string interp where a
        // bare reference would be an undefined local. The wrap
        // collapses the value to a runtime getter; we still want the
        // STYLING side to route to the live `_signal__` setter, so
        // peel the wrap back to recover the raw id.
        if let TypedExpression::Call(c) = &value.node {
            let TypedExpression::Variable(callee) = &c.callee.node else {
                return None;
            };
            let callee_str = callee.resolve_global()?;
            let getter_ty = match (callee_str.as_ref(), expected_ty) {
                ("__signal_get_by_id_f64", blinc_runtime::signal::SignalType::F64) => {
                    blinc_runtime::signal::SignalType::F64
                }
                ("__signal_get_by_id_i32", blinc_runtime::signal::SignalType::I32) => {
                    blinc_runtime::signal::SignalType::I32
                }
                _ => return None,
            };
            let arg = c.positional_args.first()?;
            let TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(id_lit)) =
                &arg.node
            else {
                return None;
            };
            // The id literal carries the raw signal id as i64. Cast
            // back to u64 — same wire convention as the `_signal__`
            // extern's arg.
            let _ = getter_ty; // type already matched above
            return Some(*id_lit as i64 as u64);
        }
        None
    }

    /// Recognise `__blinc_computed_<T>__(closure_expr)` — the call
    /// shape `computed { … } : T` lowers to (per `grammar/blinc.zyn`).
    /// Returns `true` when the value is one of these calls AND the
    /// inner T matches what the styling prop expects.
    ///
    /// At runtime the call evaluates to a `DerivedId.to_raw() as i64`,
    /// which is exactly the payload the `_computed__` setters need.
    /// Mirrors the recognizer inside `lower_reactive_args`.
    fn is_computed_call_of_kind(
        value: &zyntax_typed_ast::TypedNode<TypedExpression>,
        kind: StylingValueKind,
    ) -> bool {
        let TypedExpression::Call(c) = &value.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &c.callee.node else {
            return false;
        };
        let want = match kind {
            StylingValueKind::Float => "__blinc_computed_f64__",
            StylingValueKind::IntColor => "__blinc_computed_i32__",
        };
        matches!(callee.resolve_global().as_deref(), Some(name) if name == want)
    }

    fn walk_stmt(stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>, counter: &mut u32) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, counter),
            TypedStatement::Return(Some(e)) => rewrite_expr(e, counter),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, counter);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, counter);
                for s in &mut if_stmt.then_block.statements {
                    walk_stmt(s, counter);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for s in &mut else_block.statements {
                        walk_stmt(s, counter);
                    }
                }
            }
            _ => {}
        }
    }

    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>, counter: &mut u32) {
        match &mut expr.node {
            TypedExpression::Call(call) => {
                rewrite_expr(&mut call.callee, counter);
                for arg in &mut call.positional_args {
                    rewrite_expr(arg, counter);
                }
                for na in &mut call.named_args {
                    rewrite_expr(&mut na.value, counter);
                }
            }
            TypedExpression::Array(items) => {
                for item in items {
                    rewrite_expr(item, counter);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    walk_stmt(stmt, counter);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, counter);
                rewrite_expr(&mut b.right, counter);
            }
            _ => {}
        }

        let TypedExpression::Call(call) = &mut expr.node else {
            return;
        };
        if !callee_is_styled_primitive(call) {
            return;
        }

        // Partition named args into styling args (consumed by
        // overlay setters) vs other args (left in place). We carry the
        // value-kind through so the redirect step below knows which
        // signal type to look for when a value is a bare Variable.
        let mut styling_args: Vec<(&'static str, StylingValueKind, TypedNamedArg)> = Vec::new();
        let mut remaining_named: Vec<TypedNamedArg> = Vec::new();
        let existing_named = std::mem::take(&mut call.named_args);
        for na in existing_named {
            let resolved = na.name.resolve_global();
            let name_str: Option<&str> = resolved.as_deref();
            if let Some(name) = name_str
                && let Some(entry) = STYLING_PROP_NAMES.iter().find(|(n, _, _)| *n == name)
            {
                styling_args.push((entry.1, entry.2, na));
                continue;
            }
            remaining_named.push(na);
        }

        let span = expr.span;
        let i64_ty = Type::Primitive(PrimitiveType::I64);
        let unit_ty = Type::Primitive(PrimitiveType::Unit);

        if styling_args.is_empty() {
            // Restore other named args and inject a null overlay
            // pointer so the call's `__style` slot is filled.
            call.named_args = remaining_named;
            call.named_args.push(TypedNamedArg {
                name: zyntax_typed_ast::InternedString::new_global("__style"),
                value: Box::new(typed_node(
                    TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
                    i64_ty.clone(),
                    span,
                )),
                span,
            });
            return;
        }

        // Allocate a unique ident for the overlay let-binding.
        let id = {
            let i = *counter;
            *counter += 1;
            i
        };
        let overlay_ident =
            zyntax_typed_ast::InternedString::new_global(&format!("__blinc_style_{id}"));

        let mut stmts: Vec<zyntax_typed_ast::TypedNode<TypedStatement>> = Vec::new();

        // let __blinc_style_N = __new_style_overlay__()
        stmts.push(typed_node(
            TypedStatement::Let(zyntax_typed_ast::typed_ast::TypedLet {
                name: overlay_ident,
                ty: i64_ty.clone(),
                mutability: zyntax_typed_ast::Mutability::Immutable,
                initializer: Some(Box::new(typed_node(
                    TypedExpression::Call(TypedCall {
                        callee: Box::new(typed_node(
                            TypedExpression::Variable(
                                zyntax_typed_ast::InternedString::new_global(
                                    "__new_style_overlay__",
                                ),
                            ),
                            Type::Any,
                            span,
                        )),
                        positional_args: vec![],
                        named_args: vec![],
                        type_args: vec![],
                    }),
                    i64_ty.clone(),
                    span,
                ))),
                span,
            }),
            unit_ty.clone(),
            span,
        ));

        // One setter call per styling arg. The arg's expression shape
        // picks which variant we emit:
        //
        //   * Bare signal identifier of the matching SignalType →
        //     `_signal__` variant, with the raw signal id as payload.
        //   * `computed { … } : T` call (already desugared to
        //     `__blinc_computed_<T>__(closure)` by `process_statement`)
        //     → `_computed__` variant, with the call expression itself
        //     as payload (it returns a `DerivedId.to_raw() as i64` at
        //     runtime).
        //   * Anything else → original literal-baking setter.
        //
        // Signal takes priority over computed only because the test
        // is cheaper; in practice each value matches at most one
        // shape so order doesn't change behaviour.
        for (setter_name, kind, na) in styling_args {
            let value_node = *na.value;
            let expected_signal_ty = match kind {
                StylingValueKind::Float => blinc_runtime::signal::SignalType::F64,
                StylingValueKind::IntColor => blinc_runtime::signal::SignalType::I32,
            };
            let signal_redirect =
                signal_id_for_variable(&value_node, expected_signal_ty).map(|id_raw_u64| {
                    // Derive the `_signal__` variant name by replacing
                    // the trailing `__` with `_signal__`. Every styling
                    // setter follows the `__set_overlay_*__` convention
                    // and has a registered `_signal__` peer in the abi
                    // table.
                    let signal_setter_name =
                        format!("{}_signal__", setter_name.trim_end_matches("__"));
                    let signal_setter =
                        zyntax_typed_ast::InternedString::new_global(&signal_setter_name);
                    let id_arg = typed_node(
                        TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(
                            id_raw_u64 as i64 as i128,
                        )),
                        i64_ty.clone(),
                        span,
                    );
                    (signal_setter, id_arg)
                });
            let (effective_setter, effective_arg) = match signal_redirect {
                Some((setter, arg)) => (setter, arg),
                None if is_computed_call_of_kind(&value_node, kind) => {
                    // Computed redirect. Same suffix-swap dance as the
                    // signal variant: `__set_overlay_X__` → `__set_overlay_X_computed__`.
                    // The arg keeps the original `__blinc_computed_<T>__`
                    // call expression — it already returns the raw
                    // derived id as i64 at runtime.
                    let computed_setter_name =
                        format!("{}_computed__", setter_name.trim_end_matches("__"));
                    let computed_setter =
                        zyntax_typed_ast::InternedString::new_global(&computed_setter_name);
                    (computed_setter, value_node)
                }
                None => (
                    zyntax_typed_ast::InternedString::new_global(setter_name),
                    value_node,
                ),
            };
            let setter_call = TypedExpression::Call(TypedCall {
                callee: Box::new(typed_node(
                    TypedExpression::Variable(effective_setter),
                    Type::Any,
                    span,
                )),
                positional_args: vec![
                    typed_node(
                        TypedExpression::Variable(overlay_ident),
                        i64_ty.clone(),
                        span,
                    ),
                    effective_arg,
                ],
                named_args: vec![],
                type_args: vec![],
            });
            stmts.push(typed_node(
                TypedStatement::Expression(Box::new(typed_node(
                    setter_call,
                    unit_ty.clone(),
                    span,
                ))),
                unit_ty.clone(),
                span,
            ));
        }

        // Trailing call: keep the original shape but attach
        // `__style = Var(__blinc_style_N)` so the named-args
        // resolution pass routes it to the right slot.
        call.named_args = remaining_named;
        call.named_args.push(TypedNamedArg {
            name: zyntax_typed_ast::InternedString::new_global("__style"),
            value: Box::new(typed_node(
                TypedExpression::Variable(overlay_ident),
                i64_ty.clone(),
                span,
            )),
            span,
        });

        // The Call expression itself is what closes the Block;
        // we extract a clone of the (now-modified) call to push
        // as the trailing Expression statement, then replace
        // `expr` with the Block.
        let final_call = TypedExpression::Call(TypedCall {
            callee: call.callee.clone(),
            positional_args: std::mem::take(&mut call.positional_args),
            named_args: std::mem::take(&mut call.named_args),
            type_args: std::mem::take(&mut call.type_args),
        });
        stmts.push(typed_node(
            TypedStatement::Expression(Box::new(typed_node(final_call, i64_ty.clone(), span))),
            i64_ty.clone(),
            span,
        ));

        expr.node = TypedExpression::Block(zyntax_typed_ast::typed_ast::TypedBlock {
            statements: stmts,
            span,
        });
    }

    let mut counter: u32 = 0;
    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = func.body.as_mut() {
                    for stmt in &mut body.statements {
                        walk_stmt(stmt, &mut counter);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = method.body.as_mut() {
                        for stmt in &mut body.statements {
                            walk_stmt(stmt, &mut counter);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Resolve named-arg calls to user-declared top-level functions.
/// `step(by = 5, x = 10)` lowers via the `bare_call_args_list`
/// grammar to `Call(Variable("step"), [__named__("by", 5),
/// __named__("x", 10)])`. This pass:
///
/// 1. Collects each top-level `TypedDeclaration::Function`'s
///    signature (param names + which have `default_value`s).
/// 2. Walks every Call(Variable(name), …) whose callee matches a
///    declared fn.
/// 3. Extracts `__named__(name, value)` markers from
///    `positional_args` into `named_args`.
/// 4. Builds slots[N] = function-arity slots, fills from
///    positional then named, splices each missing slot's declared
///    `default_value`.
/// 5. Writes the fully-positional list back to `positional_args`,
///    clears `named_args`.
///
/// Same shape as `resolve_extern_widget_named_args` but pulls
/// signatures from `TypedDeclaration::Function` instead of the
/// substrate `ComponentRegistry`. Without this pass, the
/// `__named__` markers leak into the JIT lowering as undefined
/// function calls.
pub(crate) fn lower_bare_call_named_args(program: &mut TypedProgram) {
    use std::collections::HashMap;
    use zyntax_typed_ast::InternedString;
    use zyntax_typed_ast::TypedNode;
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedExpression, TypedLiteral, TypedNamedArg,
    };

    // Step 1: collect signatures — each entry maps a fn name to
    // its ordered list of `(param_name, optional_default_clone)`.
    // Position is encoded by index into the inner Vec; the
    // slot-resolution step looks up params by name and inherits
    // the position from the Vec ordering.
    #[derive(Clone)]
    struct ParamInfo {
        default: Option<TypedNode<TypedExpression>>,
    }
    #[derive(Clone)]
    struct FnSig {
        params: Vec<(InternedString, ParamInfo)>,
    }
    let mut signatures: HashMap<InternedString, FnSig> = HashMap::new();
    for decl in &program.declarations {
        let TypedDeclaration::Function(func) = &decl.node else {
            continue;
        };
        if func.body.is_none() {
            continue;
        }
        // Skip extern decls (no body) and synthetic markers.
        if func.is_external {
            continue;
        }
        let Some(name_arc) = func.name.resolve_global() else {
            continue;
        };
        if name_arc.starts_with("__") {
            continue;
        }
        let mut params: Vec<(InternedString, ParamInfo)> = Vec::new();
        for p in &func.params {
            let default = p.default_value.as_ref().map(|d| (**d).clone());
            params.push((p.name, ParamInfo { default }));
        }
        signatures.insert(func.name, FnSig { params });
    }
    if signatures.is_empty() {
        return;
    }

    // Step 2 + 3 + 4: visitor that walks every Call(Variable(name)),
    // extracts __named__ markers, slot-resolves against the signature.
    struct Lowerer<'a> {
        signatures: &'a HashMap<InternedString, FnSig>,
    }
    impl Lowerer<'_> {
        fn rewrite_expr(&self, expr: &mut TypedNode<TypedExpression>) {
            match &mut expr.node {
                TypedExpression::Call(call) => {
                    self.rewrite_expr(&mut call.callee);
                    for a in &mut call.positional_args {
                        self.rewrite_expr(a);
                    }
                    for na in &mut call.named_args {
                        self.rewrite_expr(&mut na.value);
                    }
                }
                TypedExpression::Binary(b) => {
                    self.rewrite_expr(&mut b.left);
                    self.rewrite_expr(&mut b.right);
                }
                TypedExpression::Unary(u) => self.rewrite_expr(&mut u.operand),
                TypedExpression::Field(f) => self.rewrite_expr(&mut f.object),
                TypedExpression::Index(idx) => {
                    self.rewrite_expr(&mut idx.object);
                    self.rewrite_expr(&mut idx.index);
                }
                TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                    for it in items {
                        self.rewrite_expr(it);
                    }
                }
                TypedExpression::MethodCall(mc) => {
                    self.rewrite_expr(&mut mc.receiver);
                    for a in &mut mc.positional_args {
                        self.rewrite_expr(a);
                    }
                }
                TypedExpression::Block(b) => self.rewrite_block(b),
                TypedExpression::If(if_expr) => {
                    self.rewrite_expr(&mut if_expr.condition);
                    self.rewrite_expr(&mut if_expr.then_branch);
                    self.rewrite_expr(&mut if_expr.else_branch);
                }
                TypedExpression::Lambda(lam) => match &mut lam.body {
                    zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                        self.rewrite_expr(e);
                    }
                    zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                        self.rewrite_block(block);
                    }
                },
                _ => {}
            }
            // After recursing into children, try to lower THIS expression
            // if it's a known bare fn call.
            self.try_resolve(expr);
        }

        fn try_resolve(&self, expr: &mut TypedNode<TypedExpression>) {
            let TypedExpression::Call(call) = &mut expr.node else {
                return;
            };
            // Only Variable callees — skip Field / MethodCall / etc.
            let TypedExpression::Variable(callee_name) = &call.callee.node else {
                return;
            };
            let Some(sig) = self.signatures.get(callee_name) else {
                return;
            };

            // Extract __named__ markers from positional_args.
            let mut positionals: Vec<TypedNode<TypedExpression>> = Vec::new();
            let mut named: Vec<TypedNamedArg> = std::mem::take(&mut call.named_args);
            for arg in std::mem::take(&mut call.positional_args) {
                if let TypedExpression::Call(inner) = &arg.node
                    && let TypedExpression::Variable(inner_callee) = &inner.callee.node
                    && inner_callee.resolve_global().as_deref() == Some("__named__")
                    && inner.positional_args.len() == 2
                    && let TypedExpression::Literal(TypedLiteral::String(arg_name)) =
                        &inner.positional_args[0].node
                {
                    named.push(TypedNamedArg {
                        name: *arg_name,
                        value: Box::new(inner.positional_args[1].clone()),
                        span: arg.span,
                    });
                    continue;
                }
                positionals.push(arg);
            }

            // No named args AND positional count matches → nothing to
            // splice. Restore positional_args verbatim and bail.
            if named.is_empty() && positionals.len() == sig.params.len() {
                call.positional_args = positionals;
                return;
            }
            // Also bail when the call has neither named args nor missing
            // trailing slots — `step(5)` against `fn step(x, by = 1)`
            // is the "splice default" case Zyntax already handles
            // natively (see `dsl_fn_default_param_splices_when_omitted_at_call_site`).
            // We only intervene when there's something to actually resolve.
            if named.is_empty() {
                call.positional_args = positionals;
                return;
            }

            // Slot-fill: positional first (by index), then named (by
            // name), then defaults for any still-empty slot.
            let mut slots: Vec<Option<TypedNode<TypedExpression>>> =
                (0..sig.params.len()).map(|_| None).collect();
            for (i, arg) in positionals.into_iter().enumerate() {
                if i < slots.len() {
                    slots[i] = Some(arg);
                }
                // Excess positionals are dropped; type-checker / arity
                // check would catch that as a separate error.
            }
            for na in named {
                if let Some(pos) = sig.params.iter().position(|(n, _)| *n == na.name) {
                    // Last-write-wins if a positional already filled
                    // this slot — matches Rust/Python semantics where
                    // `foo(x = 5)` with `x` already positional is a
                    // duplicate-arg error. We accept silently here;
                    // a stricter diagnostic is a follow-up.
                    slots[pos] = Some(*na.value);
                }
                // Unknown name → silently drop. Future: emit a
                // BLINC-NAMED-ARG-UNKNOWN diagnostic.
            }
            let mut new_positional: Vec<TypedNode<TypedExpression>> =
                Vec::with_capacity(slots.len());
            for (slot, (_, info)) in slots.into_iter().zip(sig.params.iter()) {
                if let Some(arg) = slot {
                    new_positional.push(arg);
                } else if let Some(default) = &info.default {
                    new_positional.push(default.clone());
                } else {
                    // Missing required arg with no default — let the
                    // downstream lowering surface the arity error.
                    break;
                }
            }
            call.positional_args = new_positional;
            // named_args was drained into `named`; leave it empty.
        }

        fn rewrite_block(&self, block: &mut zyntax_typed_ast::typed_ast::TypedBlock) {
            for stmt in &mut block.statements {
                self.rewrite_stmt(stmt);
            }
        }

        fn rewrite_stmt(&self, stmt: &mut TypedNode<TypedStatement>) {
            match &mut stmt.node {
                TypedStatement::Expression(e) => self.rewrite_expr(e),
                TypedStatement::Return(Some(e)) => self.rewrite_expr(e),
                TypedStatement::Let(l) => {
                    if let Some(init) = &mut l.initializer {
                        self.rewrite_expr(init);
                    }
                }
                TypedStatement::If(if_stmt) => {
                    self.rewrite_expr(&mut if_stmt.condition);
                    self.rewrite_block(&mut if_stmt.then_block);
                    if let Some(else_block) = &mut if_stmt.else_block {
                        self.rewrite_block(else_block);
                    }
                }
                TypedStatement::While(w) => {
                    self.rewrite_expr(&mut w.condition);
                    self.rewrite_block(&mut w.body);
                }
                TypedStatement::Block(b) => self.rewrite_block(b),
                _ => {}
            }
        }
    }

    let lowerer = Lowerer {
        signatures: &signatures,
    };
    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    lowerer.rewrite_block(body);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        lowerer.rewrite_block(body);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Positionalise named args on `$Blinc$<X>$view` calls using the substrate
/// `ComponentRegistry` prop order. Zyntax's auto-injected extern decls carry
/// synthetic param names (`p0`, `p1`, …) that can't bind by name. Skipped
/// for user-declared components (handled by `bind_component_props`).
pub(crate) fn resolve_extern_widget_named_args(program: &mut TypedProgram) {
    use zyntax_typed_ast::{TypedCall, TypedDeclaration, TypedExpression};

    fn walk_stmt(stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e),
            TypedStatement::Return(Some(e)) => rewrite_expr(e),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition);
                for s in &mut if_stmt.then_block.statements {
                    walk_stmt(s);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for s in &mut else_block.statements {
                        walk_stmt(s);
                    }
                }
            }
            _ => {}
        }
    }

    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>) {
        // Recurse first — nested calls resolved before the outer.
        match &mut expr.node {
            TypedExpression::Call(call) => {
                rewrite_expr(&mut call.callee);
                for arg in &mut call.positional_args {
                    rewrite_expr(arg);
                }
                for na in &mut call.named_args {
                    rewrite_expr(&mut na.value);
                }
            }
            TypedExpression::Array(items) => {
                for item in items {
                    rewrite_expr(item);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    walk_stmt(stmt);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left);
                rewrite_expr(&mut b.right);
            }
            _ => {}
        }

        let span = expr.span;
        let TypedExpression::Call(call) = &mut expr.node else {
            return;
        };
        let Some(props) = primitive_callee_props(call) else {
            return;
        };
        // Don't early-out on empty named_args — trailing slots still need defaults
        // so call arity matches the extern signature.

        let mut slots: Vec<Option<zyntax_typed_ast::TypedNode<TypedExpression>>> =
            (0..props.len()).map(|_| None).collect();
        let existing_positional = std::mem::take(&mut call.positional_args);
        let mut overflow: Vec<zyntax_typed_ast::TypedNode<TypedExpression>> = Vec::new();
        for (i, arg) in existing_positional.into_iter().enumerate() {
            if i < slots.len() {
                slots[i] = Some(coerce_extern_arg_for_prop(arg, &props[i].1));
            } else {
                overflow.push(arg);
            }
        }

        let existing_named = std::mem::take(&mut call.named_args);
        let mut unresolved_named: Vec<zyntax_typed_ast::TypedNamedArg> = Vec::new();
        for na in existing_named {
            let Some(name) = na.name.resolve_global() else {
                unresolved_named.push(na);
                continue;
            };
            let name_str: &str = &name;
            if let Some(pos) = props.iter().position(|(n, _)| n == name_str) {
                if slots[pos].is_some() {
                    unresolved_named.push(na);
                } else {
                    slots[pos] = Some(coerce_extern_arg_for_prop(*na.value, &props[pos].1));
                }
            } else {
                unresolved_named.push(na);
            }
        }

        // Fill unfilled slots with type-appropriate defaults so call arity matches.
        let mut new_positional: Vec<zyntax_typed_ast::TypedNode<TypedExpression>> =
            Vec::with_capacity(slots.len());
        for (slot, (_, ty)) in slots.into_iter().zip(props.iter()) {
            if let Some(arg) = slot {
                new_positional.push(arg);
            } else {
                new_positional.push(default_literal_for(ty, span));
            }
        }
        new_positional.extend(overflow);

        call.positional_args = new_positional;
        call.named_args = unresolved_named;
    }

    /// Bool-like widget props keep an `i32` extern ABI slot, so a DSL
    /// `true`/`false` literal is lowered to `1`/`0` before the final call.
    fn coerce_extern_arg_for_prop(
        arg: zyntax_typed_ast::TypedNode<TypedExpression>,
        ty: &Type,
    ) -> zyntax_typed_ast::TypedNode<TypedExpression> {
        if !matches!(ty, Type::Primitive(PrimitiveType::Bool)) {
            return arg;
        }
        let span = arg.span;
        let arg_ty = arg.ty;
        match arg.node {
            TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Bool(value)) => typed_node(
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(if value {
                    1
                } else {
                    0
                })),
                Type::Primitive(PrimitiveType::I32),
                span,
            ),
            other if matches!(&arg_ty, Type::Primitive(PrimitiveType::Bool)) => {
                let bool_expr = typed_node(other, Type::Primitive(PrimitiveType::Bool), span);
                typed_node(
                    TypedExpression::If(zyntax_typed_ast::typed_ast::TypedIfExpr {
                        condition: Box::new(bool_expr),
                        then_branch: Box::new(typed_node(
                            TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(1)),
                            Type::Primitive(PrimitiveType::I32),
                            span,
                        )),
                        else_branch: Box::new(typed_node(
                            TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
                            Type::Primitive(PrimitiveType::I32),
                            span,
                        )),
                    }),
                    Type::Primitive(PrimitiveType::I32),
                    span,
                )
            }
            other => typed_node(other, arg_ty, span),
        }
    }

    /// Substrate primitive's prop (name, type) pairs in declaration order.
    fn primitive_callee_props(call: &TypedCall) -> Option<Vec<(String, Type)>> {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return None;
        };
        let sym = callee.resolve_global()?;
        let sym: &str = &sym;
        let name = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))?;
        blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name).map(|def| {
                def.props
                    .iter()
                    .map(|p| (p.name.to_string(), p.ty.clone()))
                    .collect()
            })
        })
    }

    /// Default literal for an unsupplied prop slot (`0` / `0.0` / `""`).
    fn default_literal_for(
        ty: &Type,
        span: zyntax_typed_ast::Span,
    ) -> zyntax_typed_ast::TypedNode<TypedExpression> {
        match ty {
            Type::Primitive(PrimitiveType::Bool) => typed_node(
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
                Type::Primitive(PrimitiveType::I32),
                span,
            ),
            Type::Primitive(PrimitiveType::F64) => typed_node(
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Float(0.0)),
                ty.clone(),
                span,
            ),
            Type::Primitive(PrimitiveType::String) => typed_node(
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::String(
                    zyntax_typed_ast::InternedString::new_global(""),
                )),
                ty.clone(),
                span,
            ),
            _ => typed_node(
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
                ty.clone(),
                span,
            ),
        }
    }

    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = func.body.as_mut() {
                    for stmt in &mut body.statements {
                        walk_stmt(stmt);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = method.body.as_mut() {
                        for stmt in &mut body.statements {
                            walk_stmt(stmt);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Append `Return(None)` to user fns so the body classifier can't promote a
/// trailing Expression into a value-bearing return.
pub(crate) fn ensure_unit_return(program: &mut TypedProgram) {
    use zyntax_typed_ast::TypedDeclaration;

    fn add_trailing_return_if_missing(body: &mut zyntax_typed_ast::typed_ast::TypedBlock) {
        let trailing_is_return = matches!(
            body.statements.last().map(|s| &s.node),
            Some(TypedStatement::Return(_))
        );
        if !trailing_is_return {
            body.statements.push(typed_node(
                TypedStatement::Return(None),
                Type::Primitive(PrimitiveType::Unit),
                Span::default(),
            ));
        }
    }

    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if func.is_external {
                    continue;
                }
                if let Some(body) = func.body.as_mut() {
                    add_trailing_return_if_missing(body);
                }
            }
            // Impl methods compile to `<TypeName>$<method>` free fns — need the
            // same `Return(None)` so `call::<()>` doesn't hit the value-return path.
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = method.body.as_mut() {
                        add_trailing_return_if_missing(body);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Prepend a path-derived `u64` call-site key as the leading positional
/// argument to every widget-view call (substrate primitives + user
/// components).
///
/// **Substrate primitives** (e.g. `$Blinc$Button$view`): widget FFIs
/// consume the leading u64 as the state-allocation seed via
/// `dsl_state_key`. Dup-labelled Buttons at distinct call sites hold
/// distinct state because their span hashes differ.
///
/// **User components** (e.g. `Counter$view`): the `__instance_id__: u64`
/// param injected by [`inject_user_view_instance_id_params`] catches
/// the value; downstream calls inside Counter's view body emit
/// `__instance_id__ ^ LOCAL_HASH` instead of just `LOCAL_HASH`, so two
/// `Counter()` invocations produce sub-trees with distinct keys even
/// though Counter's body source is shared.
///
/// The XOR composition is the runtime piece that makes the
/// shared-body case work — `LOCAL_HASH` alone is identical across all
/// Counter instances (same source position), but XOR'd with the
/// caller's distinct `__instance_id__`, the composed key is per-instance.
///
/// MUST run AFTER `lower_children_arrays_to_blocks` so we walk the
/// final shape of widget calls including those that were moved into
/// `__push_child__` arg positions during children-array expansion.
pub(crate) fn inject_call_site_keys(program: &mut TypedProgram, filename: &str) {
    use zyntax_typed_ast::typed_ast::{
        TypedBinary, TypedDeclaration, TypedExpression, TypedLiteral,
    };

    /// Is `callee_name` a substrate-primitive view symbol (auto-injected
    /// leading u64; FFI consumes it)?
    fn is_substrate_view_symbol(callee_name: &str) -> bool {
        crate::abi::is_substrate_widget_view_public(callee_name)
    }

    /// Is `callee_name` a DSL-declared user-component view symbol
    /// (auto-prepended `__instance_id__` param by
    /// [`inject_user_view_instance_id_params`])?
    ///
    /// Heuristic: ends with `$view`, doesn't start with `$Blinc$`, and
    /// is in the component registry. This correctly excludes
    /// externally-registered widgets via `register_extern_widget_spec`
    /// (whose view_symbols start with `$Blinc$` by convention but
    /// whose Rust FFIs don't have the auto-injected leading u64).
    fn is_user_view_symbol(callee_name: &str) -> bool {
        if !callee_name.ends_with("$view") || callee_name.starts_with("$Blinc$") {
            return false;
        }
        let bare = match callee_name.strip_suffix("$view") {
            Some(s) => s,
            None => return false,
        };
        blinc_runtime::component::with_component_registry(|r| r.get_by_name(bare).is_some())
    }

    /// Does this Call need a leading call-site key injected? Either kind
    /// of view symbol qualifies.
    fn needs_call_id_injection(callee_name: &str) -> bool {
        is_substrate_view_symbol(callee_name) || is_user_view_symbol(callee_name)
    }

    /// Walker context — tracks whether we're inside a user-component
    /// view body (where injected keys must XOR with `__instance_id__`).
    struct Ctx<'a> {
        filename: &'a str,
        in_user_view: bool,
    }

    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>, ctx: &Ctx<'_>) {
        match &mut expr.node {
            TypedExpression::Call(call) => {
                rewrite_expr(&mut call.callee, ctx);
                for a in &mut call.positional_args {
                    rewrite_expr(a, ctx);
                }
                for n in &mut call.named_args {
                    rewrite_expr(&mut n.value, ctx);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, ctx);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, ctx);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, ctx);
                rewrite_expr(&mut b.right, ctx);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand, ctx),
            TypedExpression::Field(f) => rewrite_expr(&mut f.object, ctx),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, ctx);
                rewrite_expr(&mut idx.index, ctx);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it, ctx);
                }
            }
            TypedExpression::Struct(s) => {
                for field in &mut s.fields {
                    rewrite_expr(&mut field.value, ctx);
                }
            }
            TypedExpression::Block(b) => rewrite_block(b, ctx),
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, ctx);
                rewrite_expr(&mut if_expr.then_branch, ctx);
                rewrite_expr(&mut if_expr.else_branch, ctx);
            }
            _ => {}
        }

        // Inject leading u64 key if this Call's callee is a view symbol
        // (substrate primitive OR user component). Use the call
        // expression's OWN span as the key source — that's the unique
        // source-location of this invocation.
        let span = expr.span;
        let TypedExpression::Call(call) = &mut expr.node else {
            return;
        };
        let TypedExpression::Variable(callee_name) = &call.callee.node else {
            return;
        };
        let Some(resolved) = callee_name.resolve_global() else {
            return;
        };
        if !needs_call_id_injection(resolved.as_ref()) {
            return;
        }

        // Build a path-shaped key: `ComponentName[.className]:hex_offset`.
        // The `class` arg, when present and a string literal, contributes
        // to identity — `Button(class="hero")` and `Button(class="cta")`
        // at the same source position diverge. Look it up via the
        // component registry's prop list, which gives us the position
        // of the `class` slot for this widget.
        let component_name = strip_view_suffix(resolved.as_ref()).unwrap_or("");
        let class_name = extract_class_arg(call, resolved.as_ref());
        let key = call_site_path_id(
            ctx.filename,
            span.start,
            component_name,
            class_name.as_deref(),
            None, // id args aren't a thing on substrate primitives today
        );

        // Cranelift backend (zyntax-compiler) doesn't handle
        // `HirConstant::U64` in its `value_map` population step — the
        // match at `cranelift_backend.rs:1471-1499` only knows about
        // I8/I16/I32/U32/I64/Bool/F32/F64 and silently `continue`s
        // for anything else. We type the literal as I64 (same bit-
        // width, same calling-convention slot) and let the abi.rs side
        // tag the param as TypeTag::U64 — the int is reinterpreted as
        // u64 on the Rust receive side without any value mangling.
        let literal = zyntax_typed_ast::TypedNode::new(
            TypedExpression::Literal(TypedLiteral::Integer(key as i64 as i128)),
            Type::Primitive(PrimitiveType::I64),
            span,
        );

        // When INSIDE a user-component view body, the leading arg is
        // `__instance_id__ ^ LOCAL_LITERAL` so the caller's distinct
        // instance id distinguishes each Counter() invocation's
        // sub-tree from another's. At the TOP-LEVEL view body (or any
        // non-user-view function), there's no `__instance_id__` in
        // scope; the literal stands alone.
        let key_arg = if ctx.in_user_view {
            let instance_id_var = zyntax_typed_ast::TypedNode::new(
                TypedExpression::Variable(zyntax_typed_ast::InternedString::new_global(
                    "__instance_id__",
                )),
                Type::Primitive(PrimitiveType::I64),
                span,
            );
            let xor_expr = TypedExpression::Binary(TypedBinary {
                op: zyntax_typed_ast::typed_ast::BinaryOp::BitXor,
                left: Box::new(instance_id_var),
                right: Box::new(literal),
            });
            zyntax_typed_ast::TypedNode::new(xor_expr, Type::Primitive(PrimitiveType::I64), span)
        } else {
            literal
        };
        call.positional_args.insert(0, key_arg);
    }

    /// Strip the `$Blinc$` prefix (if any) and the `$view` suffix from
    /// a view symbol to recover the bare component name. Returns `None`
    /// if the symbol doesn't have the expected shape.
    fn strip_view_suffix(view_symbol: &str) -> Option<&str> {
        let stripped = view_symbol.strip_suffix("$view")?;
        Some(stripped.strip_prefix("$Blinc$").unwrap_or(stripped))
    }

    /// Find the `class` arg in `call`'s positional_args and return its
    /// string-literal value, if present. The arg's POSITION is looked
    /// up from the component registry's prop list — `class` is usually
    /// the 3rd or 4th positional for substrate primitives but the exact
    /// index varies (e.g. `Image` has no class slot).
    fn extract_class_arg(
        call: &zyntax_typed_ast::typed_ast::TypedCall,
        callee_name: &str,
    ) -> Option<String> {
        let component_name = strip_view_suffix(callee_name)?;
        let class_idx = blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(component_name)
                .and_then(|def| def.props.iter().position(|p| p.name.as_ref() == "class"))
        })?;
        let class_arg = call.positional_args.get(class_idx)?;
        let TypedExpression::Literal(TypedLiteral::String(s)) = &class_arg.node else {
            return None;
        };
        s.resolve_global().map(|s| s.to_string())
    }

    fn rewrite_block(block: &mut zyntax_typed_ast::typed_ast::TypedBlock, ctx: &Ctx<'_>) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, ctx);
        }
    }

    fn rewrite_stmt(stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>, ctx: &Ctx<'_>) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, ctx),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, ctx);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, ctx),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, ctx);
                rewrite_block(&mut if_stmt.then_block, ctx);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, ctx);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, ctx);
                rewrite_block(&mut w.body, ctx);
            }
            TypedStatement::Block(b) => rewrite_block(b, ctx),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            // Top-level functions — `render_view`, plus any user helpers.
            // None of these have `__instance_id__` in scope.
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    let ctx = Ctx {
                        filename,
                        in_user_view: false,
                    };
                    rewrite_block(body, &ctx);
                }
            }
            // Impl methods. A method named `view` is the user-component
            // view body — `__instance_id__` IS in scope as the leading
            // synthetic param, so children inside the body must XOR
            // with it. Other methods (init, helpers) walk with
            // in_user_view=false.
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        let in_user_view = method.name.resolve_global().as_deref() == Some("view");
                        let ctx = Ctx {
                            filename,
                            in_user_view,
                        };
                        rewrite_block(body, &ctx);
                    }
                }
            }
            _ => {}
        }
    }
}

// =====================================================================
// FSM context + transition actions
// =====================================================================
//
// The grammar emits three new shapes when an FSM uses extended state:
//
//   1. `__fsm_context_field__("name", "ty", default_literal)` markers
//      inside `__fsm_meta__` — one per `context { … }` field.
//   2. `__fsm_transition__("from", "event", "to", Block { stmts })` —
//      the optional 4th positional arg carries the action body.
//   3. `__compound_assign__("Add", lhs, rhs)` marker calls produced by
//      `+=` / `-=` / `*=` / `/=` (works everywhere statements appear,
//      not just inside FSM bodies).
//
// The lowering pipeline turns these into ordinary Blinc machinery:
//   - Context fields become top-level `signal __fsm_ctx_<Fsm>_<field>: <ty>`
//     decls so `resolve_signal_calls` handles get/set uniformly.
//   - Action bodies get lifted to top-level fns
//     `__fsm_action_<Fsm>_<idx>__` whose body has `ctx.<field>` rewritten
//     to the mangled signal name. The `__fsm_transition__` marker's 4th
//     arg is rewritten from Block to a string-literal carrying the lifted
//     symbol name so `populate_fsm_registry_pass` reads it as
//     `TransitionAction::Symbol(...)`.
//   - Tick-guard expressions go through the same `ctx.<field>` rewrite.
//   - Compound-assign markers expand to plain `target = target op value`
//     (with the LHS cloned).
//   - From-outside dotted access `<Fsm>.<field>` (typically followed by
//     `.get()` / `.set(...)` or appearing on either side of an `=`) is
//     rewritten to the mangled signal identifier so `resolve_signal_calls`
//     picks it up.

/// Desugar `__compound_assign__("Add", lhs, rhs)` marker calls into
/// `lhs = lhs <op> rhs`. Runs early so subsequent passes only see plain
/// `Binary Assign` shapes — no special-casing of `+=` etc. downstream.
///
/// Walks every expression position reachable from the program's
/// declarations, including inside lambda bodies and nested blocks. The
/// LHS is cloned to share between the outer Assign and the inner Binary
/// arithmetic — TypedNodes are owned trees, no aliasing concerns.
pub(crate) fn desugar_compound_assigns(program: &mut TypedProgram) {
    use zyntax_typed_ast::TypedNode;
    use zyntax_typed_ast::typed_ast::{
        BinaryOp, TypedBinary, TypedCall, TypedDeclaration, TypedExpression, TypedLambdaBody,
        TypedLiteral,
    };

    fn op_from_str(s: &str) -> Option<BinaryOp> {
        match s {
            "+=" => Some(BinaryOp::Add),
            "-=" => Some(BinaryOp::Sub),
            "*=" => Some(BinaryOp::Mul),
            "/=" => Some(BinaryOp::Div),
            _ => None,
        }
    }

    fn rewrite_expr(node: &mut TypedNode<TypedExpression>) {
        // Walk children first so nested compound assigns inside arg
        // expressions are handled before the outer is examined.
        match &mut node.node {
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee);
                for a in &mut c.positional_args {
                    rewrite_expr(a);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver);
                for a in &mut mc.positional_args {
                    rewrite_expr(a);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left);
                rewrite_expr(&mut b.right);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand),
            TypedExpression::Field(f) => rewrite_expr(&mut f.object),
            TypedExpression::Index(i) => {
                rewrite_expr(&mut i.object);
                rewrite_expr(&mut i.index);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    rewrite_stmt(stmt);
                }
            }
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition);
                rewrite_expr(&mut if_expr.then_branch);
                rewrite_expr(&mut if_expr.else_branch);
            }
            TypedExpression::Lambda(lam) => match &mut lam.body {
                TypedLambdaBody::Expression(e) => rewrite_expr(e),
                TypedLambdaBody::Block(block) => {
                    for stmt in &mut block.statements {
                        rewrite_stmt(stmt);
                    }
                }
            },
            _ => {}
        }

        // Now look for `__compound_assign__(op_str, lhs, rhs)` at this
        // node and rewrite in place.
        let TypedExpression::Call(call) = &node.node else {
            return;
        };
        let TypedExpression::Variable(callee_name) = &call.callee.node else {
            return;
        };
        if callee_name.resolve_global().as_deref() != Some("__compound_assign__") {
            return;
        }
        if call.positional_args.len() != 3 {
            return;
        }
        let TypedExpression::Literal(TypedLiteral::String(op_intern)) =
            &call.positional_args[0].node
        else {
            return;
        };
        let Some(op_str) = op_intern.resolve_global() else {
            return;
        };
        let Some(op) = op_from_str(&op_str) else {
            return;
        };
        let lhs = call.positional_args[1].clone();
        let rhs = call.positional_args[2].clone();
        let span = node.span;
        let lhs_for_rhs = lhs.clone();
        let inner_ty = lhs.ty.clone();
        let combined = TypedNode::new(
            TypedExpression::Binary(TypedBinary {
                op,
                left: Box::new(lhs_for_rhs),
                right: Box::new(rhs),
            }),
            inner_ty,
            span,
        );
        node.node = TypedExpression::Binary(TypedBinary {
            op: BinaryOp::Assign,
            left: Box::new(lhs),
            right: Box::new(combined),
        });
        node.ty = Type::Primitive(PrimitiveType::Unit);
        // Silence "unused" lints on imports only used through pattern matches above.
        let _ = std::any::type_name::<TypedCall>();
    }

    fn rewrite_stmt(stmt: &mut TypedNode<TypedStatement>) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e),
            TypedStatement::Block(b) => {
                for inner in &mut b.statements {
                    rewrite_stmt(inner);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition);
                for inner in &mut if_stmt.then_block.statements {
                    rewrite_stmt(inner);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for inner in &mut else_block.statements {
                        rewrite_stmt(inner);
                    }
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition);
                for inner in &mut w.body.statements {
                    rewrite_stmt(inner);
                }
            }
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(f) => {
                if let Some(body) = &mut f.body {
                    for stmt in &mut body.statements {
                        rewrite_stmt(stmt);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for m in &mut imp.methods {
                    if let Some(body) = &mut m.body {
                        for stmt in &mut body.statements {
                            rewrite_stmt(stmt);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Rewrite `ctx.<field>` → `<mangled_signal_name>` inside the supplied
/// block. Used by both the action-body lifter and the tick-guard
/// expression handler so the two paths share one resolution rule.
///
/// `Field { object: Variable("ctx"), field: <name> }` → `Variable(__fsm_ctx_<Fsm>_<name>)`.
///
/// Any other appearance of bare `ctx` (e.g. `let x = ctx`,
/// `someFn(ctx)`) is left untouched — downstream resolution will error
/// out because `ctx` isn't a real binding. We rely on that to surface
/// misuse rather than implementing a separate check here.
fn rewrite_fsm_ctx_access_block(
    fsm_name: &str,
    block: &mut zyntax_typed_ast::typed_ast::TypedBlock,
) {
    for stmt in &mut block.statements {
        rewrite_fsm_ctx_access_stmt(fsm_name, stmt);
    }
}

fn rewrite_fsm_ctx_access_stmt(
    fsm_name: &str,
    stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
) {
    match &mut stmt.node {
        TypedStatement::Expression(e) => rewrite_fsm_ctx_access_expr(fsm_name, e),
        TypedStatement::Let(l) => {
            if let Some(init) = &mut l.initializer {
                rewrite_fsm_ctx_access_expr(fsm_name, init);
            }
        }
        TypedStatement::Return(Some(e)) => rewrite_fsm_ctx_access_expr(fsm_name, e),
        TypedStatement::Block(b) => rewrite_fsm_ctx_access_block(fsm_name, b),
        TypedStatement::If(if_stmt) => {
            rewrite_fsm_ctx_access_expr(fsm_name, &mut if_stmt.condition);
            rewrite_fsm_ctx_access_block(fsm_name, &mut if_stmt.then_block);
            if let Some(else_block) = &mut if_stmt.else_block {
                rewrite_fsm_ctx_access_block(fsm_name, else_block);
            }
        }
        TypedStatement::While(w) => {
            rewrite_fsm_ctx_access_expr(fsm_name, &mut w.condition);
            rewrite_fsm_ctx_access_block(fsm_name, &mut w.body);
        }
        _ => {}
    }
}

fn rewrite_fsm_ctx_access_expr(
    fsm_name: &str,
    expr: &mut zyntax_typed_ast::TypedNode<zyntax_typed_ast::typed_ast::TypedExpression>,
) {
    use zyntax_typed_ast::InternedString;
    use zyntax_typed_ast::typed_ast::{TypedExpression, TypedLambdaBody};

    // Walk children first so deeper accesses are rewritten before the
    // outer node is inspected.
    match &mut expr.node {
        TypedExpression::Call(c) => {
            rewrite_fsm_ctx_access_expr(fsm_name, &mut c.callee);
            for a in &mut c.positional_args {
                rewrite_fsm_ctx_access_expr(fsm_name, a);
            }
        }
        TypedExpression::MethodCall(mc) => {
            rewrite_fsm_ctx_access_expr(fsm_name, &mut mc.receiver);
            for a in &mut mc.positional_args {
                rewrite_fsm_ctx_access_expr(fsm_name, a);
            }
        }
        TypedExpression::Binary(b) => {
            rewrite_fsm_ctx_access_expr(fsm_name, &mut b.left);
            rewrite_fsm_ctx_access_expr(fsm_name, &mut b.right);
        }
        TypedExpression::Unary(u) => {
            rewrite_fsm_ctx_access_expr(fsm_name, &mut u.operand);
        }
        TypedExpression::Field(f) => {
            // Recurse into the object first — handles
            // `something.ctx.field` shapes if they ever appear; mostly
            // a no-op since `ctx` is leftmost.
            rewrite_fsm_ctx_access_expr(fsm_name, &mut f.object);
        }
        TypedExpression::Index(i) => {
            rewrite_fsm_ctx_access_expr(fsm_name, &mut i.object);
            rewrite_fsm_ctx_access_expr(fsm_name, &mut i.index);
        }
        TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
            for it in items {
                rewrite_fsm_ctx_access_expr(fsm_name, it);
            }
        }
        TypedExpression::Block(block) => {
            rewrite_fsm_ctx_access_block(fsm_name, block);
        }
        TypedExpression::If(if_expr) => {
            rewrite_fsm_ctx_access_expr(fsm_name, &mut if_expr.condition);
            rewrite_fsm_ctx_access_expr(fsm_name, &mut if_expr.then_branch);
            rewrite_fsm_ctx_access_expr(fsm_name, &mut if_expr.else_branch);
        }
        TypedExpression::Lambda(lam) => match &mut lam.body {
            TypedLambdaBody::Expression(e) => rewrite_fsm_ctx_access_expr(fsm_name, e),
            TypedLambdaBody::Block(block) => rewrite_fsm_ctx_access_block(fsm_name, block),
        },
        _ => {}
    }

    // Now look at THIS node: is it `Field { object: Variable("ctx"), field: <name> }`?
    let is_ctx_field = match &expr.node {
        TypedExpression::Field(f) => match &f.object.node {
            TypedExpression::Variable(name) => name.resolve_global().as_deref() == Some("ctx"),
            _ => false,
        },
        _ => false,
    };
    if !is_ctx_field {
        return;
    }
    let TypedExpression::Field(f) = &expr.node else {
        return;
    };
    let Some(field_name) = f.field.resolve_global() else {
        return;
    };
    let mangled = super::fsm_registry::mangle_ctx_signal(fsm_name, &field_name);
    let span = expr.span;
    let ty = expr.ty.clone();
    expr.node = TypedExpression::Variable(InternedString::new_global(&mangled));
    expr.ty = ty;
    expr.span = span;
}

/// Early FSM-context pass: scans each `__fsm_meta__` body for
/// `__fsm_context_field__` and `__fsm_transition__` markers; emits
/// top-level signal declarations for context fields; lifts action
/// bodies (4th-positional `Block` on `__fsm_transition__`) to
/// top-level fns `__fsm_action_<Fsm>_<idx>__`; rewrites `ctx.<field>`
/// access inside each lifted body to the mangled signal name. Also
/// applies the ctx-rewrite to tick-guard expressions
/// (`__fsm_tick__("from", <guard>, "to")` arg 1) so guards can read
/// context like actions can.
///
/// Side effects on `__fsm_meta__`:
///   - `__fsm_context_field__` markers are LEFT in place so the
///     publish-step can read them.
///   - `__fsm_transition__` markers with a 4th-arg Block are rewritten:
///     the Block is replaced by a `StringLiteral(<lifted_symbol_name>)`.
///     `populate_fsm_registry_pass` reads that string and emits
///     `TransitionAction::Symbol(...)` on the `EventTransition`.
pub(crate) fn synthesize_fsm_context_and_actions(program: &mut TypedProgram) {
    use zyntax_typed_ast::TypedNode;
    use zyntax_typed_ast::typed_ast::{
        TypedBlock, TypedCall, TypedDeclaration, TypedExpression, TypedFunction, TypedLiteral,
    };
    use zyntax_typed_ast::{InternedString, Mutability};

    // Collected work — applied after the read loop so we don't hold
    // borrows across mutations.
    struct ActionLift {
        fn_name: InternedString,
        body: TypedBlock,
    }
    struct CtxFieldDecl {
        signal_name: String,
        ty: Type,
    }

    let mut signal_decls: Vec<CtxFieldDecl> = Vec::new();
    let mut action_lifts: Vec<ActionLift> = Vec::new();

    fn type_from_name(ty_name: &str) -> Option<Type> {
        Some(Type::Primitive(match ty_name {
            "i32" => PrimitiveType::I32,
            "f64" => PrimitiveType::F64,
            "bool" => PrimitiveType::Bool,
            "string" | "str" => PrimitiveType::String,
            _ => return None,
        }))
    }

    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        let Some(fsm_name) = imp.trait_name.resolve_global() else {
            continue;
        };
        let fsm_name_str: &str = &fsm_name;

        for method in &mut imp.methods {
            if method.name.resolve_global().as_deref() != Some("__fsm_meta__") {
                continue;
            }
            let Some(body) = method.body.as_mut() else {
                continue;
            };

            let mut action_idx: usize = 0;
            for stmt in &mut body.statements {
                let TypedStatement::Expression(expr_node) = &mut stmt.node else {
                    continue;
                };
                let TypedExpression::Call(call) = &mut expr_node.node else {
                    continue;
                };
                let TypedExpression::Variable(callee_id) = &call.callee.node else {
                    continue;
                };
                let Some(callee) = callee_id.resolve_global() else {
                    continue;
                };
                let callee_str: &str = &callee;

                match callee_str {
                    "__fsm_context_field__" => {
                        // arg[0] = name (StringLiteral)
                        // arg[1] = type (StringLiteral, e.g. "i32")
                        // arg[2] = default expression (literal)
                        let Some(name_arg) = call.positional_args.first() else {
                            continue;
                        };
                        let Some(ty_arg) = call.positional_args.get(1) else {
                            continue;
                        };
                        let TypedExpression::Literal(TypedLiteral::String(name_intern)) =
                            &name_arg.node
                        else {
                            continue;
                        };
                        let TypedExpression::Literal(TypedLiteral::String(ty_intern)) =
                            &ty_arg.node
                        else {
                            continue;
                        };
                        let Some(name_str) = name_intern.resolve_global() else {
                            continue;
                        };
                        let Some(ty_str) = ty_intern.resolve_global() else {
                            continue;
                        };
                        let Some(field_ty) = type_from_name(&ty_str) else {
                            // Unknown type — skip; populate_fsm_registry_pass
                            // will emit a diagnostic on the same marker shape.
                            continue;
                        };
                        let signal_name =
                            super::fsm_registry::mangle_ctx_signal(fsm_name_str, &name_str);
                        signal_decls.push(CtxFieldDecl {
                            signal_name,
                            ty: field_ty,
                        });
                    }
                    "__fsm_transition__" => {
                        // 4th positional arg (if present) is the action
                        // body `Block`. Lift it; rewrite the marker arg
                        // to a string literal carrying the lifted symbol.
                        if call.positional_args.len() < 4 {
                            continue;
                        }
                        let body_arg = call.positional_args[3].clone();
                        let TypedExpression::Block(action_block) = body_arg.node else {
                            continue;
                        };
                        // Apply ctx-rewrite to the lifted body before
                        // it leaves this FSM's scope.
                        let mut rewritten = action_block;
                        rewrite_fsm_ctx_access_block(fsm_name_str, &mut rewritten);

                        let fn_name_str = format!("__fsm_action_{fsm_name_str}_{action_idx}__");
                        let fn_name = InternedString::new_global(&fn_name_str);
                        action_lifts.push(ActionLift {
                            fn_name,
                            body: rewritten,
                        });

                        // Replace the Block arg with a StringLiteral
                        // carrying the lifted symbol name. populate
                        // reads it as args[3] and emits
                        // TransitionAction::Symbol(...).
                        call.positional_args[3] = TypedNode::new(
                            TypedExpression::Literal(TypedLiteral::String(
                                InternedString::new_global(&fn_name_str),
                            )),
                            Type::Primitive(PrimitiveType::String),
                            body_arg.span,
                        );
                        action_idx += 1;
                    }
                    "__fsm_tick__" => {
                        // arg[1] is the raw guard expression; apply
                        // ctx-rewrite so guards can read context fields
                        // the same way actions do.
                        if let Some(guard_expr) = call.positional_args.get_mut(1) {
                            rewrite_fsm_ctx_access_expr(fsm_name_str, guard_expr);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Emit top-level signal decls (extern fn-with-no-body shape) so
    // `resolve_signal_calls` recognises them. The signal-init pass
    // (publish-side) seeds non-zero defaults at FSM-registration time.
    for ctx_field in signal_decls {
        let sig_func = TypedFunction {
            name: InternedString::new_global(&ctx_field.signal_name),
            params: vec![],
            return_type: ctx_field.ty,
            body: None,
            is_external: true,
            link_name: None,
            ..Default::default()
        };
        program.declarations.push(TypedNode::new(
            TypedDeclaration::Function(sig_func),
            Type::Unknown,
            Span::default(),
        ));
    }

    // Emit lifted action fns.
    for ActionLift { fn_name, body } in action_lifts {
        let func = TypedFunction {
            name: fn_name,
            params: vec![],
            return_type: Type::Primitive(PrimitiveType::Unit),
            body: Some(body),
            ..Default::default()
        };
        program.declarations.push(TypedNode::new(
            TypedDeclaration::Function(func),
            Type::Unknown,
            Span::default(),
        ));
    }

    // Silence "unused" lints on imports only used through pattern matches above.
    let _ = std::any::type_name::<TypedCall>();
    let _ = std::any::type_name::<Mutability>();
}

/// Resolve `<FsmName>.<field>` field-access expressions appearing
/// OUTSIDE an FSM body (view, init, other components) by rewriting them
/// to the mangled signal identifier. Required so user-facing code can
/// read / write FSM context like a signal:
///
///   `Text(f"{CounterFsm.count.get()}")`     // read
///   `CounterFsm.count.set(0)`               // write via set()
///   `@stateful([CounterFsm.count])`         // binding list
///
/// After this pass, the surface forms reach `resolve_signal_calls` as
/// plain `Variable(<mangled>).get()` etc.
///
/// Discrimination: only rewrites when a matching synthetic signal decl
/// (`__fsm_ctx_<Fsm>_<field>`) exists. Non-FSM field-access shapes
/// (struct.field) pass through unchanged.
pub(crate) fn resolve_dotted_fsm_field_access(program: &mut TypedProgram) {
    use std::collections::HashSet;
    use zyntax_typed_ast::InternedString;
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedExpression, TypedLambdaBody};

    // Build the set of known mangled context-signal names from the
    // synthesized extern decls.
    let mut known_ctx_signals: HashSet<String> = HashSet::new();
    for decl in &program.declarations {
        if let TypedDeclaration::Function(f) = &decl.node {
            if !is_signal_decl(f) {
                continue;
            }
            let Some(name) = f.name.resolve_global() else {
                continue;
            };
            if name.starts_with("__fsm_ctx_") {
                known_ctx_signals.insert(name.to_string());
            }
        }
    }
    if known_ctx_signals.is_empty() {
        return;
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        known: &HashSet<String>,
    ) {
        // Children first.
        match &mut expr.node {
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, known);
                for a in &mut c.positional_args {
                    rewrite_expr(a, known);
                }
                // MUST also walk named args. Without this, `Div(opacity = Ticker.pct)`
                // ships its `Ticker.pct` Field access through to the
                // styling-args lowering as a raw Field — never matches
                // `signal_id_for_variable`, falls back to literal path,
                // overlay's `*_signal_id_raw` stays None, and the live
                // binding never wires up.
                for na in &mut c.named_args {
                    rewrite_expr(&mut na.value, known);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, known);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, known);
                }
                for na in &mut mc.named_args {
                    rewrite_expr(&mut na.value, known);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, known);
                rewrite_expr(&mut b.right, known);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand, known),
            TypedExpression::Field(f) => rewrite_expr(&mut f.object, known),
            TypedExpression::Index(i) => {
                rewrite_expr(&mut i.object, known);
                rewrite_expr(&mut i.index, known);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it, known);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    rewrite_stmt(stmt, known);
                }
            }
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, known);
                rewrite_expr(&mut if_expr.then_branch, known);
                rewrite_expr(&mut if_expr.else_branch, known);
            }
            TypedExpression::Lambda(lam) => match &mut lam.body {
                TypedLambdaBody::Expression(e) => rewrite_expr(e, known),
                TypedLambdaBody::Block(block) => {
                    for stmt in &mut block.statements {
                        rewrite_stmt(stmt, known);
                    }
                }
            },
            _ => {}
        }

        // Now check THIS node for `Field { object: Variable(<FsmName>),
        // field: <name> }` and rewrite to a Variable lookup of the
        // mangled signal name — but only when that mangled signal
        // actually exists.
        let TypedExpression::Field(f) = &expr.node else {
            return;
        };
        let TypedExpression::Variable(obj_name) = &f.object.node else {
            return;
        };
        let Some(obj_str) = obj_name.resolve_global() else {
            return;
        };
        let Some(field_str) = f.field.resolve_global() else {
            return;
        };
        let candidate = super::fsm_registry::mangle_ctx_signal(&obj_str, &field_str);
        if !known.contains(&candidate) {
            return;
        }
        let span = expr.span;
        let ty = expr.ty.clone();
        expr.node = TypedExpression::Variable(InternedString::new_global(&candidate));
        expr.ty = ty;
        expr.span = span;
    }

    fn rewrite_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        known: &HashSet<String>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, known),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, known);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, known),
            TypedStatement::Block(b) => {
                for inner in &mut b.statements {
                    rewrite_stmt(inner, known);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, known);
                for inner in &mut if_stmt.then_block.statements {
                    rewrite_stmt(inner, known);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for inner in &mut else_block.statements {
                        rewrite_stmt(inner, known);
                    }
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, known);
                for inner in &mut w.body.statements {
                    rewrite_stmt(inner, known);
                }
            }
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(f) => {
                if let Some(body) = &mut f.body {
                    for stmt in &mut body.statements {
                        rewrite_stmt(stmt, &known_ctx_signals);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for m in &mut imp.methods {
                    if let Some(body) = &mut m.body {
                        for stmt in &mut body.statements {
                            rewrite_stmt(stmt, &known_ctx_signals);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// =====================================================================
// Reactive-prop FFI expansion
// =====================================================================
//
// Companion pass to `resolve_extern_widget_named_args`. Walks every
// `$Blinc$<X>$view(...)` call site whose registered widget declares
// one or more `#[reactive] Reactive<T>` props (PropDef carries
// `reactive_inner: Some(T)`). For each such prop's positional arg
// slot, EXPANDS the single arg into two FFI slots — `tag: i32`,
// `payload: i64` — per the user-written value shape:
//
//   * Literal expression       → `(REACTIVE_TAG_LITERAL, encoded_bits)`
//   * Bare-Variable signal ref → `(REACTIVE_TAG_SIGNAL,  signal_id)`
//   * `computed { … } : T` call → `(REACTIVE_TAG_COMPUTED, derived_id)`
//
// Unrecognised arg shapes fall back to LITERAL with the original
// expression as the payload — preserves existing behaviour for
// arbitrary expressions, at the cost of an `f64→i64`-bitcast
// mismatch for runtime-computed floats. The doc on
// `blinc_runtime::reactive_value` describes the workaround:
// wrap arbitrary exprs in `computed { … } : T`.
//
// Runs AFTER `resolve_extern_widget_named_args` so we see fully-
// positionalised args; runs BEFORE Cranelift compile so the new
// arg list matches the macro-generated thunk's two-slot signature.
pub(crate) fn lower_reactive_args(program: &mut TypedProgram) {
    use zyntax_typed_ast::TypedNode;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};

    /// Type tag constants — kept in sync with
    /// `blinc_runtime::reactive_value::REACTIVE_TAG_*`. We don't
    /// `pub use` those here because this pass should stay
    /// dependency-light; they're three ints, easy to keep aligned.
    const TAG_LITERAL: i128 = 0;
    const TAG_SIGNAL: i128 = 1;
    const TAG_COMPUTED: i128 = 2;

    /// Match the rightmost callee against the registry to recover the
    /// PropDef list. Returns `None` for non-substrate calls so the
    /// pass leaves user-component / closure-target / etc. calls
    /// alone.
    fn registry_props(call: &TypedCall) -> Option<Vec<blinc_runtime::component::PropDef>> {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return None;
        };
        let sym = callee.resolve_global()?;
        let sym: &str = &sym;
        let name = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))?;
        blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name).map(|def| def.props.clone())
        })
    }

    /// Encode a literal value (per the prop's inner T) into the
    /// payload bit-pattern the runtime decoder reads back. f64 goes
    /// through `to_bits`; bool / i32 cast directly; non-matching
    /// shapes fall back to `0` and let runtime decode produce the
    /// inner type's default.
    fn encode_literal_payload(inner_ty: &Type, value: &TypedNode<TypedExpression>) -> i128 {
        match (inner_ty, &value.node) {
            (
                Type::Primitive(PrimitiveType::I32),
                TypedExpression::Literal(TypedLiteral::Integer(n)),
            ) => *n,
            (
                Type::Primitive(PrimitiveType::Bool),
                TypedExpression::Literal(TypedLiteral::Bool(b)),
            ) if *b => 1,
            (
                Type::Primitive(PrimitiveType::Bool),
                TypedExpression::Literal(TypedLiteral::Bool(_)),
            ) => 0,
            (
                Type::Primitive(PrimitiveType::Bool),
                TypedExpression::Literal(TypedLiteral::Integer(n)),
            ) if *n != 0 => 1,
            (
                Type::Primitive(PrimitiveType::Bool),
                TypedExpression::Literal(TypedLiteral::Integer(_)),
            ) => 0,
            (
                Type::Primitive(PrimitiveType::F64),
                TypedExpression::Literal(TypedLiteral::Float(f)),
            ) => f.to_bits() as i128,
            (
                Type::Primitive(PrimitiveType::F64),
                TypedExpression::Literal(TypedLiteral::Integer(n)),
            ) => (*n as f64).to_bits() as i128,
            // Non-literal / type-mismatched shapes encode as 0 here;
            // the existing `default_literal_for` path on the
            // resolve-named-args pass already left a `0` literal in
            // the slot for unsupplied props.
            _ => 0,
        }
    }

    /// Build a `Literal::Integer(n)` arg node carrying an `i64`-typed
    /// constant — used for both the tag slot and the encoded
    /// payload slot when emitting the expanded two-arg pair.
    fn i64_literal(value: i128, span: zyntax_typed_ast::Span) -> TypedNode<TypedExpression> {
        zyntax_typed_ast::TypedNode::new(
            TypedExpression::Literal(TypedLiteral::Integer(value)),
            Type::Primitive(PrimitiveType::I64),
            span,
        )
    }

    fn i32_literal(value: i128, span: zyntax_typed_ast::Span) -> TypedNode<TypedExpression> {
        zyntax_typed_ast::TypedNode::new(
            TypedExpression::Literal(TypedLiteral::Integer(value)),
            Type::Primitive(PrimitiveType::I32),
            span,
        )
    }

    /// Recognise `__blinc_computed_<T>__(closure_expr)` — the call
    /// shape `computed { … } : T` lowers to (per `grammar/blinc.zyn`).
    /// The call evaluates at runtime to a `DerivedId.to_raw() as i64`,
    /// which is exactly the payload we want under
    /// `REACTIVE_TAG_COMPUTED`.
    fn is_computed_call(expr: &TypedNode<TypedExpression>) -> bool {
        let TypedExpression::Call(c) = &expr.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &c.callee.node else {
            return false;
        };
        matches!(
            callee.resolve_global().as_deref(),
            Some("__blinc_computed_i32__")
                | Some("__blinc_computed_f64__")
                | Some("__blinc_computed_string__")
        )
    }

    /// Recognise a bare-Variable reference to a declared signal.
    /// Same lookup `lower_styling_args_to_overlays` uses for built-in
    /// widgets — bare identifier → `blinc_runtime::signal::lookup`
    /// hits the process-global signal registry, returns
    /// `Some((id_raw, ty))` when the name is registered.
    fn signal_id_for_variable(value: &TypedNode<TypedExpression>) -> Option<u64> {
        let TypedExpression::Variable(name) = &value.node else {
            return None;
        };
        let name_str = name.resolve_global()?;
        let (id_raw, _ty) = blinc_runtime::signal::lookup(&name_str)?;
        Some(id_raw)
    }

    /// Expand one reactive-prop arg into the wire-format slots the
    /// macro thunk expects. Scalar `Reactive<T>` returns two slots
    /// `(tag, payload: i64)`; `Reactive<String>` returns three
    /// `(tag, id_payload: i64, literal_ptr: *const i32)`. The caller
    /// splices the result in place of the original single arg.
    fn expand_reactive(
        inner_ty: &Type,
        arg: TypedNode<TypedExpression>,
    ) -> Vec<TypedNode<TypedExpression>> {
        let span = arg.span;
        let is_string = matches!(inner_ty, Type::Primitive(PrimitiveType::String));

        // Shape A: bare-Variable signal ref → SIGNAL tag.
        if let Some(id_raw) = signal_id_for_variable(&arg) {
            let tag = i32_literal(TAG_SIGNAL, span);
            let id = i64_literal(id_raw as i128, span);
            if is_string {
                return vec![tag, id, null_string_ptr_literal(span)];
            }
            return vec![tag, id];
        }
        // Shape B: `computed { … } : T` call → COMPUTED tag.
        // For string the call's return value is the raw derived id
        // (an i64); the literal slot stays null.
        if is_computed_call(&arg) {
            let tag = i32_literal(TAG_COMPUTED, span);
            if is_string {
                return vec![tag, arg, null_string_ptr_literal(span)];
            }
            return vec![tag, arg];
        }
        // Shape C: literal expression → LITERAL tag.
        let tag = i32_literal(TAG_LITERAL, span);
        if is_string {
            // The literal is a String expression; it flows verbatim
            // into the `literal_ptr` slot. The `id_payload` slot is
            // unused for the literal path — write 0.
            return vec![tag, i64_literal(0, span), arg];
        }
        let payload = encode_literal_payload(inner_ty, &arg);
        vec![tag, i64_literal(payload, span)]
    }

    /// A null `*const i32` literal for the unused `literal_ptr` slot
    /// in non-literal `Reactive<String>` wire-format triples. Zyntax
    /// doesn't have a dedicated null-pointer literal, so we approximate
    /// with an empty `""` string literal — `decode_string` on the
    /// resulting pointer would yield an empty string, but the macro
    /// thunk's match never reaches the literal branch when the tag
    /// is SIGNAL or COMPUTED, so the value is never observed.
    fn null_string_ptr_literal(span: zyntax_typed_ast::Span) -> TypedNode<TypedExpression> {
        TypedNode::new(
            TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::String(
                zyntax_typed_ast::InternedString::new_global(""),
            )),
            Type::Primitive(PrimitiveType::String),
            span,
        )
    }

    /// Per-call expansion. Iterates `props` and `positional_args` in
    /// lockstep; each reactive prop's single arg slot becomes two,
    /// each non-reactive prop passes through.
    fn rewrite_call(call: &mut TypedCall) {
        let Some(props) = registry_props(call) else {
            return;
        };
        // Cheap pre-check: if the widget has no reactive props, the
        // walk would be a no-op. Skip.
        if !props.iter().any(|p| p.reactive_inner.is_some()) {
            return;
        }
        let old_args = std::mem::take(&mut call.positional_args);
        let mut new_args: Vec<TypedNode<TypedExpression>> =
            Vec::with_capacity(old_args.len() + props.len());

        let mut arg_iter = old_args.into_iter();
        for prop in &props {
            let Some(arg) = arg_iter.next() else {
                break;
            };
            if let Some(inner_ty) = &prop.reactive_inner {
                new_args.extend(expand_reactive(inner_ty, arg));
            } else {
                new_args.push(arg);
            }
        }
        // Any args beyond the prop count pass through unchanged —
        // matches existing behaviour for varargs / overflow.
        new_args.extend(arg_iter);

        call.positional_args = new_args;
    }

    fn rewrite_expr(expr: &mut TypedNode<TypedExpression>) {
        match &mut expr.node {
            TypedExpression::Call(call) => {
                rewrite_expr(&mut call.callee);
                for a in &mut call.positional_args {
                    rewrite_expr(a);
                }
                for na in &mut call.named_args {
                    rewrite_expr(&mut na.value);
                }
                rewrite_call(call);
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver);
                for a in &mut mc.positional_args {
                    rewrite_expr(a);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left);
                rewrite_expr(&mut b.right);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand),
            TypedExpression::Field(f) => rewrite_expr(&mut f.object),
            TypedExpression::Index(i) => {
                rewrite_expr(&mut i.object);
                rewrite_expr(&mut i.index);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    rewrite_stmt(stmt);
                }
            }
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition);
                rewrite_expr(&mut if_expr.then_branch);
                rewrite_expr(&mut if_expr.else_branch);
            }
            _ => {}
        }
    }

    fn rewrite_stmt(stmt: &mut TypedNode<TypedStatement>) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition);
                for inner in &mut if_stmt.then_block.statements {
                    rewrite_stmt(inner);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for inner in &mut else_block.statements {
                        rewrite_stmt(inner);
                    }
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition);
                for inner in &mut w.body.statements {
                    rewrite_stmt(inner);
                }
            }
            TypedStatement::Block(b) => {
                for inner in &mut b.statements {
                    rewrite_stmt(inner);
                }
            }
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    for stmt in &mut body.statements {
                        rewrite_stmt(stmt);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        for stmt in &mut body.statements {
                            rewrite_stmt(stmt);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
