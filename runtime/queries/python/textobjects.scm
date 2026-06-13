; Tree-sitter text-object queries for Python (onda T18.2).

(function_definition
  body: (_) @function.inner) @function.outer

(lambda
  body: (_) @function.inner) @function.outer

(class_definition
  body: (_) @class.inner) @class.outer

(parameters
  (_) @parameter.inner) @parameter.outer

(argument_list
  (_) @parameter.inner) @parameter.outer
