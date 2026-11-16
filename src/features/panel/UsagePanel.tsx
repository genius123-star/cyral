import { useCallback, useEffect, useState } from "react";

export interface MonthlyUsage {
  month: string;
  usedUsd: number;
  budgetUsd: number;
  totalTokens: number;
}

export interface UsagePoint {
  day: string;
  totalTokens: number;
}

interface UsagePanelProps {
  onLoadUsage?: () => Promise<MonthlyUsage>;
  onLoadHistory?: () => Promise<UsagePoint[]>;
}

/** 无依赖的简易折线图：按最近 30 天 token 消耗绘制 SVG 折线。 */
function UsageChart({ points }: { points: UsagePoint[] }) {
  const width = 300;
  const height = 110;
  const pad = 8;
  if (points.length === 0) {
    return <p className="todo-empty">近 30 天还没有用量记录。</p>;
  }
  const max = Math.max(1, ...points.map((point) => point.totalTokens));
  const stepX = points.length > 1 ? (width - pad * 2) / (points.length - 1) : 0;
  const coords = points.map((point, index) => {
    const x = points.length > 1 ? pad + index * stepX : width / 2;
    const y = height - pad - (point.totalTokens / max) * (height - pad * 2);
    return `${x.toFixed(1)},${y.toFixed(1)}`;
  });
  const polyline = coords.join(" ");
  const first = points[0].day;
  const last = points[points.length - 1].day;
  return (
    <div className="usage-chart">
      <svg
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        aria-label={`近 30 天 token 消耗折线图，最高 ${max.toLocaleString()}`}
      >
        <polyline points={polyline} fill="none" stroke="#cc5a91" strokeWidth="2" />
        <circle cx={pad} cy={height - pad} r="2" fill="#cc5a91" />
        <circle
          cx={points.length > 1 ? width - pad : width / 2}
          cy={points.length > 1 ? height - pad - (points[points.length - 1].totalTokens / max) * (height - pad * 2) : height - pad}
          r="2"
          fill="#cc5a91"
        />
      </svg>
      <div className="usage-chart-axis">
        <span>{first}</span>
        <span>峰值 {max.toLocaleString()} tokens</span>
        <span>{last}</span>
      </div>
    </div>
  );
}

export function UsagePanel({ onLoadUsage, onLoadHistory }: UsagePanelProps) {
  const [usage, setUsage] = useState<MonthlyUsage>();
  const [points, setPoints] = useState<UsagePoint[]>([]);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    if (!onLoadUsage && !onLoadHistory) return;
    try {
      const [monthly, history] = await Promise.all([
        onLoadUsage?.() ?? Promise.resolve(undefined),
        onLoadHistory?.() ?? Promise.resolve([]),
      ]);
      if (monthly) setUsage(monthly);
      setPoints(history);
      setError("");
    } catch {
      setError("用量数据读取失败，请稍后重试。");
    }
  }, [onLoadUsage, onLoadHistory]);

  useEffect(() => {
    void load();
  }, [load]);

  if (error) return <p className="todo-error">{error}</p>;

  const percent = usage && usage.budgetUsd > 0 ? Math.round((usage.usedUsd / usage.budgetUsd) * 100) : 0;

  return (
    <div className="usage-panel">
      <div className="usage-summary">
        <span>本月（{usage?.month ?? "--"}）已用</span>
        <strong>${(usage?.usedUsd ?? 0).toFixed(2)}</strong>
        <span>
          上限 ${usage?.budgetUsd.toFixed(2) ?? "10.00"}（{percent}%）
        </span>
        <span>共 {usage?.totalTokens.toLocaleString() ?? 0} tokens（估算）</span>
      </div>
      <UsageChart points={points} />
    </div>
  );
}
