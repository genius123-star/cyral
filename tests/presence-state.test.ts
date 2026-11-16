import { describe, expect, it } from "vitest";

import {
  createStartupPresence,
  enterInteraction,
  enableCompanion,
  exitInteraction,
  grantObservation,
  stopCompanion,
} from "../src/features/companion/presence-state";

describe("presence state", () => {
  it("always starts in standby without restoring companion permission", () => {
    expect(createStartupPresence()).toEqual({
      baseMode: "standby",
      interaction: "closed",
      observationPermission: "not-requested",
    });
  });

  it("requests permission before companion observation starts", () => {
    const state = enableCompanion(createStartupPresence());

    expect(state.baseMode).toBe("standby");
    expect(state.observationPermission).toBe("requested");
    expect(grantObservation(state).baseMode).toBe("companion");
  });

  it("keeps companion active while interaction opens and closes", () => {
    const companion = grantObservation(enableCompanion(createStartupPresence()));
    const interacting = enterInteraction(companion);

    expect(interacting.interaction).toBe("open");
    expect(interacting.baseMode).toBe("companion");
    expect(exitInteraction(interacting)).toEqual(companion);
  });

  it("stops observation without preserving permission across a restart", () => {
    const companion = grantObservation(enableCompanion(createStartupPresence()));

    expect(stopCompanion(companion)).toEqual(createStartupPresence());
  });
});
