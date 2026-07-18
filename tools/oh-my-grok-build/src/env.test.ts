import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { sanitizeUserEnv } from "./env.js";

describe("env", () => {
  it("keeps API-key env vars", () => {
    assert.deepEqual(sanitizeUserEnv({ OPENAI_API_KEY: "sk-123", CODEX_API_KEY: "sk-456" }), {
      OPENAI_API_KEY: "sk-123",
      CODEX_API_KEY: "sk-456",
    });
  });

  it("drops dangerous env vars", () => {
    assert.deepEqual(
      sanitizeUserEnv({
        PATH: "/tmp/evil",
        LD_PRELOAD: "/tmp/evil.so",
        HOME: "/tmp",
        OPENAI_API_KEY: "sk-123",
      }),
      { OPENAI_API_KEY: "sk-123" }
    );
  });

  it("returns an empty object for undefined input", () => {
    assert.deepEqual(sanitizeUserEnv(undefined), {});
  });

  it("rejects keys that do not end with _API_KEY", () => {
    assert.deepEqual(
      sanitizeUserEnv({
        FOO_APIKEY: "bar",
        FOO_KEY: "bar",
        MY_API_KEY: "ok",
      }),
      { MY_API_KEY: "ok" }
    );
  });
});
