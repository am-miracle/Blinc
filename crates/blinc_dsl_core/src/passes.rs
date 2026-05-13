// =====================================================================
// FSM dispatch synthesis (post-parse)
// =====================================================================

use super::*;

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
                if is_fsm {
                    if let Some(name) = imp.trait_name.resolve_global() {
                        fsms.push(name.to_string());
                    }
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
            if let TypedExpression::Literal(TypedLiteral::String(s)) = &expr.node {
                if let Some(text) = s.resolve_global() {
                    out.push(auto_inject_semicolons(&text));
                }
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
pub(crate) fn resolve_signal_calls(program: &mut TypedProgram) {
    use std::collections::HashMap;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};
    use zyntax_typed_ast::InternedString;

    // Phase 1: collect signal name → return type.
    let mut signals: HashMap<InternedString, Type> = HashMap::new();
    for decl in &program.declarations {
        let TypedDeclaration::Function(func) = &decl.node else {
            continue;
        };
        if !is_signal_decl(func) {
            continue;
        }
        signals.insert(func.name, func.return_type.clone());
    }

    if signals.is_empty() {
        return;
    }

    // Phase 2: rewrite `<sig>.get()` → `__signal_get_<T>("<name>")`.
    fn typed_signal_extern_name(ty: &Type) -> Option<&'static str> {
        match ty {
            Type::Primitive(PrimitiveType::I32) => Some("__signal_get_i32"),
            Type::Primitive(PrimitiveType::F64) => Some("__signal_get_f64"),
            Type::Primitive(PrimitiveType::String) => Some("__signal_get_string"),
            _ => None,
        }
    }

    fn typed_signal_setter_extern_name(ty: &Type) -> Option<&'static str> {
        match ty {
            Type::Primitive(PrimitiveType::I32) => Some("__signal_set_i32"),
            Type::Primitive(PrimitiveType::F64) => Some("__signal_set_f64"),
            Type::Primitive(PrimitiveType::String) => Some("__signal_set_string"),
            _ => None,
        }
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        signals: &HashMap<InternedString, Type>,
    ) {
        // MUST intercept `<signal> = <expr>` BEFORE the recursive walk — the
        // LHS `Variable` doesn't otherwise trigger a rewrite.
        if let TypedExpression::Binary(b) = &expr.node {
            if b.op == zyntax_typed_ast::typed_ast::BinaryOp::Assign {
                if let TypedExpression::Variable(name) = &b.left.node {
                    if let Some(sig_ty) = signals.get(name).cloned() {
                        if let Some(setter) = typed_signal_setter_extern_name(&sig_ty) {
                            // Rewrite RHS first so nested signal reads route through getters.
                            let mut rhs = (*b.right).clone();
                            rewrite_expr(&mut rhs, signals);

                            let name_arg = zyntax_typed_ast::TypedNode::new(
                                TypedExpression::Literal(TypedLiteral::String(*name)),
                                Type::Primitive(PrimitiveType::String),
                                expr.span,
                            );
                            let callee = zyntax_typed_ast::TypedNode::new(
                                TypedExpression::Variable(InternedString::new_global(setter)),
                                Type::Unknown,
                                expr.span,
                            );
                            expr.node = TypedExpression::Call(TypedCall {
                                callee: Box::new(callee),
                                positional_args: vec![name_arg, rhs],
                                named_args: vec![],
                                type_args: vec![],
                            });
                            expr.ty = Type::Primitive(PrimitiveType::Unit);
                            return;
                        }
                    }
                }
            }
        }

        // Children first so nested signal calls (e.g. `text(count.get())`) are rewritten.
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, signals);
                rewrite_expr(&mut b.right, signals);
            }
            TypedExpression::Unary(u) => {
                rewrite_expr(&mut u.operand, signals);
            }
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, signals);
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
                rewrite_expr(&mut mc.receiver, signals);
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
            return;
        };
        let Some(sig_ty) = signals.get(&receiver_name).cloned() else {
            return;
        };
        let method_name = method.resolve_global().map(|s| s.to_string());
        match method_name.as_deref() {
            // `count.get()` — read. Zero args, returns the
            // signal's value type.
            Some("get") if args.is_empty() => {
                let Some(extern_name) = typed_signal_extern_name(&sig_ty) else {
                    return;
                };
                expr.node = TypedExpression::Call(TypedCall {
                    callee: Box::new(zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Variable(InternedString::new_global(extern_name)),
                        Type::Unknown,
                        span,
                    )),
                    positional_args: vec![zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Literal(TypedLiteral::String(receiver_name)),
                        Type::Primitive(PrimitiveType::String),
                        span,
                    )],
                    named_args: vec![],
                    type_args: vec![],
                });
                expr.ty = sig_ty;
            }
            // `count.set(value)` — write. Arg already child-rewritten.
            Some("set") if args.len() == 1 => {
                let Some(setter) = typed_signal_setter_extern_name(&sig_ty) else {
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
                            TypedExpression::Literal(TypedLiteral::String(receiver_name)),
                            Type::Primitive(PrimitiveType::String),
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
        signals: &HashMap<InternedString, Type>,
    ) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, signals);
        }
    }

    fn rewrite_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        signals: &HashMap<InternedString, Type>,
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

    // Phase 3: strip signal-marker decls (metadata only; usage was rewritten above).
    program.declarations.retain(|decl| {
        let TypedDeclaration::Function(func) = &decl.node else {
            return true;
        };
        !is_signal_decl(func)
    });
}

