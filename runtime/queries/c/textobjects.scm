; Tree-sitter text-object queries for C (onda T18.2).

(function_definition
  body: (_) @function.inner) @function.outer

; "Class"-like: struct / union / enum specifiers with a body.
(struct_specifier
  body: (field_declaration_list) @class.inner) @class.outer

(union_specifier
  body: (field_declaration_list) @class.inner) @class.outer

(enum_specifier
  body: (enumerator_list) @class.inner) @class.outer

(parameter_list
  (_) @parameter.inner) @parameter.outer

(argument_list
  (_) @parameter.inner) @parameter.outer
