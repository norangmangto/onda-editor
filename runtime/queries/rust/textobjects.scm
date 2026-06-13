; Tree-sitter text-object queries for Rust (onda T18.2).
; Captures consumed by onda-syntax::textobjects: @function.outer/.inner,
; @class.outer/.inner, @parameter.outer/.inner.

; Functions (free functions, methods, closures).
(function_item
  body: (_) @function.inner) @function.outer

(function_signature_item) @function.outer

(closure_expression
  body: (_) @function.inner) @function.outer

; "Class"-like nominal items: structs, enums, traits, impls, unions, modules.
(struct_item
  body: (_) @class.inner) @class.outer

(enum_item
  body: (_) @class.inner) @class.outer

(union_item
  body: (_) @class.inner) @class.outer

(trait_item
  body: (_) @class.inner) @class.outer

(impl_item
  body: (_) @class.inner) @class.outer

; Parameters / arguments (both declaration and call sites).
(parameters
  (_) @parameter.inner) @parameter.outer

(arguments
  (_) @parameter.inner) @parameter.outer
