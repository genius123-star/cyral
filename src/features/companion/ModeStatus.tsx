import { useCallback, useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ModeState {
  companion: boolean;
  work: boolean;
  nextProactiveAt?: number | null;
  proactiveRetryCount?: number;
  lastProactiveResult?: string | null;
}

function proactiveNote(mode: ModeState): string {
  if (!mode.companion) return "未开启。";
  const result = mode.lastProactiveResult ?? "正在等待下一次主动搭话。";
  if (!mode.nextProactiveAt) return result;
  const next = new Date(mode.nextProactiveAt * 1000).toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
  });
  const retry = mode.proactiveRetryCount ? `，当前第 ${mode.proactiveRetryCount} 次重试` : "";
  return `${result}；下一次随机搭话约在 ${next}${retry}。`;
}

interface ModeStatusProps {
  onLoad?: () => Promise<ModeState>;
  /** 通过面板切换陪伴模式；返回被阻止原因则保持原状态，undefined 表示成功。 */
  onToggleCompanion?: (enabled: boolean) => Promise<string | undefined>;
}

interface ModeCardProps {
  name: string;
  active: boolean;
  note: string;
  action?: {
    label: string;
    onClick: () => void;
    disabled?: boolean;
  };
}

function ModeCard({ name, active, note, action }: ModeCardProps) {
  return (
    <div className={`mode-card ${active ? "active" : ""}`}>
      <div className="mode-card-head">
        <span className="mode-dot" aria-hidden="true" />
        <strong>{name}</strong>
        <span className={`mode-badge ${active ? "on" : "off"}`}>
          {active ? "已开启" : "未开启"}
        </span>
      </div>
      <p className="mode-note">{note}</p>
      {action && (
        <button
          type="button"
          className="mode-toggle"
          onClick={action.onClick}
          disabled={action.disabled}
        >
          {action.label}
        </button>
      )}
    </div>
  );
}

export function ModeStatus({ onLoad, onToggleCompanion }: ModeStatusProps) {
  const [mode, setMode] = useState<ModeState>({ companion: false, work: false });
  const [error, setError] = useState<string>();

  const load = useCallback(async () => {
    if (!onLoad) return;
    try {
      setMode(await onLoad());
      setError(undefined);
    } catch {
      // 读取失败保持当前状态，静默。
    }
  }, [onLoad]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    // 面板打开期间通过 /陪伴模式、/停止陪伴 命令切换时实时刷新。
    void listen("mode-changed", () => {
      void load();
    }).then((dispose) => {
      unlisten = dispose;
    });
    return () => {
      unlisten?.();
    };
  }, [load]);

  const toggleCompanion = async (enabled: boolean) => {
    if (!onToggleCompanion) return;
    const blocked = await onToggleCompanion(enabled);
    if (blocked) {
      setError(blocked);
    } else {
      setError(undefined);
      await load();
    }
  };

  const idle = !mode.companion && !mode.work;

  return (
    <div className="mode-status">
      {error && <div className="mode-error" role="alert">{error}</div>}
      <ModeCard
        name="普通模式"
        active={idle}
        note={idle ? "当前仅支持对话交互。开启陪伴或工作模式后自动退出普通模式。" : "已退出普通模式（陪伴/工作模式运行中）。"}
      />
      <ModeCard
        name="陪伴模式"
        active={mode.companion}
        note={proactiveNote(mode)}
        action={{
          label: mode.companion ? "停止陪伴" : "开启陪伴",
          onClick: () => void toggleCompanion(!mode.companion),
        }}
      />
      <ModeCard
        name="工作模式"
        active={mode.work}
        note="规划中，尚未开放。未来将以 Agent 身份安装 Skills 并执行简单工作。"
      />
    </div>
  );
}
