import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

// 测试环境无 Tauri 运行时，mock 事件监听为无害空实现。
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => undefined),
}));

import { ModeStatus } from "../src/features/companion/ModeStatus";

describe("ModeStatus", () => {
  it("shows idle mode active when neither companion nor work is on", async () => {
    render(<ModeStatus onLoad={vi.fn().mockResolvedValue({ companion: false, work: false })} />);

    expect(await screen.findByText("普通模式")).toBeVisible();
    expect(screen.getAllByText("已开启").length).toBe(1);
    expect(screen.getAllByText("未开启").length).toBe(2);
  });

  it("shows companion mode active and idle exited when companion is on", async () => {
    render(<ModeStatus onLoad={vi.fn().mockResolvedValue({ companion: true, work: false })} />);

    expect(await screen.findByText("陪伴模式")).toBeVisible();
    expect(screen.getAllByText("已开启").length).toBe(1);
    expect(screen.getByText(/已退出普通模式/)).toBeVisible();
  });

  it("shows work mode as planned and not yet available", async () => {
    render(<ModeStatus onLoad={vi.fn().mockResolvedValue({ companion: false, work: false })} />);

    expect(await screen.findByText("工作模式")).toBeVisible();
    expect(screen.getByText(/规划中/)).toBeVisible();
  });

  it("toggles companion from the panel and refreshes state", async () => {
    const onToggle = vi.fn().mockResolvedValue(undefined);
    const onLoad = vi.fn().mockResolvedValue({ companion: false, work: false });
    render(<ModeStatus onLoad={onLoad} onToggleCompanion={onToggle} />);

    const enable = await screen.findByRole("button", { name: "开启陪伴" });
    enable.click();
    expect(onToggle).toHaveBeenCalledWith(true);
    // 刷新后陪伴变为开启。
    onLoad.mockResolvedValue({ companion: true, work: false });
    await screen.findByText("停止陪伴");
  });

  it("shows the block reason when enabling is denied", async () => {
    const onToggle = vi.fn().mockResolvedValue("开启陪伴模式需要屏幕观察授权。");
    render(
      <ModeStatus
        onLoad={vi.fn().mockResolvedValue({ companion: false, work: false })}
        onToggleCompanion={onToggle}
      />,
    );

    const enable = await screen.findByRole("button", { name: "开启陪伴" });
    enable.click();
    expect(await screen.findByRole("alert")).toHaveTextContent("需要屏幕观察授权");
  });
});
