import { useEffect, useRef, useState } from "react";

export interface ConversationMessage {
  role: "user" | "assistant";
  content: string;
  createdAt: number;
  source?: "chat" | "proactive";
}

interface ConversationHistoryProps {
  onLoad?: () => Promise<ConversationMessage[]>;
}

export function ConversationHistory({ onLoad }: ConversationHistoryProps) {
  const [messages, setMessages] = useState<ConversationMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(undefined);
    void (onLoad?.() ?? Promise.resolve([]))
      .then((loaded) => {
        if (active) setMessages(loaded);
      })
      .catch((reason) => {
        if (active) setError(reason instanceof Error ? reason.message : String(reason));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [onLoad]);

  useEffect(() => {
    if (typeof endRef.current?.scrollIntoView === "function") {
      endRef.current.scrollIntoView({ block: "end" });
    }
  }, [messages]);

  if (loading) return <div className="conversation-state">正在读取本地对话…</div>;
  if (error) return <div className="conversation-state error" role="alert">读取失败：{error}</div>;
  if (messages.length === 0) {
    return <div className="conversation-state">还没有对话记录，去和希莉尔说句话吧。</div>;
  }

  return (
    <div className="conversation-list" role="log" aria-label="本地对话记录">
      {messages.map((message, index) => (
        <div className={`conversation-row ${message.role}`} key={`${message.createdAt}-${index}`}>
          <span className="conversation-avatar" aria-hidden="true">
            {message.role === "assistant" ? "希" : "我"}
          </span>
          <div className="conversation-bubble">
            {message.source === "proactive" && <small className="conversation-source">主动关心</small>}
            {message.content}
          </div>
        </div>
      ))}
      <div ref={endRef} />
    </div>
  );
}
