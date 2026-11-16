import { describe, expect, it } from "vitest";

import { bubbleDurationMs } from "../src/features/chat/bubble-duration";

describe("bubbleDurationMs", () => {
  it("keeps short messages visible for at least six seconds", () => {
    expect(bubbleDurationMs("你好")).toBe(6_000);
  });

  it("grows with visible character count and caps at twenty-four seconds", () => {
    expect(bubbleDurationMs("一".repeat(50))).toBe(7_000);
    expect(bubbleDurationMs("一".repeat(1_000))).toBe(24_000);
  });
});
