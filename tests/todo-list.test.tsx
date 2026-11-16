import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { TodoList } from "../src/features/organizer/TodoList";

const draft = {
  id: 3,
  title: "提交 PPT",
  deadlineLocal: "2026-08-12T16:00",
  remindBeforeMinutes: 120,
  sourceText: "今天下午四点要交 PPT",
  status: "draft" as const,
  createdAt: 1_800_000_000,
};

describe("TodoList", () => {
  it("shows an editable confirmation card for a draft", async () => {
    render(<TodoList onLoad={vi.fn().mockResolvedValue([draft])} />);

    expect(await screen.findByDisplayValue("提交 PPT")).toBeVisible();
    expect(screen.getByLabelText("截止时间")).toHaveValue("2026-08-12T16:00");
    expect(screen.getByText(/今天下午四点要交 PPT/)).toBeVisible();
    expect(screen.getByText("提醒：截止前 2 小时、30 分钟、5 分钟各提醒一次")).toBeVisible();
  });

  it("creates the todo only after explicit confirmation", async () => {
    const onConfirm = vi.fn().mockResolvedValue(undefined);
    render(<TodoList onLoad={vi.fn().mockResolvedValue([draft])} onConfirm={onConfirm} />);
    await screen.findByDisplayValue("提交 PPT");
    fireEvent.click(screen.getByRole("button", { name: "确认创建待办" }));

    await waitFor(() => expect(onConfirm).toHaveBeenCalledWith({
      id: 3,
      title: "提交 PPT",
      deadlineLocal: "2026-08-12T16:00",
      remindBeforeMinutes: 120,
    }));
  });

  it("creates a todo directly from the panel form", async () => {
    const onCreate = vi.fn().mockResolvedValue(undefined);
    render(<TodoList onLoad={vi.fn().mockResolvedValue([])} onCreate={onCreate} />);
    fireEvent.click(screen.getByRole("button", { name: "新建待办" }));
    fireEvent.change(screen.getByLabelText("新建待办事项"), { target: { value: "提交周报" } });
    fireEvent.change(screen.getByLabelText("新建待办截止时间"), { target: { value: "2026-08-19T18:00" } });
    fireEvent.click(screen.getByRole("button", { name: "添加待办" }));

    await waitFor(() => expect(onCreate).toHaveBeenCalledWith("提交周报", "2026-08-19T18:00"));
  });

  it("shows completed todos with strikethrough and supports search", async () => {
    const completed = { ...draft, id: 7, title: "提交周报", status: "completed" as const };
    render(<TodoList onLoad={vi.fn().mockResolvedValue([completed])} />);
    fireEvent.click(screen.getByRole("tab", { name: /已完成/ }));

    expect(await screen.findByText("提交周报")).toBeVisible();
    expect(screen.getByText("提交周报").tagName).toBe("S");

    fireEvent.change(screen.getByLabelText("搜索已完成事项"), { target: { value: "不存在的" } });
    expect(screen.getByText("没有匹配的已完成事项。")).toBeVisible();
  });

  it("marks an active todo as complete from the panel", async () => {
    const onComplete = vi.fn().mockResolvedValue(undefined);
    const active = { ...draft, id: 9, status: "active" as const };
    render(<TodoList onLoad={vi.fn().mockResolvedValue([active])} onComplete={onComplete} />);
    await screen.findByText("提交 PPT");
    expect(screen.getByText("截止前 2 小时、30 分钟、5 分钟提醒")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "完成待办 提交 PPT" }));

    await waitFor(() => expect(onComplete).toHaveBeenCalledWith(9));
  });

  it("groups active todos by overdue, today, upcoming and later", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-08-20T09:00:00"));
    try {
      const at = (dayOffset: number, hour: number) => {
        const date = new Date();
        date.setDate(date.getDate() + dayOffset);
        date.setHours(hour, 0, 0, 0);
        const offset = date.getTimezoneOffset();
        return new Date(date.getTime() - offset * 60_000).toISOString().slice(0, 16);
      };
      const todos = [
        { ...draft, id: 10, title: "补交周报", deadlineLocal: at(-1, 10), status: "active" as const },
        { ...draft, id: 11, title: "今天评审", deadlineLocal: at(0, 23), status: "active" as const },
        { ...draft, id: 12, title: "明天验收", deadlineLocal: at(1, 10), status: "active" as const },
        { ...draft, id: 13, title: "下周整理", deadlineLocal: at(10, 10), status: "active" as const },
      ];

      render(<TodoList onLoad={vi.fn().mockResolvedValue(todos)} />);

      await act(async () => { await Promise.resolve(); });
      expect(screen.getByRole("heading", { name: "已逾期" })).toBeVisible();
      expect(screen.getByRole("heading", { name: "今天" })).toBeVisible();
      expect(screen.getByRole("heading", { name: "即将到期" })).toBeVisible();
      expect(screen.getByRole("heading", { name: "以后" })).toBeVisible();
    } finally {
      vi.useRealTimers();
    }
  });
});
