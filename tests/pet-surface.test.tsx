import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { PetSurface } from "../src/features/pet/PetSurface";

describe("PetSurface", () => {
  it("shows the startup greeting beside the character", () => {
    render(
      <PetSurface
        locked={false}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
        welcomeMessage="欢迎回来。休息了大约 9 小时，现在感觉怎么样？"
      />,
    );

    expect(screen.getByRole("status")).toHaveTextContent(
      "欢迎回来。休息了大约 9 小时，现在感觉怎么样？",
    );
    expect(screen.getByRole("status")).toHaveAttribute("aria-label", "希莉尔说");
    expect(screen.getByRole("status").style.maxHeight).toBe("");
    expect(screen.getByRole("status").style.overflowY).toBe("");
    expect(screen.getByRole("status")).toHaveStyle({ borderRadius: "14px" });
  });

  it("offers completion and ten-minute snooze actions for a todo reminder", () => {
    const onCompleteReminder = vi.fn();
    const onSnoozeReminder = vi.fn();
    render(
      <PetSurface
        locked={false}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
        reminder={{ id: 7, message: "提醒你一下：16:00 要提交 PPT。现在进展怎么样？" }}
        onCompleteReminder={onCompleteReminder}
        onSnoozeReminder={onSnoozeReminder}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "标记待办已完成" }));
    fireEvent.click(screen.getByRole("button", { name: "十分钟后再次提醒" }));

    expect(onCompleteReminder).toHaveBeenCalledWith(7);
    expect(onSnoozeReminder).toHaveBeenCalledWith(7);
  });

  it("renders the idle animation as an accessible desktop companion", () => {
    render(<PetSurface locked={false} onToggleLock={vi.fn()} onHide={vi.fn()} />);

    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toBeVisible();
  });

  it("renders the selected thinking animation instead of idle", () => {
    render(
      <PetSurface
        locked={false}
        thinkingVariant="C"
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    expect(screen.getByRole("img", { name: "希莉尔正在思考" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/thinking-c.png")',
    });
  });

  it("exposes a lock control without making transparent padding interactive", () => {
    const onToggleLock = vi.fn();
    render(<PetSurface locked={false} onToggleLock={onToggleLock} onHide={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "锁定希莉尔的位置" }));

    expect(onToggleLock).toHaveBeenCalledOnce();
  });

  it("opens the independent composer when the character is clicked", () => {
    const onOpenComposer = vi.fn();
    render(
      <PetSurface
        locked={false}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
        onOpenComposer={onOpenComposer}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "和希莉尔对话" }));

    expect(onOpenComposer).toHaveBeenCalledOnce();
  });

  it("starts dragging from the character after the pointer moves", () => {
    const onStartDragging = vi.fn();
    render(
      <PetSurface
        locked={false}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
        onStartDragging={onStartDragging}
      />,
    );

    const character = screen.getByRole("button", { name: "和希莉尔对话" });
    fireEvent.mouseDown(character, { button: 0, clientX: 80, clientY: 80 });
    fireEvent.mouseMove(character, { buttons: 1, clientX: 88, clientY: 84 });

    expect(onStartDragging).toHaveBeenCalledOnce();
  });

  it("does not drag the character while its position is locked", () => {
    const onStartDragging = vi.fn();
    render(
      <PetSurface
        locked
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
        onStartDragging={onStartDragging}
      />,
    );

    const character = screen.getByRole("button", { name: "和希莉尔对话" });
    fireEvent.mouseDown(character, { button: 0, clientX: 80, clientY: 80 });
    fireEvent.mouseMove(character, { buttons: 1, clientX: 92, clientY: 92 });

    expect(onStartDragging).not.toHaveBeenCalled();
  });

  it("hides the desktop window from a dedicated close control", () => {
    const onHide = vi.fn();
    render(<PetSurface locked={false} onToggleLock={vi.fn()} onHide={onHide} />);

    fireEvent.click(screen.getByRole("button", { name: "隐藏希莉尔到系统托盘" }));

    expect(onHide).toHaveBeenCalledOnce();
  });

  it("clips interaction to one sprite frame and the character silhouette", () => {
    render(<PetSurface locked={false} onToggleLock={vi.fn()} onHide={vi.fn()} />);

    const character = screen.getByRole("button", { name: "和希莉尔对话" });

    expect(character).toHaveStyle({ width: "180px" });
    expect(character.style.clipPath).toContain("polygon");
  });

  it("keeps the visible sprite outside the clipped interaction layer", () => {
    render(<PetSurface locked={false} onToggleLock={vi.fn()} onHide={vi.fn()} />);

    const sprite = screen.getByRole("img", { name: "希莉尔正在待机" });

    expect(sprite.closest("button")).toBeNull();
  });

  it("opens the control panel from the character context menu", () => {
    const onOpenPanel = vi.fn();
    render(
      <PetSurface
        locked={false}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
        onOpenPanel={onOpenPanel}
      />,
    );

    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    screen.getByRole("button", { name: "和希莉尔对话" }).dispatchEvent(event);

    expect(event.defaultPrevented).toBe(true);
    expect(onOpenPanel).toHaveBeenCalledOnce();
  });
});
