// Runtime validation against the generated JSON Schemas (CORE-05). The Rust
// SDK validates by decoding into the types the schemas are generated from, so
// both SDKs accept the same JSON. This covers the draft 2020-12 keywords those
// types produce; unknown keywords (such as `format`) constrain nothing.

import type { JsonSchema } from './generated.ts';

type Schema = { [keyword: string]: unknown };

/** The first way `value` breaks `schema`, or `null` when it conforms. */
export function validate(
  schema: JsonSchema,
  value: unknown,
  defs: Record<string, JsonSchema>,
  path = '$',
): string | null {
  if (schema === true) return null;
  if (schema === false) return `${path}: no value is allowed here`;
  const s = schema as Schema;
  if (typeof s.$ref === 'string') {
    const name = s.$ref.startsWith('#/$defs/') ? s.$ref.slice('#/$defs/'.length) : undefined;
    const target = name === undefined ? undefined : defs[name];
    if (target === undefined) return `${path}: unresolved reference ${s.$ref}`;
    const problem = validate(target, value, defs, path);
    if (problem !== null) return problem;
  }
  if ('const' in s && !equal(s.const, value)) {
    return `${path}: expected ${JSON.stringify(s.const)}`;
  }
  if (Array.isArray(s.enum) && !s.enum.some((allowed) => equal(allowed, value))) {
    return `${path}: expected one of ${JSON.stringify(s.enum)}`;
  }
  if (s.type !== undefined) {
    const types = (Array.isArray(s.type) ? s.type : [s.type]) as string[];
    if (!types.some((type) => hasType(type, value))) {
      return `${path}: expected ${types.join(' or ')}, got ${describe(value)}`;
    }
  }
  if (typeof value === 'number') {
    if (typeof s.minimum === 'number' && value < s.minimum) return `${path}: below ${s.minimum}`;
    if (typeof s.maximum === 'number' && value > s.maximum) return `${path}: above ${s.maximum}`;
  }
  if (typeof value === 'string') {
    if (typeof s.minLength === 'number' && value.length < s.minLength) return `${path}: too short`;
    if (typeof s.maxLength === 'number' && value.length > s.maxLength) return `${path}: too long`;
  }
  if (Array.isArray(value)) {
    if (s.items !== undefined) {
      for (let index = 0; index < value.length; index++) {
        const problem = validate(s.items as JsonSchema, value[index], defs, `${path}[${index}]`);
        if (problem !== null) return problem;
      }
    }
    if (typeof s.minItems === 'number' && value.length < s.minItems) return `${path}: too few items`;
    if (typeof s.maxItems === 'number' && value.length > s.maxItems) return `${path}: too many items`;
  }
  if (isObject(value)) {
    for (const name of (Array.isArray(s.required) ? s.required : []) as string[]) {
      if (!Object.hasOwn(value, name)) return `${path}: missing ${name}`;
    }
    const properties = isObject(s.properties) ? (s.properties as Record<string, JsonSchema>) : {};
    for (const [name, property] of Object.entries(properties)) {
      if (Object.hasOwn(value, name)) {
        const problem = validate(property, value[name], defs, `${path}.${name}`);
        if (problem !== null) return problem;
      }
    }
    if (s.additionalProperties !== undefined) {
      for (const name of Object.keys(value)) {
        if (Object.hasOwn(properties, name)) continue;
        const problem = validate(s.additionalProperties as JsonSchema, value[name], defs, `${path}.${name}`);
        if (problem !== null) return problem;
      }
    }
  }
  if (Array.isArray(s.allOf)) {
    for (const part of s.allOf as JsonSchema[]) {
      const problem = validate(part, value, defs, path);
      if (problem !== null) return problem;
    }
  }
  if (Array.isArray(s.anyOf)) {
    const parts = s.anyOf as JsonSchema[];
    if (!parts.some((part) => validate(part, value, defs, path) === null)) {
      return `${path}: matches none of ${parts.length} alternatives`;
    }
  }
  if (Array.isArray(s.oneOf)) {
    const parts = s.oneOf as JsonSchema[];
    const matches = parts.filter((part) => validate(part, value, defs, path) === null).length;
    if (matches !== 1) return `${path}: matches ${matches} of ${parts.length} exclusive alternatives`;
  }
  if (s.not !== undefined && validate(s.not as JsonSchema, value, defs, path) === null) {
    return `${path}: matches a forbidden shape`;
  }
  return null;
}

function hasType(type: string, value: unknown): boolean {
  switch (type) {
    case 'null':
      return value === null;
    case 'boolean':
      return typeof value === 'boolean';
    case 'string':
      return typeof value === 'string';
    case 'number':
      return typeof value === 'number' && Number.isFinite(value);
    case 'integer':
      return Number.isInteger(value);
    case 'array':
      return Array.isArray(value);
    case 'object':
      return isObject(value);
    default:
      return false;
  }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function describe(value: unknown): string {
  if (value === null) return 'null';
  if (Array.isArray(value)) return 'array';
  return typeof value;
}

function equal(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (Array.isArray(left) && Array.isArray(right)) {
    return left.length === right.length && left.every((item, index) => equal(item, right[index]));
  }
  if (isObject(left) && isObject(right)) {
    const keys = Object.keys(left);
    return (
      keys.length === Object.keys(right).length &&
      keys.every((key) => Object.hasOwn(right, key) && equal(left[key], right[key]))
    );
  }
  return false;
}
