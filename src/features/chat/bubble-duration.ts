export function bubbleDurationMs(text: string): number {
  const characters = Array.from(text.trim()).length;
  return Math.min(24_000, Math.max(6_000, 4_000 + Math.ceil(characters / 10) * 600));
}
