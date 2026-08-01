# Minimal JSON Schema (draft 2020-12) evaluator, covering exactly the keyword
# subset the QEDGen auditor benchmark contracts use.
#
# Usage:
#   jq -r --slurpfile schema <schema.json> -f jsonschema.jq <document.json>
#
# Prints one line per violation. Empty output means the document conforms.
#
# This file exists so `validate.sh` does not restate the schemas in jq. The
# `.schema.json` files are the single source of truth for structure; the shell
# script adds only the cross-record rules JSON Schema cannot express. A schema
# that grows a keyword this evaluator does not implement fails loudly rather
# than silently going unenforced, so the pair cannot drift apart unnoticed.

def root: $schema[0];

def supported:
  [
    "$schema", "$id", "$ref", "$defs", "title", "description",
    "type", "const", "enum", "pattern", "minLength",
    "minimum", "maximum", "minItems", "uniqueItems", "items",
    "minProperties", "required", "properties", "additionalProperties",
    "allOf"
  ];

# Every object that sits in a schema position. Property names and $defs member
# names are not schema positions, so walking `..` would misread them as
# keywords.
def schema_nodes:
  if type == "object" then
    .,
    (if has("properties") then (.properties[] | schema_nodes) else empty end),
    (if has("items") then (.items | schema_nodes) else empty end),
    (if has("$defs") then (.["$defs"][] | schema_nodes) else empty end),
    (if has("allOf") then (.allOf[] | schema_nodes) else empty end)
  else empty end;

def type_ok($t):
  if $t == "integer" then (type == "number" and floor == .)
  elif $t == "number" then type == "number"
  else type == $t
  end;

def deref($s):
  if ($s | type) == "object" and ($s | has("$ref"))
  then root["$defs"][$s["$ref"] | ltrimstr("#/$defs/")]
  else $s
  end;

def errors($s; $path):
  . as $doc
  | deref($s) as $s
  | (if ($s | has("allOf"))
     then [$s.allOf[] as $sub | $doc | errors($sub; $path) | .[]]
     else [] end)
  + (if ($s | has("type")) and (($doc | type_ok($s.type)) | not)
     then ["\($path): expected \($s.type), got \($doc | type)"]
     else [] end)
  + (if ($s | has("const")) and ($doc != $s.const)
     then ["\($path): must equal \($s.const | tojson)"]
     else [] end)
  + (if ($s | has("enum")) and (($doc | IN($s.enum[])) | not)
     then ["\($path): \($doc | tojson) is not one of \($s.enum | tojson)"]
     else [] end)
  + (if ($doc | type) == "string"
     then
       (if ($s | has("pattern")) and (($doc | test($s.pattern)) | not)
        then ["\($path): does not match \($s.pattern)"] else [] end)
       + (if ($s | has("minLength")) and (($doc | length) < $s.minLength)
          then ["\($path): shorter than minLength \($s.minLength)"] else [] end)
     else [] end)
  + (if ($doc | type) == "number"
     then
       (if ($s | has("minimum")) and ($doc < $s.minimum)
        then ["\($path): below minimum \($s.minimum)"] else [] end)
       + (if ($s | has("maximum")) and ($doc > $s.maximum)
          then ["\($path): above maximum \($s.maximum)"] else [] end)
     else [] end)
  + (if ($doc | type) == "array"
     then
       (if ($s | has("minItems")) and (($doc | length) < $s.minItems)
        then ["\($path): fewer than minItems \($s.minItems)"] else [] end)
       + (if ($s | has("uniqueItems")) and $s.uniqueItems
             and (($doc | length) != ($doc | unique | length))
          then ["\($path): items are not unique"] else [] end)
       + (if ($s | has("items"))
          then [range($doc | length) as $i
                | $doc[$i] | errors($s.items; "\($path)[\($i)]") | .[]]
          else [] end)
     else [] end)
  + (if ($doc | type) == "object"
     then
       (if ($s | has("minProperties"))
             and (($doc | keys | length) < $s.minProperties)
        then ["\($path): fewer than minProperties \($s.minProperties)"]
        else [] end)
       + (if ($s | has("required"))
          then [$s.required[] as $k
                | select(($doc | has($k)) | not)
                | "\($path): missing required \($k)"]
          else [] end)
       + (if ($s.additionalProperties) == false
          then [(($doc | keys) - (($s.properties // {}) | keys))[]
                | "\($path): unexpected property \(.)"]
          else [] end)
       + (if ($s | has("properties"))
          then [($s.properties | keys[]) as $k
                | select($doc | has($k))
                | $doc[$k] | errors($s.properties[$k]; "\($path).\($k)") | .[]]
          else [] end)
     else [] end);

([root | schema_nodes | keys_unsorted[]] | unique
  | map(select(IN(supported[]) | not))) as $unsupported
| [root | schema_nodes
   | select(has("$ref") and ((keys_unsorted | length) > 1))] as $ref_siblings
| ([root | schema_nodes | select(has("$ref")) | .["$ref"]]
   | unique
   | map(select(ltrimstr("#/$defs/") as $name
                | (((root | .["$defs"]) // {}) | has($name) | not)))) as $dangling
| if ($unsupported | length) > 0
  then "schema uses keywords this validator does not implement: \($unsupported | join(", "))"
  elif ($ref_siblings | length) > 0
  then "schema uses $ref alongside sibling keywords, which this validator ignores"
  elif ($dangling | length) > 0
  then "schema has unresolvable references: \($dangling | join(", "))"
  else errors(root; "$")[]
  end
