import { useEffect, useState } from "react";

export interface MemoryAtom {
  id: number;
  kind: string;
  content: string;
  sourceExcerpt: string;
  confidence: number;
  createdAt: number;
}

interface MemoryLibraryProps {
  onLoad?: () => Promise<MemoryAtom[]>;
  onUpdate?: (id: number, content: string) => Promise<unknown>;
  onDelete?: (id: number) => Promise<unknown>;
}

export function MemoryLibrary({ onLoad, onUpdate, onDelete }: MemoryLibraryProps) {
  const [memories, setMemories] = useState<MemoryAtom[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const [editingId, setEditingId] = useState<number>();
  const [draft, setDraft] = useState("");

  useEffect(() => {
    if (!onLoad) {
      setLoading(false);
      return;
    }
    void onLoad()
      .then(setMemories)
      .catch((reason) => setError(String(reason)))
      .finally(() => setLoading(false));
  }, [onLoad]);

  if (loading) return <div className="conversation-state">正在读取本地记忆…</div>;
  if (error) return <div className="conversation-state error">读取失败：{error}</div>;
  if (memories.length === 0) {
    return <div className="conversation-state">每 5 轮对话整理一次，目前还没有长期记忆。</div>;
  }

  return (
    <div className="memory-list">
      {memories.map((memory) => (
        <article className="memory-card" key={memory.id}>
          <header><span>{memory.kind}</span><strong>{Math.round(memory.confidence * 100)}%</strong></header>
          {editingId === memory.id ? (
            <input aria-label="记忆内容" value={draft} onChange={(event) => setDraft(event.target.value)} />
          ) : <p>{memory.content}</p>}
          <small>来源：{memory.sourceExcerpt}</small>
          <footer>
            {editingId === memory.id ? (
              <button type="button" aria-label="保存记忆" onClick={() => {
                void onUpdate?.(memory.id, draft).then(() => {
                  setMemories((current) => current.map((item) => item.id === memory.id
                    ? { ...item, content: draft, confidence: 1 }
                    : item));
                  setEditingId(undefined);
                });
              }}>保存</button>
            ) : (
              <button type="button" aria-label="编辑记忆" onClick={() => {
                setEditingId(memory.id);
                setDraft(memory.content);
              }}>编辑</button>
            )}
            <button type="button" aria-label="删除记忆" onClick={() => {
              void onDelete?.(memory.id).then(() => {
                setMemories((current) => current.filter((item) => item.id !== memory.id));
              });
            }}>删除</button>
          </footer>
        </article>
      ))}
    </div>
  );
}
