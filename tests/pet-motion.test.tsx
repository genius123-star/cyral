import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { PetSurface } from "../src/features/pet/PetSurface";
import type { PetActionSpec } from "../src/features/pet/pet-manifest";

const idleSpec: PetActionSpec = {
  action: "idle-a",
  strip: "/assets/characters/cyral/idle-row-v1.png",
  frames: 6,
  frameWidth: 180,
  frameHeight: 452,
  durationMs: 2400,
  loop: true,
  offsetY: -118,
};

const walkSpec: PetActionSpec = {
  action: "walk",
  strip: "/assets/characters/cyral/walk-row.png",
  frames: 6,
  frameWidth: 180,
  frameHeight: 226,
  durationMs: 2400,
  loop: true,
};

describe("PetSurface motion", () => {
  it("renders the walk strip while moving", () => {
    render(
      <PetSurface
        locked={false}
        motion="walking"
        idleSpec={idleSpec}
        walkSpec={walkSpec}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    expect(screen.getByRole("img", { name: "希莉尔正在移动" })).toBeVisible();
  });

  it("chooses a walk variant once for each movement segment", () => {
    const walkBSpec: PetActionSpec = {
      ...walkSpec,
      action: "walk-b",
      strip: "/assets/characters/cyral/walk-b.png",
    };
    const random = vi.fn().mockReturnValueOnce(0.1).mockReturnValueOnce(0.9);
    const props = {
      locked: false,
      idleSpec,
      walkSpecs: [walkSpec, walkBSpec],
      random,
      onToggleLock: vi.fn(),
      onHide: vi.fn(),
    };

    const { rerender } = render(<PetSurface {...props} motion="walking" />);
    expect(screen.getByRole("img", { name: "希莉尔正在移动" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/walk-row.png")',
    });

    rerender(<PetSurface {...props} motion="normal-idle" />);
    rerender(<PetSurface {...props} motion="walking" />);

    expect(screen.getByRole("img", { name: "希莉尔正在移动" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/walk-b.png")',
    });
    expect(random).toHaveBeenCalledTimes(2);
  });

  it("animates both frames of a two-frame walk strip", () => {
    vi.useFakeTimers();
    const twoFrame: PetActionSpec = {
      action: "walk-a",
      strip: "/assets/characters/cyral/walk-1-strip.png",
      frames: 2,
      frameWidth: 180,
      frameHeight: 226,
      durationMs: 700,
      loop: true,
    };
    render(
      <PetSurface
        locked={false}
        motion="walking"
        idleSpec={idleSpec}
        walkSpec={twoFrame}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    const strip = screen.getByRole("img", { name: "希莉尔正在移动" });
    expect(strip.style.backgroundPosition).toBe("0px 0px");
    act(() => vi.advanceTimersByTime(350));
    expect(strip.style.backgroundPosition).toBe("-180px 0px");
    act(() => vi.advanceTimersByTime(350));
    expect(strip.style.backgroundPosition).toBe("0px 0px");
    vi.useRealTimers();
  });

  it("starts each idle segment with the first idle animation", () => {
    const idleBSpec: PetActionSpec = {
      action: "idle-b",
      strip: "/assets/characters/cyral/idle-b.png",
      frames: 4,
      frameWidth: 180,
      frameHeight: 226,
      durationMs: 1800,
      loop: true,
    };
    const random = vi.fn().mockReturnValue(0.9);
    const props = {
      locked: false,
      idleSpecs: [idleSpec, idleBSpec],
      random,
      onToggleLock: vi.fn(),
      onHide: vi.fn(),
    };

    const { rerender } = render(<PetSurface {...props} motion="normal-idle" />);
    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/idle-row-v1.png")',
    });

    rerender(<PetSurface {...props} motion="walking" />);
    rerender(<PetSurface {...props} motion="normal-idle" />);

    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/idle-row-v1.png")',
    });
    expect(random).not.toHaveBeenCalled();
  });

  it("falls back to idle animation when walk asset is unavailable", () => {
    render(
      <PetSurface
        locked={false}
        motion="walking"
        idleSpec={idleSpec}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toBeVisible();
  });

  it("shows the compact ball and restores the normal form on click", () => {
    const onRestoreFromCompact = vi.fn();
    const compactSpec: PetActionSpec = {
      action: "compact",
      strip: "/assets/characters/cyral/compact.png",
      frames: 1,
      frameWidth: 64,
      frameHeight: 64,
      durationMs: 1000,
      loop: true,
    };
    render(
      <PetSurface
        locked={false}
        motion="compact"
        compactSpec={compactSpec}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
        onRestoreFromCompact={onRestoreFromCompact}
      />,
    );

    const ball = screen.getByRole("button", { name: "展开希莉尔" });
    expect(ball.querySelector(".compact-sprite")).toBeInTheDocument();
    fireEvent.click(ball);
    expect(onRestoreFromCompact).toHaveBeenCalledOnce();
  });

  it("animates every frame in a multi-frame compact strip", () => {
    vi.useFakeTimers();
    const compactSpec: PetActionSpec = {
      action: "compact",
      strip: "/assets/characters/cyral/compact.png",
      frames: 4,
      frameWidth: 64,
      frameHeight: 64,
      durationMs: 1600,
      loop: true,
    };

    const { container } = render(
      <PetSurface
        locked={false}
        motion="compact"
        compactSpec={compactSpec}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    const sprite = container.querySelector<HTMLElement>(".compact-sprite");
    expect(sprite).toHaveStyle({ width: "77px", height: "77px", backgroundSize: "308px 77px" });
    expect(sprite?.style.backgroundPosition).toBe("0px 0px");
    act(() => vi.advanceTimersByTime(400));
    expect(sprite?.style.backgroundPosition).toBe("-77px 0px");
    act(() => vi.advanceTimersByTime(400));
    expect(sprite?.style.backgroundPosition).toBe("-154px 0px");
    act(() => vi.advanceTimersByTime(400));
    expect(sprite?.style.backgroundPosition).toBe("-231px 0px");
    act(() => vi.advanceTimersByTime(400));
    expect(sprite?.style.backgroundPosition).toBe("0px 0px");
    vi.useRealTimers();
  });

  it("keeps the compact ball clickable and draggable", () => {
    const onRestoreFromCompact = vi.fn();
    const onStartDragging = vi.fn();
    render(
      <PetSurface
        locked={false}
        motion="compact"
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
        onPointerActive={vi.fn()}
        onRestoreFromCompact={onRestoreFromCompact}
        onStartDragging={onStartDragging}
      />,
    );

    const ball = screen.getByRole("button", { name: "展开希莉尔" });
    expect(ball).toHaveStyle({ width: "77px", height: "77px", pointerEvents: "auto" });
    fireEvent.mouseDown(ball, { button: 0, clientX: 20, clientY: 20 });
    fireEvent.mouseMove(ball, { buttons: 1, clientX: 30, clientY: 30 });
    fireEvent.click(ball);
    expect(onStartDragging).toHaveBeenCalledOnce();
    expect(onRestoreFromCompact).not.toHaveBeenCalled();
  });

  it("keeps the compact ball clickable even without a compact asset", () => {
    const { container } = render(
      <PetSurface
        locked={false}
        motion="compact"
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );
    expect(container.querySelector(".compact-fallback")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "展开希莉尔" })).toBeVisible();
  });

  it("plays the exit transition before the compact ball", () => {
    const exitSpec: PetActionSpec = {
      action: "exit",
      strip: "/assets/characters/cyral/exit-row.png",
      frames: 6,
      frameWidth: 180,
      frameHeight: 226,
      durationMs: 1800,
      loop: false,
    };

    render(
      <PetSurface
        locked={false}
        motion="compacting"
        exitSpec={exitSpec}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    expect(screen.getByRole("img", { name: "希莉尔正在隐入悬浮球" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/exit-row.png")',
    });
  });

  it("plays the enter transition while restoring from the compact ball", () => {
    const enterSpec: PetActionSpec = {
      action: "enter",
      strip: "/assets/characters/cyral/enter-row.png",
      frames: 6,
      frameWidth: 180,
      frameHeight: 226,
      durationMs: 1800,
      loop: false,
    };

    render(
      <PetSurface
        locked={false}
        motion="restoring"
        enterSpec={enterSpec}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    expect(screen.getByRole("img", { name: "希莉尔正在从悬浮球出现" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/enter-row.png")',
    });
  });

  it("plays a screen-edge peek once and returns to the normal idle", () => {
    vi.useFakeTimers();
    const peekSpec: PetActionSpec = {
      action: "peek-left",
      strip: "/assets/characters/cyral/peek-left.png",
      frames: 3,
      frameWidth: 180,
      frameHeight: 226,
      durationMs: 900,
      loop: false,
    };

    render(
      <PetSurface
        locked={false}
        motion="normal-idle"
        idleSpec={idleSpec}
        ambientSpecs={[peekSpec]}
        peekEdges={["left"]}
        random={() => 0}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    expect(screen.getByRole("img", { name: "希莉尔正从屏幕边缘探出" })).toBeVisible();
    act(() => vi.advanceTimersByTime(900));
    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toBeVisible();
    vi.useRealTimers();
  });

  it("replaces the startup fallback with the loaded blink-only idle", () => {
    vi.useFakeTimers();
    const blink: PetActionSpec = {
      action: "idle-blink",
      strip: "/assets/characters/cyral/idle-blink-row-v1.png",
      frames: 4,
      frameWidth: 180,
      frameHeight: 226,
      durationMs: 1000,
      loop: true,
    };
    const { rerender } = render(
      <PetSurface
        locked={false}
        motion="normal-idle"
        idleSpec={idleSpec}
        random={() => 0}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/idle-row-v1.png")',
    });
    rerender(
      <PetSurface
        locked={false}
        motion="normal-idle"
        idleSpecs={[blink]}
        random={() => 0}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );
    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/idle-blink-row-v1.png")',
    });
    act(() => vi.advanceTimersByTime(5000));
    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/idle-blink-row-v1.png")',
    });
    vi.useRealTimers();
  });

  it("keeps the blink animation as the sole idle style", () => {
    vi.useFakeTimers();
    const blink: PetActionSpec = {
      action: "idle-blink",
      strip: "/assets/characters/cyral/idle-blink-runtime.png",
      frames: 4,
      frameWidth: 180,
      frameHeight: 226,
      durationMs: 1000,
      loop: true,
    };
    render(
      <PetSurface
        locked={false}
        motion="normal-idle"
        idleSpecs={[blink]}
        random={() => 0}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    // 初始选中 idle-a（索引 0）
    act(() => vi.advanceTimersByTime(5000));
    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/idle-blink-runtime.png")',
    });
    vi.useRealTimers();
  });

  it("never plays a screen-edge peek when the pet is away from the edge", () => {
    vi.useFakeTimers();
    const peekSpec: PetActionSpec = {
      action: "peek-left",
      strip: "/assets/characters/cyral/peek-left.png",
      frames: 3,
      frameWidth: 180,
      frameHeight: 226,
      durationMs: 900,
      loop: false,
    };

    render(
      <PetSurface
        locked={false}
        motion="normal-idle"
        idleSpec={idleSpec}
        ambientSpecs={[peekSpec]}
        peekEdges={[]}
        random={() => 0}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    // 不在边缘：即使 random=0（本会选中探头）也只显示待机。
    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toBeVisible();
    act(() => vi.advanceTimersByTime(900));
    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toBeVisible();
    vi.useRealTimers();
  });

  it("does not switch away from the only configured idle animation", () => {
    vi.useFakeTimers();
    const blink = { ...idleSpec, action: "idle-blink", strip: "/assets/characters/cyral/idle-blink-runtime.png" };
    render(
      <PetSurface
        locked={false}
        motion="normal-idle"
        idleSpecs={[blink]}
        random={() => 0}
        onToggleLock={vi.fn()}
        onHide={vi.fn()}
      />,
    );

    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/idle-blink-runtime.png")',
    });
    act(() => vi.advanceTimersByTime(5000));
    expect(screen.getByRole("img", { name: "希莉尔正在待机" })).toHaveStyle({
      backgroundImage: 'url("/assets/characters/cyral/idle-blink-runtime.png")',
    });
    vi.useRealTimers();
  });
});
