import { describe, expect, it } from "vitest";

import {
  chooseThinkingVariant,
  transitionThinkingAnimation,
} from "../src/features/pet/thinking-animation";

describe("thinking animation", () => {
  it("maps the full random range evenly across four variants", () => {
    expect([0, 0.25, 0.5, 0.75].map(chooseThinkingVariant)).toEqual(["A", "B", "C", "D"]);
  });

  it("starts randomly and stops on the first streamed text fragment", () => {
    expect(transitionThinkingAnimation(undefined, "start", () => 0.6)).toBe("C");
    expect(transitionThinkingAnimation("C", "delta", () => 0)).toBeUndefined();
  });

  it("keeps thinking when a streamed fragment has no visible text", () => {
    expect(transitionThinkingAnimation("B", "delta", () => 0, "\n  ")).toBe("B");
    expect(transitionThinkingAnimation("B", "delta", () => 0, "你好")).toBeUndefined();
  });

  it("also stops after completion or failure", () => {
    expect(transitionThinkingAnimation("A", "end", () => 0)).toBeUndefined();
    expect(transitionThinkingAnimation("D", "error", () => 0)).toBeUndefined();
  });
});
