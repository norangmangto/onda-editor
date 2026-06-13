; Tree-sitter text-object queries for TypeScript (onda T18.2).

(function_declaration
  body: (_) @function.inner) @function.outer

(generator_function_declaration
  body: (_) @function.inner) @function.outer

(function_expression
  body: (_) @function.inner) @function.outer

(arrow_function
  body: (_) @function.inner) @function.outer

(method_definition
  body: (_) @function.inner) @function.outer

(class_declaration
  body: (_) @class.inner) @class.outer

(class
  body: (_) @class.inner) @class.outer

(interface_declaration
  body: (_) @class.inner) @class.outer

(formal_parameters
  (_) @parameter.inner) @parameter.outer

(arguments
  (_) @parameter.inner) @parameter.outer
