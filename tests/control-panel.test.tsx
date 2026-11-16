import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ControlPanel } from "../src/features/panel/ControlPanel";

describe("ControlPanel", () => {
  it("offers the agreed product sections", () => {
    render(<ControlPanel onClose={vi.fn()} />);

    const expected = ["模型配置", "对话记录", "功能模式", "隐私设置", "邮件助手", "活动用量", "待办提醒"];
    for (const name of expected) {
      expect(screen.getByRole("tab", { name })).toBeVisible();
    }
    expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual(expected);
  }, 15000);

  it("switches the visible section without closing the panel", () => {
    render(<ControlPanel onClose={vi.fn()} />);

    fireEvent.click(screen.getByRole("tab", { name: "活动用量" }));

    expect(screen.getByRole("tabpanel", { name: "活动用量" })).toBeVisible();
    expect(screen.getByRole("heading", { name: "活动用量" })).toBeVisible();
  });

  it("opens the todo review section when a conversation draft arrives", async () => {
    render(<ControlPanel onClose={vi.fn()} todoReviewId={3} />);

    expect(await screen.findByRole("tabpanel", { name: "待办提醒" })).toBeVisible();
    expect(screen.getByRole("heading", { name: "还没有待办" })).toBeVisible();
  });

  it("opens the dedicated email section when an email review arrives", async () => {
    const onLoadEmailDraft = vi.fn().mockResolvedValue({
      id: "email-review-1",
      accountId: 1,
      accountLabel: "lcr20041104a@gmail.com",
      recipients: ["recipient@example.com"],
      subject: "测试邮件",
      body: "正文",
      attachmentLabels: [],
      revision: 1,
      status: "waiting_for_user",
    });
    render(
      <ControlPanel
        onClose={vi.fn()}
        emailReviewId="email-review-1"
        onLoadEmailDraft={onLoadEmailDraft}
      />,
    );

    expect(await screen.findByRole("tabpanel", { name: "邮件助手" })).toBeVisible();
    expect(screen.getByRole("heading", { name: "邮件助手", level: 2 })).toBeVisible();
    await waitFor(() => expect(onLoadEmailDraft).toHaveBeenCalledWith("email-review-1"));
  });
  it("provides a dedicated close action", () => {
    const onClose = vi.fn();
    render(<ControlPanel onClose={onClose} />);

    fireEvent.click(screen.getByRole("button", { name: "关闭侧边面板" }));

    expect(onClose).toHaveBeenCalledOnce();
  });

  it("preserves the entered API key while switching between panel sections", () => {
    render(<ControlPanel onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: "模型配置" }));
    const firstKey = screen.getAllByLabelText("API Key")[0];
    fireEvent.change(firstKey, { target: { value: "sk-session-secret" } });

    fireEvent.click(screen.getByRole("tab", { name: "待办提醒" }));
    fireEvent.click(screen.getByRole("tab", { name: "模型配置" }));

    expect(screen.getAllByLabelText("API Key")[0]).toHaveValue("sk-session-secret");
  });

  it("removes the decorative empty orbit so content owns the available space", () => {
    const { container } = render(<ControlPanel onClose={vi.fn()} />);

    expect(container.querySelector(".panel-empty-orbit")).not.toBeInTheDocument();
  });

  it("separates conversation history from model configuration", () => {
    render(<ControlPanel onClose={vi.fn()} />);

    expect(screen.getByRole("tab", { name: "对话记录" })).toBeVisible();
    expect(screen.getByRole("tab", { name: "模型配置" })).toBeVisible();

    fireEvent.click(screen.getByRole("tab", { name: "模型配置" }));
  });

  it("loads local messages and renders them as two-sided chat bubbles", async () => {
    const onLoadMessages = vi.fn().mockResolvedValue([
      { role: "assistant", content: "我在这里。", createdAt: 1_800_000_000 },
      { role: "user", content: "今天一起工作吧。", createdAt: 1_800_000_060 },
    ]);
    const { container } = render(
      <ControlPanel onClose={vi.fn()} onLoadMessages={onLoadMessages} />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "对话记录" }));

    expect(await screen.findByText("我在这里。")).toBeVisible();
    expect(screen.getByText("今天一起工作吧。")).toBeVisible();
    expect(container.querySelector(".conversation-row.assistant")).toBeInTheDocument();
    expect(container.querySelector(".conversation-row.user")).toBeInTheDocument();
    await waitFor(() => expect(onLoadMessages).toHaveBeenCalledOnce());
  });

});
