/**
 * JSON Schema for AT Protocol Lexicon documents (v1).
 *
 * Based on the official spec at https://atproto.com/specs/lexicon and the
 * authoritative Zod validators in @atproto/lex-document.
 *
 * Used by Monaco's JSON language service to provide autocompletion,
 * validation, and hover documentation in the lexicon JSON editor.
 */

/* eslint-disable @typescript-eslint/no-explicit-any */

import { NSID_PATTERN } from "@happyview/nsid";

export const LEXICON_SCHEMA_URI = "https://atproto.com/schemas/lexicon-v1.json";

export const lexiconJsonSchema: Record<string, any> = {
  $schema: "http://json-schema.org/draft-07/schema#",
  $id: LEXICON_SCHEMA_URI,
  title: "AT Protocol Lexicon",
  description: "Schema for AT Protocol Lexicon definition documents.",
  type: "object",
  properties: {
    $type: {
      type: "string",
      description: "Record type identifier.",
      const: "com.atproto.lexicon.schema",
    },
    lexicon: {
      type: "integer",
      description: "Lexicon language version. Must be 1.",
      enum: [1],
    },
    id: {
      type: "string",
      description:
        "Namespaced Identifier (NSID) for this lexicon, e.g. 'app.bsky.feed.post'. Minimum 3 dot-separated segments.",
      pattern: NSID_PATTERN,
      maxLength: 317,
    },
    description: {
      type: "string",
      description: "Human-readable description of the lexicon.",
    },
    defs: {
      type: "object",
      description:
        "Map of definition names to type definitions. Primary types (record, query, procedure, subscription) must use the key 'main'.",
      properties: {
        main: { $ref: "#/definitions/mainDef" },
      },
      additionalProperties: { $ref: "#/definitions/nonPrimaryDef" },
      minProperties: 1,
    },
  },
  required: ["lexicon", "id", "defs"],
  additionalProperties: false,

  definitions: {
    // ─── Top-level def unions ───────────────────────────────────

    mainDef: {
      description: "A definition under the 'main' key — can be any type.",
      oneOf: [
        { $ref: "#/definitions/recordDef" },
        { $ref: "#/definitions/queryDef" },
        { $ref: "#/definitions/procedureDef" },
        { $ref: "#/definitions/subscriptionDef" },
        { $ref: "#/definitions/objectDef" },
        { $ref: "#/definitions/arrayDef" },
        { $ref: "#/definitions/tokenDef" },
        { $ref: "#/definitions/booleanDef" },
        { $ref: "#/definitions/integerDef" },
        { $ref: "#/definitions/stringDef" },
        { $ref: "#/definitions/unknownDef" },
        { $ref: "#/definitions/bytesDef" },
        { $ref: "#/definitions/cidLinkDef" },
        { $ref: "#/definitions/blobDef" },
      ],
    },

    nonPrimaryDef: {
      description:
        "A definition under a non-'main' key — primary types (record, query, procedure, subscription) are not allowed here.",
      oneOf: [
        { $ref: "#/definitions/objectDef" },
        { $ref: "#/definitions/arrayDef" },
        { $ref: "#/definitions/tokenDef" },
        { $ref: "#/definitions/booleanDef" },
        { $ref: "#/definitions/integerDef" },
        { $ref: "#/definitions/stringDef" },
        { $ref: "#/definitions/unknownDef" },
        { $ref: "#/definitions/bytesDef" },
        { $ref: "#/definitions/cidLinkDef" },
        { $ref: "#/definitions/blobDef" },
      ],
    },

    // ─── Primary types ──────────────────────────────────────────

    recordDef: {
      type: "object",
      description:
        "A record type — stored in a user's repository. Must be the 'main' definition.",
      properties: {
        type: { type: "string", const: "record" },
        description: { type: "string", description: "Human-readable description." },
        key: {
          type: "string",
          description:
            "Record key type:\n• 'tid' — Timestamp Identifier (most common)\n• 'nsid' — Key must be a valid NSID\n• 'any' — Any valid record key string\n• 'literal:<value>' — Fixed key, e.g. 'literal:self' for singleton records",
          examples: ["tid", "any", "nsid", "literal:self"],
        },
        record: {
          $ref: "#/definitions/objectDef",
          description: "Object schema defining the record's fields.",
        },
      },
      required: ["type", "record"],
      additionalProperties: false,
    },

    queryDef: {
      type: "object",
      description:
        "An XRPC query (HTTP GET). Must be the 'main' definition.",
      properties: {
        type: { type: "string", const: "query" },
        description: { type: "string", description: "Human-readable description." },
        parameters: { $ref: "#/definitions/paramsDef" },
        output: { $ref: "#/definitions/xrpcBody" },
        errors: { $ref: "#/definitions/xrpcErrors" },
      },
      required: ["type"],
      additionalProperties: false,
    },

    procedureDef: {
      type: "object",
      description:
        "An XRPC procedure (HTTP POST). Must be the 'main' definition.",
      properties: {
        type: { type: "string", const: "procedure" },
        description: { type: "string", description: "Human-readable description." },
        parameters: { $ref: "#/definitions/paramsDef" },
        input: { $ref: "#/definitions/xrpcBody" },
        output: { $ref: "#/definitions/xrpcBody" },
        errors: { $ref: "#/definitions/xrpcErrors" },
      },
      required: ["type"],
      additionalProperties: false,
    },

    subscriptionDef: {
      type: "object",
      description:
        "An XRPC subscription (WebSocket event stream). Must be the 'main' definition.",
      properties: {
        type: { type: "string", const: "subscription" },
        description: { type: "string", description: "Human-readable description." },
        parameters: { $ref: "#/definitions/paramsDef" },
        message: {
          type: "object",
          description: "Schema for subscription messages.",
          properties: {
            description: { type: "string" },
            schema: { $ref: "#/definitions/unionDef" },
          },
          required: ["schema"],
          additionalProperties: false,
        },
        errors: { $ref: "#/definitions/xrpcErrors" },
      },
      required: ["type"],
      additionalProperties: false,
    },

    // ─── Complex types ──────────────────────────────────────────

    objectDef: {
      type: "object",
      description: "An object with named properties.",
      properties: {
        type: { type: "string", const: "object" },
        description: { type: "string", description: "Human-readable description." },
        properties: {
          type: "object",
          description: "Map of property names to their type schemas.",
          additionalProperties: { $ref: "#/definitions/objectProperty" },
        },
        required: {
          type: "array",
          description:
            "Property names that must be present. Each must be a key in 'properties'.",
          items: { type: "string" },
          uniqueItems: true,
        },
        nullable: {
          type: "array",
          description:
            "Property names that may be null. Each must be a key in 'properties'.",
          items: { type: "string" },
          uniqueItems: true,
        },
      },
      required: ["type", "properties"],
      additionalProperties: false,
    },

    arrayDef: {
      type: "object",
      description: "An array of items of a single type.",
      properties: {
        type: { type: "string", const: "array" },
        description: { type: "string", description: "Human-readable description." },
        items: {
          $ref: "#/definitions/arrayItem",
          description: "Schema for each element in the array.",
        },
        minLength: {
          type: "integer",
          description: "Minimum number of elements.",
          minimum: 0,
        },
        maxLength: {
          type: "integer",
          description: "Maximum number of elements.",
          minimum: 0,
        },
      },
      required: ["type", "items"],
      additionalProperties: false,
    },

    tokenDef: {
      type: "object",
      description:
        "A named symbol with no data representation. Used as values in 'knownValues' string enumerations.",
      properties: {
        type: { type: "string", const: "token" },
        description: { type: "string", description: "Clarifies the token's meaning." },
      },
      required: ["type"],
      additionalProperties: false,
    },

    // ─── Primitive types ────────────────────────────────────────

    booleanDef: {
      type: "object",
      description: "A boolean value.",
      properties: {
        type: { type: "string", const: "boolean" },
        description: { type: "string" },
        default: { type: "boolean", description: "Default value. Mutually exclusive with 'const'." },
        const: { type: "boolean", description: "Fixed value. Mutually exclusive with 'default'." },
      },
      required: ["type"],
      additionalProperties: false,
    },

    integerDef: {
      type: "object",
      description: "A 64-bit signed integer (limit to 53-bit for JS compatibility).",
      properties: {
        type: { type: "string", const: "integer" },
        description: { type: "string" },
        default: { type: "integer", description: "Default value. Mutually exclusive with 'const'." },
        minimum: { type: "integer", description: "Minimum allowed value (inclusive)." },
        maximum: { type: "integer", description: "Maximum allowed value (inclusive)." },
        enum: {
          type: "array",
          items: { type: "integer" },
          description: "Closed set of allowed values.",
          uniqueItems: true,
        },
        const: { type: "integer", description: "Fixed value. Mutually exclusive with 'default'." },
      },
      required: ["type"],
      additionalProperties: false,
    },

    stringDef: {
      type: "object",
      description: "A UTF-8 string value.",
      properties: {
        type: { type: "string", const: "string" },
        description: { type: "string" },
        format: {
          type: "string",
          description: "Semantic format constraint for the string value.",
          enum: [
            "datetime",
            "uri",
            "at-uri",
            "did",
            "handle",
            "at-identifier",
            "nsid",
            "cid",
            "language",
            "tid",
            "record-key",
          ],
        },
        default: { type: "string", description: "Default value. Mutually exclusive with 'const'." },
        minLength: { type: "integer", description: "Minimum length in UTF-8 bytes.", minimum: 0 },
        maxLength: { type: "integer", description: "Maximum length in UTF-8 bytes.", minimum: 0 },
        minGraphemes: {
          type: "integer",
          description: "Minimum length in Unicode grapheme clusters.",
          minimum: 0,
        },
        maxGraphemes: {
          type: "integer",
          description: "Maximum length in Unicode grapheme clusters.",
          minimum: 0,
        },
        enum: {
          type: "array",
          items: { type: "string" },
          description: "Closed set of allowed values.",
          uniqueItems: true,
        },
        const: { type: "string", description: "Fixed value. Mutually exclusive with 'default'." },
        knownValues: {
          type: "array",
          items: { type: "string" },
          description:
            "Suggested values (open set, not strictly enforced). Entries may be token references.",
          uniqueItems: true,
        },
      },
      required: ["type"],
      additionalProperties: false,
    },

    unknownDef: {
      type: "object",
      description:
        "Accepts any valid data model value. Not recommended for record definitions.",
      properties: {
        type: { type: "string", const: "unknown" },
        description: { type: "string" },
      },
      required: ["type"],
      additionalProperties: false,
    },

    // ─── IPLD types ─────────────────────────────────────────────

    bytesDef: {
      type: "object",
      description:
        "Raw bytes. Encoded in JSON as { \"$bytes\": \"<base64>\" }.",
      properties: {
        type: { type: "string", const: "bytes" },
        description: { type: "string" },
        minLength: { type: "integer", description: "Minimum byte length.", minimum: 0 },
        maxLength: { type: "integer", description: "Maximum byte length.", minimum: 0 },
      },
      required: ["type"],
      additionalProperties: false,
    },

    cidLinkDef: {
      type: "object",
      description:
        "A CID link to content-addressed data. Encoded in JSON as { \"$link\": \"<CID>\" }.",
      properties: {
        type: { type: "string", const: "cid-link" },
        description: { type: "string" },
      },
      required: ["type"],
      additionalProperties: false,
    },

    // ─── Reference types ────────────────────────────────────────

    refDef: {
      type: "object",
      description:
        "A reference to another definition. Use '#name' for local refs or 'com.example.lexicon#name' for external.",
      properties: {
        type: { type: "string", const: "ref" },
        description: { type: "string" },
        ref: {
          type: "string",
          description:
            "Reference string. Local: '#defName'. External: 'com.example.lexicon' or 'com.example.lexicon#defName'.",
          examples: ["#myObject", "com.atproto.repo.strongRef"],
        },
      },
      required: ["type", "ref"],
      additionalProperties: false,
    },

    unionDef: {
      type: "object",
      description:
        "A discriminated union of object/record types. Each variant must include a '$type' field in encoded data.",
      properties: {
        type: { type: "string", const: "union" },
        description: { type: "string" },
        refs: {
          type: "array",
          items: { type: "string" },
          description:
            "References to object or record definitions that are members of this union.",
          minItems: 1,
        },
        closed: {
          type: "boolean",
          description:
            "If true, the union is closed — only the listed refs are valid. Defaults to false (open union).",
          default: false,
        },
      },
      required: ["type", "refs"],
      additionalProperties: false,
    },

    // ─── Blob type ──────────────────────────────────────────────

    blobDef: {
      type: "object",
      description: "A binary blob (e.g. image, video).",
      properties: {
        type: { type: "string", const: "blob" },
        description: { type: "string" },
        accept: {
          type: "array",
          items: { type: "string" },
          description:
            "Accepted MIME types. Use '*/*' for any. Glob suffix allowed, e.g. 'image/*'.",
          examples: [["image/png", "image/jpeg"], ["image/*"], ["*/*"]],
        },
        maxSize: {
          type: "integer",
          description: "Maximum blob size in bytes.",
          minimum: 0,
        },
      },
      required: ["type"],
      additionalProperties: false,
    },

    // ─── Composite property unions ──────────────────────────────

    objectProperty: {
      description: "A property in an object definition.",
      oneOf: [
        { $ref: "#/definitions/booleanDef" },
        { $ref: "#/definitions/integerDef" },
        { $ref: "#/definitions/stringDef" },
        { $ref: "#/definitions/unknownDef" },
        { $ref: "#/definitions/bytesDef" },
        { $ref: "#/definitions/cidLinkDef" },
        { $ref: "#/definitions/refDef" },
        { $ref: "#/definitions/unionDef" },
        { $ref: "#/definitions/blobDef" },
        { $ref: "#/definitions/arrayDef" },
      ],
    },

    arrayItem: {
      description:
        "Valid types for array items. Note: nested arrays and inline objects are not allowed — use a ref to an object definition instead.",
      oneOf: [
        { $ref: "#/definitions/booleanDef" },
        { $ref: "#/definitions/integerDef" },
        { $ref: "#/definitions/stringDef" },
        { $ref: "#/definitions/unknownDef" },
        { $ref: "#/definitions/bytesDef" },
        { $ref: "#/definitions/cidLinkDef" },
        { $ref: "#/definitions/refDef" },
        { $ref: "#/definitions/unionDef" },
        { $ref: "#/definitions/blobDef" },
      ],
    },

    // ─── XRPC helpers ───────────────────────────────────────────

    paramsDef: {
      type: "object",
      description: "XRPC query/procedure parameters (HTTP query string).",
      properties: {
        type: { type: "string", const: "params" },
        description: { type: "string" },
        required: {
          type: "array",
          items: { type: "string" },
          description: "Required parameter names. Each must be a key in 'properties'.",
          uniqueItems: true,
        },
        properties: {
          type: "object",
          description:
            "Map of parameter names to schemas. Only primitives and primitive arrays are allowed.",
          additionalProperties: { $ref: "#/definitions/paramProperty" },
        },
      },
      required: ["type", "properties"],
      additionalProperties: false,
    },

    paramProperty: {
      description:
        "Valid types for XRPC parameters: boolean, integer, string, unknown, or an array of primitives.",
      oneOf: [
        { $ref: "#/definitions/booleanDef" },
        { $ref: "#/definitions/integerDef" },
        { $ref: "#/definitions/stringDef" },
        { $ref: "#/definitions/unknownDef" },
        { $ref: "#/definitions/primitiveArrayDef" },
      ],
    },

    primitiveArrayDef: {
      type: "object",
      description: "An array whose items are a primitive type (for use in XRPC parameters).",
      properties: {
        type: { type: "string", const: "array" },
        description: { type: "string" },
        items: {
          oneOf: [
            { $ref: "#/definitions/booleanDef" },
            { $ref: "#/definitions/integerDef" },
            { $ref: "#/definitions/stringDef" },
            { $ref: "#/definitions/unknownDef" },
          ],
        },
        minLength: { type: "integer", minimum: 0 },
        maxLength: { type: "integer", minimum: 0 },
      },
      required: ["type", "items"],
      additionalProperties: false,
    },

    xrpcBody: {
      type: "object",
      description: "XRPC request/response body definition.",
      properties: {
        description: { type: "string" },
        encoding: {
          type: "string",
          description: "MIME type, e.g. 'application/json' or '*/*'.",
          examples: ["application/json", "*/*", "application/cbor"],
        },
        schema: {
          description: "Body schema — must be an object, ref, or union.",
          oneOf: [
            { $ref: "#/definitions/objectDef" },
            { $ref: "#/definitions/refDef" },
            { $ref: "#/definitions/unionDef" },
          ],
        },
      },
      required: ["encoding"],
      additionalProperties: false,
    },

    xrpcErrors: {
      type: "array",
      description: "Possible error responses.",
      items: {
        type: "object",
        properties: {
          name: {
            type: "string",
            description: "Short error name with no whitespace, e.g. 'InvalidRequest'.",
          },
          description: { type: "string", description: "Human-readable error description." },
        },
        required: ["name"],
        additionalProperties: false,
      },
    },
  },
};
