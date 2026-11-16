import { useEffect, useState } from "react";

export interface CyralActivityEvent {
  id: number;
  eventType: string;
  source: string;
  appName: string;
  windowTitle?: string | null;
  summary: string;
  observedAt: number;
}

export interface ActivityTheme {
  identityKey: string;
  kind: "file" | "media";
  displayName: string;
  activeDays: number;
  totalActiveSecs: number;
  observationCount: number;
  firstSeenAt: number;
  lastSeenAt: number;
  mutedUntilAt?: number | null;
}

interface ActivityTimelineProps {
  onLoad?: () => Promise<CyralActivityEvent[]>;
  onLoadThemes?: () => Promise<ActivityTheme[]>;
  onMuteTheme?: (identityKey: string) => Promise<unknown>;
  onDeleteTheme?: (identityKey: string) => Promise<unknown>;
}

function formatTime(seconds: number): string {
  return new Date(seconds * 1000).toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatDuration(seconds: number): string {
  const minutes = Math.max(0, Math.floor(seconds / 60));
  return minutes >= 60 ? `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分钟` : `${minutes} 分钟`;
}

function sourceLabel(source: string): string {
  return source === "visual" ? "视觉摘要" : "窗口元数据";
}

function ActivityRow({ event }: { event: CyralActivityEvent }) {
  return (
    <article className="activity-row" role="listitem">
      <time className="activity-time" dateTime={new Date(event.observedAt * 1000).toISOString()}>
        {formatTime(event.observedAt)}
      </time>
      <div className="activity-event-body">
        <strong>观察了当前屏幕</strong>
        <div className="activity-name">{event.appName}</div>
        {event.windowTitle && <div className="activity-window-title">{event.windowTitle}</div>}
        <div className="activity-summary">{event.summary}</div>
        <span className="activity-source">{sourceLabel(event.source)}</span>
      </div>
    </article>
  );
}

function ThemeIcon({ kind }: { kind: ActivityTheme["kind"] }) {
  return <span className={`theme-kind-icon ${kind}`} aria-hidden="true">{kind === "media" ? "影" : "文"}</span>;
}

export function ActivityTimeline({ onLoad, onLoadThemes, onMuteTheme, onDeleteTheme }: ActivityTimelineProps) {
  const [events, setEvents] = useState<CyralActivityEvent[]>([]);
  const [themes, setThemes] = useState<ActivityTheme[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();

  const load = () => Promise.all([onLoad?.() ?? Promise.resolve([]), onLoadThemes?.() ?? Promise.resolve([])])
    .then(([loadedEvents, loadedThemes]) => {
      setEvents(loadedEvents);
      setThemes(loadedThemes);
    });

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(undefined);
    void load()
      .catch((reason) => { if (active) setError(reason instanceof Error ? reason.message : String(reason)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [onLoad, onLoadThemes]);

  const updateTheme = async (action: ((identityKey: string) => Promise<unknown>) | undefined, identityKey: string) => {
    if (!action) return;
    await action(identityKey);
    await load();
  };

  if (loading) return <div className="conversation-state">正在读取活动记录…</div>;
  if (error) return <div className="conversation-state error" role="alert">读取失败：{error}</div>;
  return (
    <div className="activity-usage-stack">
      {themes.length > 0 && <section className="activity-themes" aria-label="跨天主题记忆">
        <header className="activity-themes-header">
          <div>
            <span className="theme-eyebrow">ACTIVITY MEMORY</span>
            <h3>跨天主题</h3>
            <p>只保留跨日出现、可确认的活动线索。</p>
          </div>
          <span className="theme-count">{themes.length} 个主题</span>
        </header>
        <div className="activity-theme-grid">
          {themes.map((theme) => <article className="activity-theme-card" key={theme.identityKey}>
            <header className="activity-theme-card-header">
              <ThemeIcon kind={theme.kind} />
              <div>
                <strong>{theme.displayName}</strong>
                <p>{theme.kind === "media" ? "剧集 / 媒体" : "文件"} · 最近观察 {formatTime(theme.lastSeenAt)}</p>
              </div>
            </header>
            <div className="theme-metrics">
              <span className="theme-chip accent">出现 {theme.activeDays} 天</span>
              <span className="theme-chip">累计 {formatDuration(theme.totalActiveSecs)}</span>
            </div>
            <p className="theme-note">
              {theme.mutedUntilAt && theme.mutedUntilAt * 1000 > Date.now()
                ? "该主题已静默，希莉尔不会主动提起。"
                : "它只会在恰当时机自然问起，不按固定时间打扰你。"}
            </p>
            <div className="activity-theme-actions" aria-label={`主题控制：${theme.displayName}`}>
              <button type="button" onClick={() => void updateTheme(onMuteTheme, theme.identityKey)}>7 天不提起</button>
              <button type="button" onClick={() => void updateTheme(onDeleteTheme, theme.identityKey)}>删除主题</button>
            </div>
          </article>)}
        </div>
      </section>}
      {events.length === 0 ? <p className="todo-empty">还没有希莉尔活动记录。开启陪伴模式并发生观察后，这里会显示时间和场景摘要。</p> : <div className="activity-list" role="list" aria-label="希莉尔活动记录">{events.map((event) => <ActivityRow key={event.id} event={event} />)}</div>}
    </div>
  );
}