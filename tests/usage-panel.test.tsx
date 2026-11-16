import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { UsagePanel, type MonthlyUsage, type UsagePoint } from "../src/features/panel/UsagePanel";

const monthly: MonthlyUsage = {
  month: "2026-08",
  usedUsd: 1.23,
  budgetUsd: 10,
  totalTokens: 1_234_567,
};

describe("UsagePanel", () => {
  it("shows the monthly summary and a 30-day token line chart", async () => {
    const points: UsagePoint[] = [
      { day: "2026-08-01", totalTokens: 1000 },
      { day: "2026-08-02", totalTokens: 2500 },
      { day: "2026-08-03", totalTokens: 1200 },
    ];
    const onLoadUsage = vi.fn().mockResolvedValue(monthly);
    const onLoadHistory = vi.fn().mockResolvedValue(points);

    render(<UsagePanel onLoadUsage={onLoadUsage} onLoadHistory={onLoadHistory} />);

    expect(await screen.findByText("$1.23")).toBeVisible();
    expect(screen.getByText(/上限 \$10\.00（12%）/)).toBeVisible();
    expect(screen.getByText(/共 1,234,567 tokens/)).toBeVisible();
    expect(screen.getByRole("img", { name: /近 30 天 token 消耗折线图/ })).toBeInTheDocument();
    expect(screen.getByText("2026-08-01")).toBeVisible();
    expect(screen.getByText("峰值 2,500 tokens")).toBeVisible();
    expect(screen.getByText("2026-08-03")).toBeVisible();
    await waitFor(() => expect(onLoadUsage).toHaveBeenCalledOnce());
    await waitFor(() => expect(onLoadHistory).toHaveBeenCalledOnce());
  });

  it("shows the empty state without usage records", async () => {
    render(
      <UsagePanel
        onLoadUsage={vi.fn().mockResolvedValue({ ...monthly, usedUsd: 0, totalTokens: 0 })}
        onLoadHistory={vi.fn().mockResolvedValue([])}
      />,
    );

    expect(await screen.findByText("近 30 天还没有用量记录。")).toBeVisible();
  });

  it("surfaces a readable error when loading fails", async () => {
    render(
      <UsagePanel
        onLoadUsage={vi.fn().mockRejectedValue(new Error("boom"))}
        onLoadHistory={vi.fn().mockResolvedValue([])}
      />,
    );

    expect(await screen.findByText("用量数据读取失败，请稍后重试。")).toBeVisible();
  });
});
