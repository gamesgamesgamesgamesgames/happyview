// The reader and resolver over files the Lua interpreter rendered for real
// (`fixture/`, copied from the plugins repo's declarations fixture).

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

import {
  parseType,
  printType,
  readDeclarations,
  type DeclarationFile,
} from "./read";
import {
  handlerTypes,
  recordCollections,
  resolveChain,
  resolveMember,
  scriptBindings,
  signature,
  trailingChain,
} from "./resolve";

const FIXTURE = join(__dirname, "fixture");

function files(dir: string): DeclarationFile[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return files(path);
    return [
      { path: relative(FIXTURE, path), contents: readFileSync(path, "utf8") },
    ];
  });
}

const decls = readDeclarations(files(FIXTURE));

describe("types", () => {
  it.each([
    "string",
    "string[]",
    '"asc"|"desc"|nil',
    "table<string, any>",
    "happyview.db.RecordEnvelope|nil",
    "{ uri: string, id: string }",
    "(string|integer)[]",
  ])("%s reads and prints back", (text) => {
    const parsed = parseType(text);
    expect(parsed?.[1]).toBe(text.length);
    expect(printType(parsed![0])).toBe(text);
  });

  it("stops where the description starts", () => {
    const parsed = parseType("string|nil Its CID; null for a record");
    expect(printType(parsed![0])).toBe("string|nil");
    expect(parsed![1]).toBe("string|nil".length);
  });
});

describe("reading", () => {
  it("reads classes, fields and their descriptions", () => {
    const ctx = decls.classes.get("happyview.QueryCtx");
    expect(ctx?.description).toBe("ctx for an XRPC query");
    const caller = ctx?.fields.find((f) => f.name === "caller_did");
    expect(caller).toMatchObject({
      optional: true,
      description: "The caller's DID",
    });
    expect(
      decls.classes.get("happyview.Blob")?.fields.map((f) => f.name),
    ).toEqual(["$type", "ref", "mimeType", "size"]);
  });

  it("binds a module to the class its file returns", () => {
    expect(decls.modules.get("happyview.db")).toBe("happyview.db");
    const db = decls.classes.get("happyview.db");
    const records = db?.functions.find((f) => f.name === "records");
    expect(records).toMatchObject({
      method: false,
      description: "Start a query over one collection",
    });
    expect(printType(records!.returns!)).toBe("happyview.db.records.Object");
    expect(records?.params).toEqual([
      {
        name: "collection",
        optional: false,
        type: { kind: "name", name: "string" },
        description: "Lexicon collection NSID",
      },
    ]);
  });

  it("reads a builder's methods as methods", () => {
    const builder = decls.classes.get("happyview.db.records.Object");
    const where = builder?.functions.find((f) => f.name === "where");
    expect(where?.method).toBe(true);
    expect(where?.params.map((p) => p.name)).toEqual(["field", "op", "value"]);
    expect(decls.aliases.has("happyview.db.Operator")).toBe(true);
  });

  it("puts the job's controls on the job", () => {
    const job = decls.classes.get("happyview.Job");
    expect(job?.functions.map((f) => f.name)).toEqual([
      "progress",
      "should_stop",
      "wait",
    ]);
  });
});

describe("resolving", () => {
  it("types input and ctx from the trigger", () => {
    expect(handlerTypes(decls, "xrpc.query:com.example.feed.getPosts")).toEqual(
      {
        input: {
          kind: "name",
          name: "lexicons.com.example.feed.getPosts.Params",
        },
        ctx: { kind: "name", name: "happyview.QueryCtx" },
      },
    );
    expect(
      handlerTypes(decls, "record.create:com.example.feed.post").input,
    ).toEqual({
      kind: "name",
      name: "lexicons.com.example.feed.post.Event",
    });
    // A collection with no declarations still gets the generic event.
    expect(
      handlerTypes(decls, "record.delete:com.example.unknown").input,
    ).toEqual({
      kind: "name",
      name: "happyview.RecordEvent",
    });
    expect(handlerTypes(decls, "job.run:nightly")).toEqual({
      ctx: { kind: "name", name: "happyview.JobCtx" },
    });
  });

  it("reads a chain backward through calls and colons", () => {
    // Nothing typed after the colon yet is no name to complete.
    expect(trailingChain('db.records("a.b"):')).toBeUndefined();
    expect(
      trailingChain('db.records("a.b"):where("x", "=", f(1)):lim'),
    ).toEqual([
      { name: "db", call: false, colon: false },
      { name: "records", call: true, colon: false },
      { name: "where", call: true, colon: true },
      { name: "lim", call: false, colon: true },
    ]);
    expect(trailingChain("  return input.cursor")).toEqual([
      { name: "input", call: false, colon: false },
      { name: "cursor", call: false, colon: false },
    ]);
    expect(trailingChain("x = 1.")).toBeUndefined();
  });

  const source = [
    'local db = require("happyview.db")',
    'local q = db.records("com.example.feed.post"):where("text", "=", "hi")',
    "local page = q:run()",
    "for _, row in ipairs(page.records) do",
    "end",
  ].join("\n");
  const bindings = scriptBindings(
    decls,
    source,
    "record.index:com.example.feed.post",
  );

  it("follows requires, calls and loops", () => {
    expect(bindings.get("db")).toEqual({ kind: "name", name: "happyview.db" });
    expect(bindings.get("q")).toEqual({
      kind: "name",
      name: "happyview.db.records.Object",
    });
    expect(bindings.get("page")).toEqual({
      kind: "name",
      name: "happyview.db.RecordsPage",
    });
    expect(bindings.get("row")).toEqual({
      kind: "name",
      name: "happyview.db.RecordEnvelope",
    });
    expect(bindings.get("input")).toEqual({
      kind: "name",
      name: "lexicons.com.example.feed.post.Event",
    });
  });

  it("resolves through a nullable field", () => {
    // `record` is `Record|nil`; its fields are the record's.
    const type = resolveChain(decls, bindings, trailingChain("input.record")!);
    expect(printType(type!)).toBe("lexicons.com.example.feed.post.Record|nil");
    const text = resolveMember(
      decls,
      bindings,
      trailingChain("input.record.text")!,
    );
    expect(text?.field?.description).toBe("The post's text");
  });

  it("names a function's signature", () => {
    const member = resolveMember(decls, bindings, trailingChain("q:where")!);
    expect(signature(member!.fn!, "q")).toBe(
      "function q:where(field?: string|nil, op: happyview.db.Operator, value: string|number|boolean): happyview.db.records.Object",
    );
  });

  it("lists the record collections", () => {
    expect(recordCollections(decls)).toEqual(["com.example.feed.post"]);
  });
});
