export type BaseMode = "standby" | "companion";
export type InteractionState = "closed" | "open";
export type ObservationPermission = "not-requested" | "requested" | "granted-this-boot";

export interface PresenceState {
  baseMode: BaseMode;
  interaction: InteractionState;
  observationPermission: ObservationPermission;
}

export function createStartupPresence(): PresenceState {
  return {
    baseMode: "standby",
    interaction: "closed",
    observationPermission: "not-requested",
  };
}

export function enableCompanion(state: PresenceState): PresenceState {
  return {
    ...state,
    observationPermission: "requested",
  };
}

export function grantObservation(state: PresenceState): PresenceState {
  return { ...state, baseMode: "companion", observationPermission: "granted-this-boot" };
}

export function stopCompanion(state: PresenceState): PresenceState {
  return { ...state, baseMode: "standby", observationPermission: "not-requested" };
}

export function enterInteraction(state: PresenceState): PresenceState {
  return { ...state, interaction: "open" };
}

export function exitInteraction(state: PresenceState): PresenceState {
  return { ...state, interaction: "closed" };
}
