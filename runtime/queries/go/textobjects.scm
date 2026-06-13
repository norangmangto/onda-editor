; Tree-sitter text-object queries for Go (onda T18.2).

(function_declaration
  body: (_) @function.inner) @function.outer

(method_declaration
  body: (_) @function.inner) @function.outer

(func_literal
  body: (_) @function.inner) @function.outer

; "Class"-like: struct and interface type declarations.
(type_declaration
  (type_spec
    type: (struct_type
      (field_declaration_list) @class.inner))) @class.outer

(type_declaration
  (type_spec
    type: (interface_type) @class.inner)) @class.outer

(parameter_list
  (_) @parameter.inner) @parameter.outer

(argument_list
  (_) @parameter.inner) @parameter.outer
