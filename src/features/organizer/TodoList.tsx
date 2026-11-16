import { useCallback, useEffect, useState } from "react";

export interface TodoItem {
  id: number;
  title: string;
  deadlineLocal: string;
  remindBeforeMinutes: number;
  sourceText: string;
  status: "draft" | "active" | "completed";
  createdAt: number;
}

export interface TodoConfirmation {
  id: number;
  title: string;
  deadlineLocal: string;
  remindBeforeMinutes: number;
}

interface TodoListProps {
  onLoad?: () => Promise<TodoItem[]>;
  onConfirm?: (todo: TodoConfirmation) => Promise<unknown>;
  onDiscard?: (id: number) => Promise<unknown>;
  onCreate?: (title: string, deadlineLocal: string) => Promise<unknown>;
  onComplete?: (id: number) => Promise<unknown>;
  reviewTodoId?: number;
}

type TodoView = "active" | "completed";

export function TodoList({ onLoad, onConfirm, onDiscard, onCreate, onComplete, reviewTodoId }: TodoListProps) {
  const [todos, setTodos] = useState<TodoItem[]>([]);
  const [error, setError] = useState("");
  const [view, setView] = useState<TodoView>("active");
  const [search, setSearch] = useState("");
  const [newTitle, setNewTitle] = useState("");
  const [newDeadline, setNewDeadline] = useState("");
  const [createOpen, setCreateOpen] = useState(false);

  const load = useCallback(async () => {
    if (!onLoad) return;
    try {
      setTodos(await onLoad());
      setError("");
    } catch {
      setError("待办读取失败，请稍后重试。");
    }
  }, [onLoad]);

  useEffect(() => {
    void load();
  }, [load, reviewTodoId]);

  const updateDraft = (id: number, patch: Partial<TodoItem>) => {
    setTodos((current) => current.map((todo) => todo.id === id ? { ...todo, ...patch } : todo));
  };

  const confirm = async (todo: TodoItem) => {
    if (!onConfirm) return;
    try {
      await onConfirm({
        id: todo.id,
        title: todo.title,
        deadlineLocal: todo.deadlineLocal,
        remindBeforeMinutes: todo.remindBeforeMinutes,
      });
      await load();
    } catch {
      setError("待办创建失败，请检查时间后重试。");
    }
  };

  const discard = async (id: number) => {
    if (!onDiscard) return;
    await onDiscard(id);
    await load();
  };

  const create = async () => {
    if (!onCreate || !newTitle.trim() || !newDeadline) return;
    try {
      await onCreate(newTitle.trim(), newDeadline);
      setNewTitle("");
      setNewDeadline("");
      setCreateOpen(false);
      await load();
    } catch {
      setError("待办创建失败，请检查时间后重试。");
    }
  };

  const complete = async (id: number) => {
    if (!onComplete) return;
    try {
      await onComplete(id);
      await load();
    } catch {
      setError("标记完成失败，请稍后重试。");
    }
  };

  if (error) return <p className="todo-error">{error}</p>;

  const drafts = todos.filter((todo) => todo.status === "draft");
  const activeTodos = todos.filter((todo) => todo.status === "active");
  const completedTodos = todos.filter((todo) => todo.status === "completed");
  const shownCompleted = completedTodos.filter((todo) =>
    todo.title.toLowerCase().includes(search.trim().toLowerCase()),
  );
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const tomorrow = new Date(today);
  tomorrow.setDate(tomorrow.getDate() + 1);
  const nextWeek = new Date(today);
  nextWeek.setDate(nextWeek.getDate() + 7);
  const groups = [
    { name: "已逾期", className: "overdue", items: activeTodos.filter((todo) => new Date(todo.deadlineLocal) < now) },
    { name: "今天", className: "today", items: activeTodos.filter((todo) => {
      const deadline = new Date(todo.deadlineLocal);
      return deadline >= now && deadline < tomorrow;
    }) },
    { name: "即将到期", className: "upcoming", items: activeTodos.filter((todo) => {
      const deadline = new Date(todo.deadlineLocal);
      return deadline >= tomorrow && deadline < nextWeek;
    }) },
    { name: "以后", className: "later", items: activeTodos.filter((todo) => new Date(todo.deadlineLocal) >= nextWeek) },
  ];

  const formatDeadline = (value: string) => {
    const deadline = new Date(value);
    const time = deadline.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
    if (deadline < now) {
      const minutes = Math.max(1, Math.floor((now.getTime() - deadline.getTime()) / 60_000));
      return minutes < 60 ? `逾期 ${minutes} 分` : `逾期 ${Math.floor(minutes / 60)} 小时`;
    }
    if (deadline < tomorrow) return `今天 ${time}`;
    const afterTomorrow = new Date(tomorrow);
    afterTomorrow.setDate(afterTomorrow.getDate() + 1);
    if (deadline < afterTomorrow) return `明天 ${time}`;
    return `${deadline.getMonth() + 1}月${deadline.getDate()}日 ${time}`;
  };

  return (
    <div className="todo-list">
      <div className="todo-tabs" role="tablist" aria-label="待办视图">
        <button
          type="button"
          role="tab"
          aria-selected={view === "active"}
          onClick={() => setView("active")}
        >
          待办（{activeTodos.length}）
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={view === "completed"}
          onClick={() => setView("completed")}
        >
          已完成（{completedTodos.length}）
        </button>
      </div>

      {view === "active" ? (
        <>
          {onCreate && !createOpen && (
            <button type="button" className="todo-create-trigger" onClick={() => setCreateOpen(true)}>
              <span aria-hidden="true">＋</span>新建待办
            </button>
          )}
          {onCreate && createOpen && (
            <form
              className="todo-create"
              onSubmit={(event) => {
                event.preventDefault();
                void create();
              }}
            >
              <div className="todo-create-head">
                <strong>新建待办</strong>
                <button type="button" aria-label="关闭新建待办" onClick={() => setCreateOpen(false)}>×</button>
              </div>
              <label>
                <input
                  aria-label="新建待办事项"
                  value={newTitle}
                  onChange={(event) => setNewTitle(event.target.value)}
                  placeholder="例如：提交周报"
                />
              </label>
              <label>
                截止时间
                <input
                  aria-label="新建待办截止时间"
                  type="datetime-local"
                  value={newDeadline}
                  onChange={(event) => setNewDeadline(event.target.value)}
                />
              </label>
              <small>提醒：截止前 2 小时、30 分钟、5 分钟各提醒一次</small>
              <button type="submit" disabled={!newTitle.trim() || !newDeadline}>
                添加待办
              </button>
            </form>
          )}

          {drafts.map((todo) => (
              <article className="todo-draft" key={todo.id}>
                <div className="todo-draft-head"><strong>待确认</strong><span>来自对话</span></div>
                <label>事项<input aria-label="事项" value={todo.title} onChange={(event) => updateDraft(todo.id, { title: event.target.value })} /></label>
                <label>截止时间<input aria-label="截止时间" type="datetime-local" value={todo.deadlineLocal} onChange={(event) => updateDraft(todo.id, { deadlineLocal: event.target.value })} /></label>
                <small>“{todo.sourceText}”</small>
                <small>提醒：截止前 2 小时、30 分钟、5 分钟各提醒一次</small>
                <div className="todo-actions">
                  <button type="button" onClick={() => void discard(todo.id)}>忽略</button>
                  <button type="button" aria-label="确认创建待办" onClick={() => void confirm(todo)}>确认时间</button>
                </div>
              </article>
          ))}

          {activeTodos.length === 0 && drafts.length === 0 ? (
            <p className="todo-empty">还没有待办。你可以在对话中直接告诉希莉尔，或在上方新建。</p>
          ) : (
            groups.filter((group) => group.items.length > 0).map((group) => (
              <section className={`todo-group ${group.className}`} key={group.name}>
                <h3>{group.name}</h3>
                <span className="todo-group-count">{group.items.length}</span>
                {group.items.map((todo) => (
                  <article className="todo-active" key={todo.id}>
                    <button className="todo-check" type="button" aria-label={`完成待办 ${todo.title}`} onClick={() => void complete(todo.id)} />
                    <div>
                      <strong>{todo.title}</strong>
                      <span>截止前 2 小时、30 分钟、5 分钟提醒</span>
                    </div>
                    <time dateTime={todo.deadlineLocal}>{formatDeadline(todo.deadlineLocal)}</time>
                  </article>
                ))}
              </section>
            ))
          )}
        </>
      ) : (
        <>
          <label className="todo-search">
            搜索已完成事项
            <input
              aria-label="搜索已完成事项"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              placeholder="输入标题关键词"
            />
          </label>
          {shownCompleted.length === 0 ? (
            <p className="todo-empty">没有匹配的已完成事项。</p>
          ) : (
            shownCompleted.map((todo) => (
              <article className="todo-completed" key={todo.id}>
                <s>{todo.title}</s>
                <span>{new Date(todo.deadlineLocal).toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" })}</span>
              </article>
            ))
          )}
        </>
      )}
    </div>
  );
}
