//! The published SDK manifest and TypeScript bindings, generated from the
//! Rust types that validate every reply. Both languages therefore accept
//! exactly the same JSON. `examples/generate.rs` writes the files and
//! `tests/generated.rs` fails when the committed copies drift.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use schemars::{JsonSchema, SchemaGenerator};
use serde_json::{Map, Value, json};

use crate::ops::{self, Operation, TaskIntent};

/// The manifest: operations, intents, frames and their JSON Schemas.
pub const MANIFEST_PATH: &str = "crates/sdk/contract/v1/sdk.json";
/// The TypeScript SDK's generated types and embedded manifest.
pub const TYPESCRIPT_PATH: &str = "sdk/typescript/src/generated.ts";

/// Every generated file, as `(path from the repository root, contents)`.
pub fn files() -> Vec<(&'static str, String)> {
    let manifest = manifest();
    vec![
        (MANIFEST_PATH, pretty(&manifest)),
        (TYPESCRIPT_PATH, typescript(&manifest)),
    ]
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("manifest serializes");
    text.push('\n');
    text
}

fn entry<O>(generator: &mut SchemaGenerator) -> Value
where
    O: Operation,
    O::Params: JsonSchema,
    O::Output: JsonSchema,
{
    let spec = ops::spec::<O>()
        .unwrap_or_else(|| panic!("{} ({}) has no catalog row", O::NAME, O::METHOD));
    json!({
        "name": O::NAME,
        "method": O::METHOD,
        "id": spec.id,
        "version": spec.version,
        "kind": spec.kind.as_str(),
        "reply": spec.reply.as_str(),
        "params": generator.subschema_for::<O::Params>(),
        "output": generator.subschema_for::<O::Output>(),
    })
}

/// The manifest document. `$ref`s resolve against its own `$defs`.
pub fn manifest() -> Value {
    let mut generator = SchemaGenerator::default();
    let operations = vec![
        entry::<ops::DescribeApi>(&mut generator),
        entry::<ops::GetEngineInfo>(&mut generator),
        entry::<ops::GetTaskCommandSnapshot>(&mut generator),
        entry::<ops::GetTaskCommandReceipt>(&mut generator),
        entry::<ops::ExecuteTaskCommand>(&mut generator),
        entry::<ops::WatchTaskCommands>(&mut generator),
    ];
    let names: Vec<(&str, &str)> = operations
        .iter()
        .map(|op| {
            (
                op["name"].as_str().expect("name"),
                op["method"].as_str().expect("method"),
            )
        })
        .collect();
    assert_eq!(
        names.as_slice(),
        ops::OPERATIONS,
        "generated operations and ops::OPERATIONS differ"
    );
    let server = generator.subschema_for::<luna_rpc::ServerFrame>();
    let client = generator.subschema_for::<luna_rpc::ClientFrame>();
    let watch_update = generator.subschema_for::<crate::TaskCommandUpdate>();
    let intents: Vec<Value> = TaskIntent::KINDS
        .iter()
        .map(|(kind, fences)| json!({ "kind": kind, "fences": fences }))
        .collect();
    json!({
        "sdkVersion": crate::SDK_VERSION,
        "apiVersion": crate::API_VERSION,
        "operations": operations,
        "intents": intents,
        "errorKinds": crate::SdkError::KINDS,
        "frames": { "server": server, "client": client },
        "watchUpdate": watch_update,
        "$defs": generator.definitions(),
    })
}

const HEADER: &str = "\
// Generated from the luna-sdk Rust types; do not edit. Regenerate with
// `scripts/cargo.sh run --locked -p luna-sdk --features schema --example generate`.
// `scripts/cargo.sh test --locked -p luna-sdk --features schema --test generated`
// fails when this file and the Rust types drift.
";

const MANIFEST_TYPES: &str = "
export type JsonSchema = boolean | { [keyword: string]: unknown };

export interface OperationEntry {
  name: OperationName;
  method: string;
  id: string;
  version: number;
  kind: string;
  reply: string;
  params: JsonSchema;
  output: JsonSchema;
}

export interface IntentEntry {
  kind: string;
  fences: { taskRevision: boolean; bindingGeneration: boolean; turn: boolean };
}

export interface Manifest {
  sdkVersion: string;
  apiVersion: number;
  operations: OperationEntry[];
  intents: IntentEntry[];
  errorKinds: ErrorKind[];
  frames: { server: JsonSchema; client: JsonSchema };
  /** One `TaskCommandWatch` update, as both SDKs and `luna inspect watch-commands` emit it. */
  watchUpdate: JsonSchema;
  $defs: Record<string, JsonSchema>;
}
";

