import { describe, expect, it } from "vitest";

import { sendsJson } from "./json-request";

describe("sendsJson", () => {
  it("is true for a DELETE with a JSON body", () => {
    expect(sendsJson({ method: "DELETE", body: "{}" })).toBe(true);
  });

  it("is false for a DELETE without a body", () => {
    expect(sendsJson({ method: "DELETE" })).toBe(false);
  });

  it("is false for a GET", () => {
    expect(sendsJson(undefined)).toBe(false);
    expect(sendsJson({ method: "GET" })).toBe(false);
  });

  it("is true for a POST, PUT or PATCH, with or without a body", () => {
    expect(sendsJson({ method: "POST" })).toBe(true);
    expect(sendsJson({ method: "PUT", body: "{}" })).toBe(true);
    expect(sendsJson({ method: "PATCH" })).toBe(true);
  });

  it("is false for a body that is not a JSON string", () => {
    expect(sendsJson({ method: "DELETE", body: new Blob(["x"]) })).toBe(false);
  });
});
