import { vi } from "vitest";
import "@testing-library/jest-dom";

// jsdom does not implement scrollIntoView.
if (typeof Element !== "undefined" && !Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = vi.fn();
}
