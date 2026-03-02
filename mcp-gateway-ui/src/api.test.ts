import { describe, expect, it } from "vitest";

import { normalizeBaseUrl } from "./api";

describe("normalizeBaseUrl", () => {
  it("adds scheme when missing", () => {
    expect(normalizeBaseUrl("127.0.0.1:8765")).toBe("http://127.0.0.1:8765");
  });

  it("trims trailing slash", () => {
    expect(normalizeBaseUrl("http://localhost:8765/")).toBe("http://localhost:8765");
  });
});