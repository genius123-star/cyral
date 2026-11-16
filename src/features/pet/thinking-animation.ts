export type ThinkingVariant = "A" | "B" | "C" | "D";
export type ThinkingEvent = "start" | "delta" | "end" | "error";

const THINKING_VARIANTS: ThinkingVariant[] = ["A", "B", "C", "D"];

export function chooseThinkingVariant(randomValue: number): ThinkingVariant {
  const index = Math.min(THINKING_VARIANTS.length - 1, Math.floor(randomValue * THINKING_VARIANTS.length));
  return THINKING_VARIANTS[Math.max(0, index)];
}

export function transitionThinkingAnimation(
  current: ThinkingVariant | undefined,
  event: ThinkingEvent,
  random: () => number = Math.random,
  fragment = "visible",
): ThinkingVariant | undefined {
  if (event === "start") return chooseThinkingVariant(random());
  if (event === "delta" && fragment.trim().length === 0) return current;
  return undefined;
}
