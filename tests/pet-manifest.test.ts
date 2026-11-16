import { describe, expect, it, vi } from "vitest";
import {
  actionSpec,
  IDLE_FALLBACK,
  IDLE_ACTION,
  IDLE_ACTIONS,
  loadPetManifest,
  preloadActionImages,
  resolveAction,
  WALK_ACTION,
  ENTER_ACTION,
  EXIT_ACTION,
  PEEK_ACTIONS,
  type PetActionSpec,
} from "../src/features/pet/pet-manifest";

const idleSpec: PetActionSpec = {
  action: IDLE_ACTION,
  strip: "/assets/characters/cyral/idle-blink-row-v1.png",
  frames: 4,
  frameWidth: 180,
  frameHeight: 226,
  durationMs: 1000,
  loop: true,
};

const walkSpec: PetActionSpec = {
  action: WALK_ACTION,
  strip: "/assets/characters/cyral/walk-row.png",
  frames: 6,
  frameWidth: 180,
  frameHeight: 226,
  durationMs: 2400,
  loop: true,
  required: true,
};

describe("pet-manifest", () => {
  it("uses only the new blink animation as the idle action", () => {
    expect(IDLE_ACTION).toBe("idle-blink");
    expect(IDLE_ACTIONS).toEqual(["idle-blink"]);
    expect(IDLE_FALLBACK).toEqual(idleSpec);
  });

  it("declares compact transitions and every screen-edge peek action", () => {
    expect(ENTER_ACTION).toBe("enter");
    expect(EXIT_ACTION).toBe("exit");
    expect(PEEK_ACTIONS).toEqual(["peek-left", "peek-right", "peek-bottom", "peek-top"]);
  });
  it("loads and parses the animation manifest", async () => {
    const fetchImpl = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ version: 1, actions: [idleSpec, walkSpec] }),
    });

    const manifest = await loadPetManifest(fetchImpl as unknown as typeof fetch);

    expect(manifest.actions).toHaveLength(2);
    expect(actionSpec(manifest, WALK_ACTION)).toEqual(walkSpec);
  });

  it("preloads only successfully loaded strips", async () => {
    const createImage = (src: string) => {
      const image = {
        onload: undefined as (() => void) | undefined,
        onerror: undefined as (() => void) | undefined,
      };
      queueMicrotask(() => {
        if (src.includes("idle-blink-row-v1")) image.onload?.();
        else image.onerror?.();
      });
      return image as unknown as HTMLImageElement;
    };

    const available = await preloadActionImages([idleSpec, walkSpec], createImage);

    expect(available.has(IDLE_ACTION)).toBe(true);
    expect(available.has(WALK_ACTION)).toBe(false);
  });

  it("falls back to the blink idle when the requested action is missing or unloaded", () => {
    const manifest = { version: 1, actions: [idleSpec, walkSpec] };

    expect(resolveAction(WALK_ACTION, new Set([WALK_ACTION]), manifest)).toEqual(walkSpec);
    expect(resolveAction(WALK_ACTION, new Set([IDLE_ACTION]), manifest)).toEqual(idleSpec);
    expect(resolveAction("nope", new Set(), manifest)).toEqual(idleSpec);
    expect(resolveAction("nope", new Set(), undefined)).toEqual(IDLE_FALLBACK);
  });
});