/// Rewrite `<FsmName>.trigger(<path>)` → `__fsm_runtime_trigger__("<FsmName>", <path>)`.
pub(crate) fn resolve_fsm_trigger_calls(program: &mut TypedProgram) {
    use std::collections::HashSet;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};
    use zyntax_typed_ast::InternedString;

    // Phase 1: collect declared FSM names from `__fsm_meta__`-bearing impls.
    let mut fsm_names: HashSet<InternedString> = HashSet::new();
    for decl in &program.declarations {
        if let TypedDeclaration::Impl(imp) = &decl.node {
            if imp.trait_name.resolve_global().is_some()
                && imp
                    .methods
                    .iter()
                    .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
            {
                fsm_names.insert(imp.trait_name);
            }
        }
    }
    if fsm_names.is_empty() {
        return;
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        fsm_names: &HashSet<InternedString>,
    ) {
        // Recurse children first.
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, fsm_names);
                rewrite_expr(&mut b.right, fsm_names);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand, fsm_names),
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, fsm_names);
                for a in &mut c.positional_args {
                    rewrite_expr(a, fsm_names);
                }
            }
            TypedExpression::Field(f) => rewrite_expr(&mut f.object, fsm_names),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, fsm_names);
                rewrite_expr(&mut idx.index, fsm_names);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it, fsm_names);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, fsm_names);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, fsm_names);
                }
            }
            TypedExpression::Block(b) => rewrite_block(b, fsm_names),
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, fsm_names);
                rewrite_expr(&mut if_expr.then_branch, fsm_names);
                rewrite_expr(&mut if_expr.else_branch, fsm_names);
            }
            TypedExpression::Lambda(lam) => match &mut lam.body {
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                    rewrite_expr(e, fsm_names);
                }
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                    rewrite_block(block, fsm_names);
                }
            },
            _ => {}
        }

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
        if !fsm_names.contains(&receiver_name) {
            return;
        }
        if method.resolve_global().as_deref() != Some("trigger") {
            return;
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

    fn rewrite_block(
        block: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        fsm_names: &HashSet<InternedString>,
    ) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, fsm_names);
        }
    }

    fn rewrite_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        fsm_names: &HashSet<InternedString>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, fsm_names),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, fsm_names);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, fsm_names),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, fsm_names);
                rewrite_block(&mut if_stmt.then_block, fsm_names);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, fsm_names);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, fsm_names);
                rewrite_block(&mut w.body, fsm_names);
            }
            TypedStatement::Block(b) => rewrite_block(b, fsm_names),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_block(body, &fsm_names);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_block(body, &fsm_names);
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
pub(crate) fn resolve_fsm_subscribe_calls(program: &mut TypedProgram) {
    use std::collections::HashSet;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};
    use zyntax_typed_ast::InternedString;

    let mut fsm_names: HashSet<InternedString> = HashSet::new();
    for decl in &program.declarations {
        if let TypedDeclaration::Impl(imp) = &decl.node {
            if imp.trait_name.resolve_global().is_some()
                && imp
                    .methods
                    .iter()
                    .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
            {
                fsm_names.insert(imp.trait_name);
            }
        }
    }
    if fsm_names.is_empty() {
        return;
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        fsm_names: &HashSet<InternedString>,
    ) {
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, fsm_names);
                rewrite_expr(&mut b.right, fsm_names);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand, fsm_names),
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, fsm_names);
                for a in &mut c.positional_args {
                    rewrite_expr(a, fsm_names);
                }
            }
            TypedExpression::Field(f) => rewrite_expr(&mut f.object, fsm_names),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, fsm_names);
                rewrite_expr(&mut idx.index, fsm_names);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it, fsm_names);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, fsm_names);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, fsm_names);
                }
            }
            TypedExpression::Block(b) => rewrite_block(b, fsm_names),
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, fsm_names);
                rewrite_expr(&mut if_expr.then_branch, fsm_names);
                rewrite_expr(&mut if_expr.else_branch, fsm_names);
            }
            TypedExpression::Lambda(lam) => match &mut lam.body {
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                    rewrite_expr(e, fsm_names);
                }
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                    rewrite_block(block, fsm_names);
                }
            },
            _ => {}
        }

        // Match in both AST shapes (MethodCall / Call+Field).
        let subscribe_call = match &expr.node {
            TypedExpression::MethodCall(mc) if mc.positional_args.len() == 2 => {
                if let TypedExpression::Variable(receiver_name) = &mc.receiver.node {
                    Some((
                        *receiver_name,
                        mc.method,
                        mc.positional_args[0].clone(),
                        mc.positional_args[1].clone(),
                        expr.span,
                    ))
                } else {
                    None
                }
            }
            TypedExpression::Call(c) if c.positional_args.len() == 2 => {
                if let TypedExpression::Field(f) = &c.callee.node {
                    if let TypedExpression::Variable(receiver_name) = &f.object.node {
                        Some((
                            *receiver_name,
                            f.field,
                            c.positional_args[0].clone(),
                            c.positional_args[1].clone(),
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

        let Some((receiver_name, method, path_arg, closure_arg, span)) = subscribe_call else {
            return;
        };
        if !fsm_names.contains(&receiver_name) {
            return;
        }
        if method.resolve_global().as_deref() != Some("subscribe") {
            return;
        }

        let fsm_name_arg = zyntax_typed_ast::TypedNode::new(
            TypedExpression::Literal(TypedLiteral::String(receiver_name)),
            Type::Primitive(PrimitiveType::String),
            span,
        );
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

    fn rewrite_block(
        block: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        fsm_names: &HashSet<InternedString>,
    ) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, fsm_names);
        }
    }

    fn rewrite_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        fsm_names: &HashSet<InternedString>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, fsm_names),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, fsm_names);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, fsm_names),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, fsm_names);
                rewrite_block(&mut if_stmt.then_block, fsm_names);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, fsm_names);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, fsm_names);
                rewrite_block(&mut w.body, fsm_names);
            }
            TypedStatement::Block(b) => rewrite_block(b, fsm_names),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_block(body, &fsm_names);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_block(body, &fsm_names);
                    }
                }
            }
            _ => {}
        }
    }
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
        if let TypedDeclaration::Class(c) = &decl.node {
            if let Some(name) = c.name.resolve_global() {
                known.insert(name.to_string());
            }
        }
        // Named imports — whitelist so the validator (pre import-resolution) doesn't flag them.
        if let TypedDeclaration::Import(import) = &decl.node {
            for item in &import.items {
                if let zyntax_typed_ast::TypedImportItem::Named { name, .. } = item {
                    if let Some(s) = name.resolve_global() {
                        known.insert(s.to_string());
                    }
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
                if let TypedExpression::Variable(callee_name) = &c.callee.node {
                    if callee_name.resolve_global().as_deref() == Some("__component_call__") {
                        if let Some(name_node) = c.positional_args.first() {
                            if let TypedExpression::Literal(TypedLiteral::String(name)) =
                                &name_node.node
                            {
                                let name_str = name.resolve_global().unwrap_or_default();
                                if !known.contains::<str>(name_str.as_ref()) {
                                    errors.push(format!(
                                        "unknown component `{}` — declare it with \
                                         `component {} {{ ... }}` before use",
                                        name_str, name_str
                                    ));
                                }
                            }
                        }
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

/// Rewrite `__component_call__("Name", positionals, __named__(...), body)` markers
/// into `Call(Variable("Name"), positionals, named_args, body)`. MUST run after
/// `validate_component_calls`. Slot markers inside body Blocks are left alone.
pub(crate) fn lower_component_calls(program: &mut TypedProgram) {
    use zyntax_typed_ast::typed_ast::{
        TypedCall, TypedDeclaration, TypedExpression, TypedLiteral, TypedNamedArg,
    };

    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>) {
        // Recurse bottom-up so nested marker calls also lower.
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left);
                rewrite_expr(&mut b.right);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand),
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee);
                for a in &mut c.positional_args {
                    rewrite_expr(a);
                }
                for n in &mut c.named_args {
                    rewrite_expr(&mut n.value);
                }
            }
            TypedExpression::Field(f) => rewrite_expr(&mut f.object),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object);
                rewrite_expr(&mut idx.index);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it);
                }
            }
            TypedExpression::Struct(s) => {
                for field in &mut s.fields {
                    rewrite_expr(&mut field.value);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver);
                for a in &mut mc.positional_args {
                    rewrite_expr(a);
                }
            }
            TypedExpression::Block(b) => rewrite_block(b),
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition);
                rewrite_expr(&mut if_expr.then_branch);
                rewrite_expr(&mut if_expr.else_branch);
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
            if let TypedExpression::Call(inner) = &arg.node {
                if let TypedExpression::Variable(inner_callee) = &inner.callee.node {
                    if inner_callee.resolve_global().as_deref() == Some("__named__") {
                        let name_node = &inner.positional_args[0];
                        let value_node = &inner.positional_args[1];
                        let TypedExpression::Literal(TypedLiteral::String(arg_name)) =
                            &name_node.node
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
                }
            }
            new_positional.push(arg.clone());
        }

        // Carry pre-existing named_args through (defensive — grammar doesn't emit them).
        new_named.extend(call.named_args.iter().cloned());

        // Resolve callee to the registry's `view_symbol` (substrate primitives use
        // `$Blinc$<Name>$view`; user components use `<Name>$view`).
        let component_name_str = component_name.resolve_global().unwrap_or_default();
        let component_name_str: &str = component_name_str.as_ref();
        let view_symbol = blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(component_name_str)
                .map(|def| def.view_symbol.as_ref().to_string())
        })
        .unwrap_or_else(|| format!("{component_name_str}$view"));
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
    }

    fn rewrite_block(block: &mut zyntax_typed_ast::typed_ast::TypedBlock) {
        let old_stmts = std::mem::take(&mut block.statements);
        let mut new_stmts: Vec<zyntax_typed_ast::TypedNode<TypedStatement>> =
            Vec::with_capacity(old_stmts.len());
        for mut stmt in old_stmts {
            rewrite_stmt(&mut stmt);
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
        if let TypedStatement::Expression(expr_node) = &mut stmt.node {
            if let TypedExpression::Call(call) = &mut expr_node.node {
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
                        let mut default_children: Vec<
                            zyntax_typed_ast::TypedNode<TypedExpression>,
                        > = Vec::new();
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

    fn rewrite_stmt(stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>) {
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
                rewrite_block(&mut if_stmt.then_block);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition);
                rewrite_block(&mut w.body);
            }
            TypedStatement::Block(b) => rewrite_block(b),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_block(body);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_block(body);
                    }
                }
            }
            _ => {}
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
    use zyntax_typed_ast::type_registry::{
        TypeDefinition, TypeId, TypeKind, VariantDef, VariantFields, Visibility,
    };
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedExpression, TypedLiteral, TypedVariantFields,
    };
    use zyntax_typed_ast::InternedString;

    // Phase 1: scan. Collect (fsm_name, FsmDefinition) tuples.
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
                "__fsm_transition__" => {
                    if let (Some(from), Some(event), Some(to)) =
                        (str_arg(0), str_arg(1), str_arg(2))
                    {
                        def.transitions.push(EventTransition {
                            from,
                            event,
                            to,
                            actions: vec![],
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

    // Phase 2: pin TypeIds + populate the registry. Pre-register so Zyntax's
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

    // Phase 2.5: lift each captured tick-guard expression into a top-level fn
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

    // Phase 3: strip `__fsm_meta__` so compile doesn't try to resolve markers.
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
    use zyntax_typed_ast::typed_ast::{
        BinaryOp, TypedBinary, TypedBlock, TypedDeclaration, TypedExpression, TypedIf, TypedLiteral,
    };
    use zyntax_typed_ast::TypedNode;

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
            let mut arms: Vec<(Option<String>, TypedBlock)> = Vec::new();
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
                let pat_str = call_first_arg(&arm_block.statements[0]).and_then(|expr| {
                    if let TypedExpression::Literal(TypedLiteral::String(s)) = &expr.node {
                        s.resolve_global().map(|s| s.to_string())
                    } else {
                        None
                    }
                });
                let body = TypedBlock {
                    statements: arm_block.statements[1..].to_vec(),
                    span: arm_block.span,
                };
                arms.push((pat_str, body));
            }

            // Build the if/else-if/else chain. First `_` arm becomes trailing `else`.
            let mut else_block: Option<TypedBlock> = None;
            let mut chain_arms: Vec<(String, TypedBlock)> = Vec::new();
            for (pat, body) in arms {
                match pat.as_deref() {
                    Some("__wildcard__") if else_block.is_none() => {
                        else_block = Some(body);
                    }
                    Some("__wildcard__") => {}
                    Some(p) => {
                        chain_arms.push((p.to_string(), body));
                    }
                    None => {}
                }
            }

            // Fold from last to first so the FIRST arm wraps everything else.
            let mut tail_else = else_block;
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
                let if_stmt = TypedStatement::If(TypedIf {
                    condition: Box::new(condition),
                    then_block: body,
                    else_block: tail_else.take(),
                    span,
                });
                tail_else = Some(TypedBlock {
                    statements: vec![TypedNode::new(
                        if_stmt,
                        Type::Primitive(PrimitiveType::Unit),
                        span,
                    )],
                    span,
                });
            }

            // Splice the chain in place of the marker span.
            let chain_stmts = tail_else.map(|b| b.statements).unwrap_or_default();
            stmts.splice(i..=end_idx, chain_stmts);
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
        if slots.is_empty() {
            None
        } else {
            Some(slots)
        }
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
        // overlay setters) vs other args (left in place).
        let mut styling_args: Vec<(&'static str, TypedNamedArg)> = Vec::new();
        let mut remaining_named: Vec<TypedNamedArg> = Vec::new();
        let existing_named = std::mem::take(&mut call.named_args);
        for na in existing_named {
            let resolved = na.name.resolve_global();
            let name_str: Option<&str> = resolved.as_deref();
            if let Some(name) = name_str {
                if let Some(entry) = STYLING_PROP_NAMES.iter().find(|(n, _, _)| *n == name) {
                    styling_args.push((entry.1, na));
                    continue;
                }
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

        // One setter call per styling arg.
        for (setter_name, na) in styling_args {
            let setter_call = TypedExpression::Call(TypedCall {
                callee: Box::new(typed_node(
                    TypedExpression::Variable(zyntax_typed_ast::InternedString::new_global(
                        setter_name,
                    )),
                    Type::Any,
                    span,
                )),
                positional_args: vec![
                    typed_node(
                        TypedExpression::Variable(overlay_ident),
                        i64_ty.clone(),
                        span,
                    ),
                    *na.value,
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
                slots[i] = Some(arg);
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
                    slots[pos] = Some(*na.value);
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
