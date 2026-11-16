import { describe, expect, it, vi } from "vitest";

import { runChatRequest } from "../src/features/chat/chat-request";

describe("chat request lifecycle", () => {
  it("starts thinking before invoking the backend and ends through backend events", async () => {
    const events: string[] = [];
    const send = vi.fn(async () => {
      events.push("send");
      return "reply";
    });

    await expect(runChatRequest(
      () => { events.push("start"); },
      send,
      () => { events.push("error"); },
    )).resolves.toBe("reply");
    expect(events).toEqual(["start", "send"]);
  });

  it("ends thinking when a backend request fails before producing a reply", async () => {
    const events: string[] = [];
    const failure = new Error("weather unavailable");

    await expect(runChatRequest(
      () => { events.push("start"); },
      async () => { throw failure; },
      () => { events.push("error"); },
    )).rejects.toBe(failure);
    expect(events).toEqual(["start", "error"]);
  });
});
