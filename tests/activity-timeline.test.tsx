import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  ActivityTimeline,
  type CyralActivityEvent,
} from "../src/features/companion/ActivityTimeline";

const visualEvent: CyralActivityEvent = {
  id: 1,
  eventType: "screen_observation",
  source: "visual",
  appName: "WINWORD.EXE",
  windowTitle: "论文.docx",
  summary: "正在编辑论文的结论章节",
  observedAt: 1_800_000_000,
};

const metadataEvent: CyralActivityEvent = {
  id: 2,
  eventType: "screen_observation",
  source: "window",
  appName: "CHROME.EXE",
  windowTitle: null,
  summary: "当前应用：CHROME.EXE",
  observedAt: 1_700_000_000,
};

const data = [visualEvent, metadataEvent];

describe("ActivityTimeline", () => {
  it("describes Cyral observations instead of user activity duration", async () => {
    render(<ActivityTimeline onLoad={vi.fn().mockResolvedValue(data)} />);

    expect(await screen.findByText("WINWORD.EXE")).toBeVisible();
    expect(screen.getByText("论文.docx")).toBeVisible();
    expect(screen.getByText("正在编辑论文的结论章节")).toBeVisible();
    expect(screen.getByText("视觉摘要")).toBeVisible();
    expect(screen.getByText("窗口元数据")).toBeVisible();
    expect(screen.queryByText("5 分")).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getAllByRole("listitem")).toHaveLength(2));
  });

  it("does not expose boot-based user activity groups", async () => {
    render(<ActivityTimeline onLoad={vi.fn().mockResolvedValue(data)} />);

    await screen.findByText("WINWORD.EXE");
    expect(screen.queryByText(/本次开机/)).not.toBeInTheDocument();
    expect(screen.queryByText(/更早/)).not.toBeInTheDocument();
  });

  it("never labels an observation as an in-progress user session", async () => {
    render(<ActivityTimeline onLoad={vi.fn().mockResolvedValue(data)} />);

    await screen.findByText("WINWORD.EXE");
    expect(screen.queryByText(/进行中/)).not.toBeInTheDocument();
  });

  it("shows an empty state when there are no events", async () => {
    render(<ActivityTimeline onLoad={vi.fn().mockResolvedValue([])} />);

    expect(await screen.findByText(/还没有希莉尔活动记录/)).toBeVisible();
  });


  it("groups cross-day themes into a readable card with explicit controls", async () => {
    render(
      <ActivityTimeline
        onLoad={vi.fn().mockResolvedValue([])}
        onLoadThemes={vi.fn().mockResolvedValue([{
          identityKey: "resume.pdf",
          kind: "file",
          displayName: "个人简历.pdf",
          activeDays: 3,
          totalActiveSecs: 8_280,
          observationCount: 4,
          firstSeenAt: 1_800_000_000,
          lastSeenAt: 1_800_100_000,
          mutedUntilAt: null,
        }])}
      />,
    );

    expect(await screen.findByText("只保留跨日出现、可确认的活动线索。")).toBeVisible();
    expect(screen.getByText("累计 2 小时 18 分钟")).toBeVisible();
    expect(screen.getByLabelText("主题控制：个人简历.pdf")).toBeVisible();
  });
  it("shows an error state when loading fails", async () => {
    render(<ActivityTimeline onLoad={vi.fn().mockRejectedValue(new Error("读取失败"))} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("读取失败");
  });
});
