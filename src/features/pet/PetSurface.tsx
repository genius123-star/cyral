import { useEffect, useMemo, useRef, useState, type CSSProperties, type MouseEvent } from "react";
import type { ThinkingVariant } from "./thinking-animation";
import { IDLE_ACTION, IDLE_FALLBACK, type PetActionSpec } from "./pet-manifest";

const CHARACTER_HIT_AREA =
  "polygon(50% 9%, 64% 13%, 73% 25%, 76% 48%, 68% 60%, 100% 70%, 100% 85%, 68% 82%, 62% 94%, 38% 94%, 32% 82%, 0% 85%, 0% 70%, 32% 60%, 24% 48%, 27% 25%, 36% 13%)";
const COMPACT_RENDER_SIZE = 77;

interface PetSurfaceProps {
  locked: boolean;
  welcomeMessage?: string;
  thinkingVariant?: ThinkingVariant;
  /** Rust 侧 pet-motion 事件：normal-idle | walking | compact | restoring。 */
  motion?: "normal-idle" | "walking" | "compacting" | "compact" | "restoring";
  idleSpec?: PetActionSpec;
  idleSpecs?: PetActionSpec[];
  walkSpec?: PetActionSpec;
  walkSpecs?: PetActionSpec[];
  compactSpec?: PetActionSpec;
  enterSpec?: PetActionSpec;
  exitSpec?: PetActionSpec;
  ambientSpecs?: PetActionSpec[];
  /** 宠物窗口当前贴近的屏幕边缘（如 ["left","bottom"]），非边缘时不允许探头动画。 */
  peekEdges?: string[];
  random?: () => number;
  onToggleLock: () => void;
  onHide: () => void;
  onOpenPanel?: () => void;
  onOpenComposer?: () => void;
  onStartDragging?: () => void;
  onPointerActive?: () => void;
  onRestoreFromCompact?: () => void;
  reminder?: { id: number; message: string };
  onCompleteReminder?: (id: number) => void;
  onSnoozeReminder?: (id: number) => void;
  onStopCompanion?: () => void;
}

/** 通用帧条渲染：JS 定时器逐帧驱动（不依赖 CSS steps 的步进语义）；缺图由上层回退眨眼待机。 */
function ActionStrip({ spec, label }: { spec: PetActionSpec; label: string }) {
  const frames = Math.max(1, spec.frames);
  const frameMs = Math.max(1, Math.round(spec.durationMs / frames));
  const [frame, setFrame] = useState(0);

  useEffect(() => {
    setFrame(0);
    const id = window.setInterval(() => {
      setFrame((current) =>
        spec.loop ? (current + 1) % frames : Math.min(current + 1, frames - 1),
      );
    }, frameMs);
    return () => window.clearInterval(id);
  }, [frameMs, frames, spec.loop, spec.strip]);

  const width = spec.frames * spec.frameWidth;
  const style = {
    width,
    height: spec.frameHeight,
    top: spec.offsetY ?? 0,
    backgroundImage: `url("${spec.strip}")`,
    backgroundSize: `${width}px ${spec.frameHeight}px`,
    backgroundPosition: `${-frame * spec.frameWidth}px 0`,
    transform: spec.flipX ? "scaleX(-1)" : undefined,
  } as CSSProperties;
  return <div className="pet-action-strip" role="img" aria-label={label} style={style} />;
}