fn typescript(manifest: &Value) -> String {
    let mut out = String::from(HEADER);
    let _ = writeln!(
        out,
        "\nexport const SDK_VERSION = {};",
        literal(&manifest["sdkVersion"])
    );
    let _ = writeln!(
        out,
        "export const API_VERSION = {};",
        literal(&manifest["apiVersion"])
    );
    let kinds: Vec<String> = manifest["errorKinds"]
        .as_array()
        .expect("error kinds")
        .iter()
        .map(literal)
        .collect();
    let _ = writeln!(out, "\nexport type ErrorKind = {};", kinds.join(" | "));
    out.push_str(MANIFEST_TYPES);
    let _ = writeln!(
        out,
        "\n/** Operations, intents, frames and every schema they reference. */\nexport const MANIFEST: Manifest = {};",
        serde_json::to_string_pretty(manifest).expect("manifest serializes")
    );
    for (name, schema) in manifest["$defs"].as_object().expect("definitions") {
        out.push('\n');
        if let Some(description) = schema.get("description").and_then(Value::as_str) {
            let _ = writeln!(out, "/** {} */", doc(description));
        }
        let _ = writeln!(out, "export type {} = {};", ident(name), ts_type(schema, 0));
    }
    out.push_str("\n/** Each operation's parameters and its result (or stream item). */\n");
    out.push_str("export interface Operations {\n");
    for op in manifest["operations"].as_array().expect("operations") {
        let _ = writeln!(
            out,
            "  {}: {{ params: {}; output: {} }};",
            op["name"].as_str().expect("name"),
            ts_type(&op["params"], 1),
            ts_type(&op["output"], 1)
        );
    }
    out.push_str("}\n\nexport type OperationName = keyof Operations;\n");
    out
}

/// A TypeScript type for the schema subset the Rust types produce.
fn ts_type(schema: &Value, indent: usize) -> String {
    let Value::Object(object) = schema else {
        return if schema == &Value::Bool(false) {
            "never".into()
        } else {
            "unknown".into()
        };
    };
    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        return reference
            .strip_prefix("#/$defs/")
            .map_or_else(|| "unknown".into(), ident);
    }
    if let Some(value) = object.get("const") {
        return literal(value);
    }
    if let Some(Value::Array(values)) = object.get("enum") {
        return union(values.iter().map(literal).collect());
    }
    let mut parts = Vec::new();
    if let Some(base) = base_type(object, indent) {
        parts.push(base);
    }
    for keyword in ["oneOf", "anyOf"] {
        if let Some(Value::Array(variants)) = object.get(keyword) {
            let variants: Vec<String> = variants.iter().map(|v| ts_type(v, indent)).collect();
            // Variants that only constrain presence (the frame schemas) add no type.
            if !variants.iter().any(|variant| variant == "unknown") {
                parts.push(union(variants));
            }
        }
    }
    if let Some(Value::Array(all)) = object.get("allOf") {
        parts.extend(
            all.iter()
                .map(|v| ts_type(v, indent))
                .filter(|part| part != "unknown"),
        );
    }
    match parts.len() {
        0 => "unknown".into(),
        1 => parts.remove(0),
        _ => parts
            .into_iter()
            .map(|part| {
                if part.contains('|') {
                    format!("({part})")
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join(" & "),
    }
}

fn base_type(object: &Map<String, Value>, indent: usize) -> Option<String> {
    let kinds: Vec<&str> = match object.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ if object.contains_key("properties") => vec!["object"],
        _ => return None,
    };
    Some(union(
        kinds
            .into_iter()
            .map(|kind| match kind {
                "null" => "null".to_owned(),
                "string" => "string".to_owned(),
                "integer" | "number" => "number".to_owned(),
                "boolean" => "boolean".to_owned(),
                "array" => format!(
                    "Array<{}>",
                    object
                        .get("items")
                        .map_or_else(|| "unknown".into(), |items| ts_type(items, indent))
                ),
                "object" => object_type(object, indent),
                _ => "unknown".to_owned(),
            })
            .collect(),
    ))
}

fn object_type(object: &Map<String, Value>, indent: usize) -> String {
    let additional = object.get("additionalProperties");
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .filter(|properties| !properties.is_empty());
    let Some(properties) = properties else {
        return match additional {
            Some(schema) if schema.is_object() => {
                format!("Record<string, {}>", ts_type(schema, indent))
            }
            Some(Value::Bool(false)) => "Record<string, never>".into(),
            _ if object.contains_key("properties") => "Record<string, never>".into(),
            _ => "Record<string, unknown>".into(),
        };
    };
    let required: BTreeSet<&str> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let pad = "  ".repeat(indent + 1);
    let mut out = String::from("{\n");
    for (key, value) in properties {
        if let Some(description) = value.get("description").and_then(Value::as_str) {
            let _ = writeln!(out, "{pad}/** {} */", doc(description));
        }
        let optional = if required.contains(key.as_str()) {
            ""
        } else {
            "?"
        };
        let _ = writeln!(
            out,
            "{pad}{}{optional}: {};",
            property(key),
            ts_type(value, indent + 1)
        );
    }
    if matches!(additional, Some(Value::Object(_)) | Some(Value::Bool(true))) {
        let _ = writeln!(out, "{pad}[key: string]: unknown;");
    }
    out.push_str(&"  ".repeat(indent));
    out.push('}');
    out
}

fn union(mut parts: Vec<String>) -> String {
    let mut seen = BTreeSet::new();
    parts.retain(|part| seen.insert(part.clone()));
    match parts.len() {
        0 => "never".into(),
        1 => parts.remove(0),
        _ => parts.join(" | "),
    }
}

fn literal(value: &Value) -> String {
    serde_json::to_string(value).expect("literal serializes")
}

fn ident(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn property(key: &str) -> String {
    let plain = key
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if plain {
        key.to_owned()
    } else {
        literal(&Value::String(key.to_owned()))
    }
}

fn doc(text: &str) -> String {
    text.replace("*/", "*\\/").replace('\n', " ")
}
