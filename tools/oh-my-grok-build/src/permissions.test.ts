import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { makePermissionResponse, selectPermissionOption } from "./permissions.js";

describe("permissions", () => {
  it("selects allow_once by default", () => {
    const options = [
      { optionId: "cancel", name: "Cancel", kind: "cancel" },
      { optionId: "allow_once", name: "Allow once", kind: "allow_once" },
      { optionId: "allow_always", name: "Always allow", kind: "allow_always" },
    ];
    assert.strictEqual(selectPermissionOption(options), "allow_once");
  });

  it("selects allow_always in yolo mode", () => {
    const options = [
      { optionId: "allow_once", name: "Allow once", kind: "allow_once" },
      { optionId: "allow_always", name: "Always allow", kind: "allow_always" },
    ];
    assert.strictEqual(selectPermissionOption(options, true), "allow_always");
  });

  it("falls back to generic allow when no allow_once", () => {
    const options = [
      { optionId: "deny", name: "Deny", kind: "deny" },
      { optionId: "approve", name: "Approve", kind: "approve" },
    ];
    assert.strictEqual(selectPermissionOption(options), "approve");
  });

  it("returns undefined when no allow option exists", () => {
    const options = [
      { optionId: "deny", name: "Deny", kind: "deny" },
      { optionId: "cancel", name: "Cancel", kind: "cancel" },
    ];
    assert.strictEqual(selectPermissionOption(options), undefined);
  });

  it("does not select cancel or deny as a fallback", () => {
    const options = [
      { optionId: "cancel", name: "Cancel", kind: "cancel" },
      { optionId: "deny", name: "Deny", kind: "deny" },
    ];
    assert.strictEqual(selectPermissionOption(options), undefined);
  });

  it("makes a selected response", () => {
    assert.deepStrictEqual(makePermissionResponse("id"), {
      outcome: { outcome: "selected", optionId: "id" },
    });
  });

  it("makes a cancelled response", () => {
    assert.deepStrictEqual(makePermissionResponse(undefined), {
      outcome: { outcome: "cancelled" },
    });
  });
});