export function PetSurface({
  locked,
  welcomeMessage,
  thinkingVariant,
  motion,
  idleSpec,
  idleSpecs,
  walkSpec,
  walkSpecs,
  compactSpec,
  enterSpec,
  exitSpec,
  ambientSpecs = [],
  peekEdges = [],
  random = Math.random,
  onToggleLock,
  onHide,
  onOpenPanel,
  onOpenComposer,
  onStartDragging,
  onPointerActive,
  onRestoreFromCompact,
  reminder,
  onCompleteReminder,
  onSnoozeReminder,
  onStopCompanion,
}: PetSurfaceProps) {
  const pointerStart = useRef<{ x: number; y: number } | null>(null);
  const suppressClick = useRef(false);
  const previousMotion = useRef(motion);
  const availableIdleSpecs = useMemo(
    () => idleSpecs ?? [idleSpec ?? IDLE_FALLBACK],
    [idleSpec, idleSpecs],
  );
  const availableWalkSpecs = useMemo(
    () => walkSpecs ?? (walkSpec ? [walkSpec] : []),
    [walkSpec, walkSpecs],
  );
  const firstIdleSpec =
    availableIdleSpecs.find((spec) => spec.action === IDLE_ACTION)
      ?? availableIdleSpecs[0]
      ?? IDLE_FALLBACK;
  // 边缘探头只允许贴近对应屏幕边缘时播放（peek-left ↔ left，其余同理）。
  const allowedAmbientSpecs = useMemo(
    () =>
      ambientSpecs.filter((spec) => {
        if (!spec.action.startsWith("peek-")) return false;
        return peekEdges.includes(spec.action.slice("peek-".length));
      }),
    [ambientSpecs, peekEdges],
  );
  const pickSpec = (specs: PetActionSpec[]) => {
    if (specs.length === 1) return specs[0];
    const index = Math.floor(random() * specs.length);
    return specs[Math.min(index, specs.length - 1)];
  };
  const [selectedIdleSpec, setSelectedIdleSpec] = useState<PetActionSpec>(firstIdleSpec);
  const [selectedWalkSpec, setSelectedWalkSpec] = useState<PetActionSpec | undefined>(() =>
    motion === "walking" && availableWalkSpecs.length > 0
      ? pickSpec(availableWalkSpecs)
      : undefined,
  );
  const [selectedAmbientSpec, setSelectedAmbientSpec] = useState<PetActionSpec | undefined>(() =>
    motion === "normal-idle" && allowedAmbientSpecs.length > 0 && random() < 0.25
      ? pickSpec(allowedAmbientSpecs)
      : undefined,
  );
  const [compactFrame, setCompactFrame] = useState(0);

  useEffect(() => {
    const startedWalking = motion === "walking" && previousMotion.current !== "walking";
    const startedIdling = motion === "normal-idle" && previousMotion.current !== "normal-idle";
    const needsInitialSelection = motion === "walking" && !selectedWalkSpec;
    if ((startedWalking || needsInitialSelection) && availableWalkSpecs.length > 0) {
      setSelectedWalkSpec(pickSpec(availableWalkSpecs));
    }
    if (startedIdling && availableIdleSpecs.length > 0) {
      setSelectedIdleSpec(firstIdleSpec);
      setSelectedAmbientSpec(
        allowedAmbientSpecs.length > 0 && random() < 0.25
          ? pickSpec(allowedAmbientSpecs)
          : undefined,
      );
    }
    previousMotion.current = motion;
  }, [allowedAmbientSpecs, availableIdleSpecs, availableWalkSpecs, motion, random, selectedWalkSpec]);

  // manifest 异步加载后，用真实动作定义替换启动时的眨眼待机回退项。
  useEffect(() => {
    if (motion !== "normal-idle" || selectedAmbientSpec || thinkingVariant) return;
    const loadedSpec =
      availableIdleSpecs.find((spec) => spec.action === selectedIdleSpec.action) ?? firstIdleSpec;
    if (loadedSpec !== selectedIdleSpec) {
      setSelectedIdleSpec(loadedSpec);
    }
  }, [availableIdleSpecs, firstIdleSpec, motion, selectedAmbientSpec, selectedIdleSpec, thinkingVariant]);

  useEffect(() => {
    if (!selectedAmbientSpec || motion !== "normal-idle") return;
    const timer = setTimeout(() => setSelectedAmbientSpec(undefined), selectedAmbientSpec.durationMs);
    return () => clearTimeout(timer);
  }, [motion, selectedAmbientSpec]);

  // 顺序播放：非循环动作播完后按 manifest 的 nextAction 接续。
  useEffect(() => {
    if (
      motion !== "normal-idle" ||
      thinkingVariant ||
      selectedAmbientSpec ||
      !selectedIdleSpec ||
      selectedIdleSpec.loop ||
      !selectedIdleSpec.nextAction
    ) {
      return;
    }
    const timer = setTimeout(() => {
      setSelectedIdleSpec((current) => {
        const next = availableIdleSpecs.find((spec) => spec.action === current.nextAction);
        return next ?? firstIdleSpec;
      });
    }, Math.max(1, selectedIdleSpec.durationMs));
    return () => clearTimeout(timer);
  }, [availableIdleSpecs, firstIdleSpec, motion, selectedAmbientSpec, selectedIdleSpec, thinkingVariant]);

  // 悬浮球：JS 逐帧播放（4 帧 64×64 渲染为 77px）。
  useEffect(() => {
    if (!compactSpec) return;
    const frames = Math.max(1, compactSpec.frames);
    const frameMs = Math.max(1, Math.round(compactSpec.durationMs / frames));
    setCompactFrame(0);
    const id = window.setInterval(() => {
      setCompactFrame((current) =>
        compactSpec.loop ? (current + 1) % frames : Math.min(current + 1, frames - 1),
      );
    }, frameMs);
    return () => window.clearInterval(id);
  }, [compactSpec]);

  const rememberPointer = (event: MouseEvent<HTMLButtonElement>) => {
    if (!locked && event.button === 0) {
      pointerStart.current = { x: event.clientX, y: event.clientY };
    }
  };

  const dragAfterMovement = (event: MouseEvent<HTMLButtonElement>) => {
    if (locked || event.buttons !== 1 || pointerStart.current === null) return;

    const distance = Math.hypot(
      event.clientX - pointerStart.current.x,
      event.clientY - pointerStart.current.y,
    );
    if (distance < 5) return;

    pointerStart.current = null;
    suppressClick.current = true;
    onStartDragging?.();
  };

  if (motion === "compact") {
    return (
      <main className="pet-surface compact">
        <button
          type="button"
          className="compact-ball"
          aria-label="展开希莉尔"
          style={{ width: COMPACT_RENDER_SIZE, height: COMPACT_RENDER_SIZE, pointerEvents: "auto" }}
          onMouseDown={(event) => {
            rememberPointer(event);
            onPointerActive?.();
          }}
          onMouseMove={dragAfterMovement}
          onMouseUp={() => {
            pointerStart.current = null;
          }}
          onClick={() => {
            if (suppressClick.current) {
              suppressClick.current = false;
              return;
            }
            onRestoreFromCompact?.();
          }}
        >
          {compactSpec ? (
            <span
              className="compact-sprite"
              style={{
                width: COMPACT_RENDER_SIZE,
                height: COMPACT_RENDER_SIZE,
                backgroundImage: `url("${compactSpec.strip}")`,
                backgroundSize: `${compactSpec.frames * COMPACT_RENDER_SIZE}px ${COMPACT_RENDER_SIZE}px`,
                backgroundPosition: `${-compactFrame * COMPACT_RENDER_SIZE}px 0`,
              } as CSSProperties}
            />
          ) : (
            <span className="compact-fallback" aria-hidden="true" />
          )}
        </button>
      </main>
    );
  }

  const transitionSpec = motion === "compacting" ? exitSpec : motion === "restoring" ? enterSpec : undefined;
  const activeSpec = transitionSpec
    ?? (motion === "walking" && selectedWalkSpec
      ? selectedWalkSpec
      : selectedAmbientSpec ?? selectedIdleSpec);
  const activeLabel = motion === "compacting"
    ? "希莉尔正在隐入悬浮球"
    : motion === "restoring"
      ? "希莉尔正在从悬浮球出现"
      : selectedAmbientSpec && motion === "normal-idle"
        ? "希莉尔正从屏幕边缘探出"
      : activeSpec.action.startsWith("walk")
        ? "希莉尔正在移动"
        : "希莉尔正在待机";

  return (
    <main className="pet-surface">
      {(reminder || welcomeMessage) && (
        <div className="welcome-bubble-shell">
          <div
            className="welcome-bubble"
            role="status"
            aria-label="希莉尔说"
            style={{ borderRadius: "14px" }}
          >
            {reminder?.message ?? welcomeMessage}
          </div>
          {reminder && (
            <div className="reminder-actions">
              <button type="button" aria-label="十分钟后再次提醒" onClick={() => onSnoozeReminder?.(reminder.id)}>10 分钟后</button>
              <button type="button" aria-label="标记待办已完成" onClick={() => onCompleteReminder?.(reminder.id)}>做完了</button>
            </div>
          )}
        </div>
      )}
      <button
        className="hide-pet"
        type="button"
        aria-label="隐藏希莉尔到系统托盘"
        onClick={onHide}
      >
        ×
      </button>
      <div className="pet-viewport">
        {thinkingVariant ? (
          <ActionStrip
            spec={{
              action: "thinking",
              strip: `/assets/characters/cyral/thinking-${thinkingVariant.toLowerCase()}.png`,
              frames: 6,
              frameWidth: 180,
              frameHeight: 226,
              durationMs: 2400,
              loop: true,
            }}
            label="希莉尔正在思考"
          />
        ) : (
          <ActionStrip
            spec={activeSpec}
            label={activeLabel}
          />
        )}
      </div>
      <button
        className="pet-hit-area"
        style={{ width: "180px", clipPath: CHARACTER_HIT_AREA }}
        type="button"
        aria-label="和希莉尔对话"
        onMouseDown={(event) => {
          rememberPointer(event);
          onPointerActive?.();
        }}
        onMouseMove={dragAfterMovement}
        onMouseUp={() => {
          pointerStart.current = null;
        }}
        onClick={() => {
          if (suppressClick.current) {
            suppressClick.current = false;
            return;
          }
          onOpenComposer?.();
        }}
        onContextMenu={(event) => {
          event.preventDefault();
          onOpenPanel?.();
        }}
        onDoubleClick={() => onStopCompanion?.()}
      />
      <button
        className="position-lock"
        type="button"
        aria-label={locked ? "解除希莉尔的位置锁定" : "锁定希莉尔的位置"}
        aria-pressed={locked}
        onClick={(event) => {
          event.stopPropagation();
          onToggleLock();
        }}
      >
        {locked ? "已锁定" : "锁定"}
      </button>
    </main>
  );
}
