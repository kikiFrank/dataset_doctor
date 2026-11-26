import type { ArtifactOwner } from "./generated/wire-primitives.ts";

const MAX_U64 = 18_446_744_073_709_551_615n;
const MIN_I64 = -9_223_372_036_854_775_808n;
const MAX_I64 = 9_223_372_036_854_775_807n;
const DECIMAL = /^(?:0|-?[1-9][0-9]*)$/u;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;

function readDecimal(input: unknown, minimum: bigint, maximum: bigint): bigint {
  if (
    typeof input !== "string" || input.length > 20 || input !== input.trim() ||
    !DECIMAL.test(input)
  ) {
    throw new TypeError("Expected a canonical decimal integer string");
  }
  const value = BigInt(input);
  if (value < minimum || value > maximum) {
    throw new RangeError("Decimal integer is out of range");
  }
  return value;
}

export function parseDecimalU64(input: unknown): bigint {
  return readDecimal(input, 0n, MAX_U64);
}

export function parseDecimalI64(input: unknown): bigint {
  return readDecimal(input, MIN_I64, MAX_I64);
}

export function readArtifactOwner(input: unknown): ArtifactOwner {
  if (input === null || typeof input !== "object" || Array.isArray(input)) {
    throw new TypeError("Expected an Artifact owner");
  }
  const { kind, id } = input as Record<string, unknown>;
  if (
    Object.keys(input).length !== 2 ||
    (kind !== "TASK" && kind !== "ASSET" && kind !== "REVIEW") ||
    typeof id !== "string" || id.length !== 36 || !UUID.test(id)
  ) {
    throw new TypeError("Invalid Artifact owner");
  }
  return { kind, id };
}
