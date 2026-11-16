import { useEffect, useState } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { PetSurface } from "./features/pet/PetSurface";
import { ControlPanel } from "./features/panel/ControlPanel";
import { ChatComposer } from "./features/chat/ChatComposer";
import type { SavedModelProvider } from "./features/settings/ModelSettings";
import type { ConversationMessage } from "./features/chat/ConversationHistory";
import type { MemoryAtom } from "./features/organizer/MemoryLibrary";
import type { TodoConfirmation, TodoItem } from "./features/organizer/TodoList";
import type { PrivacyConfig } from "./features/settings/PrivacySettings";
import type { ActivityTheme, CyralActivityEvent } from "./features/companion/ActivityTimeline";
import type { SavedVisionProvider } from "./features/settings/VisionSettings";
import type { ModeState } from "./features/companion/ModeStatus";
import type { AppSettings } from "./features/settings/SettingsPanel";
import type { MonthlyUsage, UsagePoint } from "./features/panel/UsagePanel";
import type { EmailAccountInput, EmailAccountView } from "./features/email/EmailSettings";
import type { EmailDraftReview } from "./features/email/EmailReviewCard";
import {
  transitionThinkingAnimation,
  type ThinkingVariant,
} from "./features/pet/thinking-animation";
import { runChatRequest } from "./features/chat/chat-request";
import { bubbleDurationMs } from "./features/chat/bubble-duration";
import {
  actionSpec,
  COMPACT_ACTION,
  ENTER_ACTION,
  EXIT_ACTION,
  IDLE_ACTION,
  IDLE_ACTIONS,
  IDLE_FALLBACK,
  loadPetManifest,
  PEEK_ACTIONS,
  preloadActionImages,
  resolveAction,
  WALK_ACTIONS,
  type PetActionSpec,
} from "./features/pet/pet-manifest";

interface PetState {
  locked: boolean;
  lastActiveAt: number;
}

type PetMotionState = "normal-idle" | "walking" | "compacting" | "compact" | "restoring";

interface TodoReminder {
  id: number;
  message: string;
}

export interface DroppedFileView {
  candidateId: string;
  name: string;
  sizeBytes: number;
  textReadable: boolean;
}

export function App() {
  const [locked, setLocked] = useState(false);
  const [welcomeMessage, setWelcomeMessage] = useState<string>();
  const [thinkingVariant, setThinkingVariant] = useState<ThinkingVariant>();
  const [reminder, setReminder] = useState<TodoReminder>();
  const [todoReviewId, setTodoReviewId] = useState<number>();
  const [emailReviewId, setEmailReviewId] = useState<string>();
  const [emailReviewResult, setEmailReviewResult] = useState<EmailDraftReview>();
  const [motion, setMotion] = useState<PetMotionState>("normal-idle");
  const [idleSpecs, setIdleSpecs] = useState<PetActionSpec[]>([IDLE_FALLBACK]);
  const [walkSpecs, setWalkSpecs] = useState<PetActionSpec[]>([]);
  const [compactSpec, setCompactSpec] = useState<PetActionSpec>();
  const [enterSpec, setEnterSpec] = useState<PetActionSpec>();
  const [exitSpec, setExitSpec] = useState<PetActionSpec>();
  const [ambientSpecs, setAmbientSpecs] = useState<PetActionSpec[]>([]);
  // 宠物窗口当前贴近的屏幕边缘（Rust 随 pet-motion 事件下发），用于限制边缘探头动画。
  const [peekEdges, setPeekEdges] = useState<string[]>([]);
  const [droppedFiles, setDroppedFiles] = useState<DroppedFileView[]>([]);
  const [dropError, setDropError] = useState<string>();
  const currentWindow = getCurrentWindow();

  // setup 竞态兜底：Tauri 的 setup() 在窗口加载后执行，早期 invoke 会因状态未托管而失败，重试直至成功。
  const invokeWithRetry = async <T,>(
    command: string,
    args?: Record<string, unknown>,
    attempts = 12,
    delayMs = 500,
  ): Promise<T> => {
    let lastError: unknown;
    for (let attempt = 0; attempt < attempts; attempt += 1) {
      try {
        return await invoke<T>(command, args);
      } catch (error) {
        lastError = error;
        await new Promise((resolve) => setTimeout(resolve, delayMs));
      }
    }
    throw lastError;
  };

  useEffect(() => {
    if (currentWindow.label !== "pet") return;

    void invokeWithRetry<PetState>("get_pet_state").then((state) => {
      setLocked(state.locked);
    });
    // 动画通道：加载 manifest + 预加载帧条，把可用动作上报 Rust（walk 缺失则关闭自动游走）。
    void loadPetManifest()
      .then(async (manifest) => {
        const available = await preloadActionImages(manifest.actions);
        const idles = IDLE_ACTIONS
          .map((action) => actionSpec(manifest, action))
          .filter((spec): spec is PetActionSpec => Boolean(spec && available.has(spec.action)));
        setIdleSpecs(idles.length > 0 ? idles : [resolveAction(IDLE_ACTION, available, manifest)]);
        const walks = WALK_ACTIONS
          .map((action) => actionSpec(manifest, action))
          .filter((spec): spec is PetActionSpec => Boolean(spec && available.has(spec.action)));
        setWalkSpecs(walks);
        const compact = actionSpec(manifest, COMPACT_ACTION);
        if (compact && available.has(COMPACT_ACTION)) {
          setCompactSpec(compact);
        }
        const enter = actionSpec(manifest, ENTER_ACTION);
        if (enter && available.has(ENTER_ACTION)) setEnterSpec(enter);
        const exit = actionSpec(manifest, EXIT_ACTION);
        if (exit && available.has(EXIT_ACTION)) setExitSpec(exit);
        setAmbientSpecs(
          PEEK_ACTIONS
            .map((action) => actionSpec(manifest, action))
            .filter((spec): spec is PetActionSpec => Boolean(spec && available.has(spec.action))),
        );
        void invokeWithRetry("set_animation_available", { walk: walks.length > 0 }).catch(() => {
          // setup 竞态兜底：重试耗尽后保持 walk 不可用，下次会话重试。
        });
      })
      .catch(() => {
        // manifest 缺失：回退现有 idle，并如实上报 walk 不可用。
        setIdleSpecs([IDLE_FALLBACK]);
        void invokeWithRetry("set_animation_available", { walk: false }).catch(() => {});
      });
    void invokeWithRetry<{ state: PetMotionState }>("get_pet_motion")
      .then((snapshot) => setMotion(snapshot.state))
      .catch(() => {
        // 启动瞬间状态未就绪属正常，pet-motion 事件会随后同步。
      });

  }, [currentWindow.label]);

  useEffect(() => {
    if (currentWindow.label !== "composer") return;
    let unlisten: UnlistenFn | undefined;
    void currentWindow.onDragDropEvent(async (event) => {
      if (event.payload.type !== "drop") return;
      setDropError(undefined);
      if (droppedFiles.length + event.payload.paths.length > 10) {
        setDropError("一次最多拖入 10 个文件");
        return;
      }
      try {
        const registered = await invoke<DroppedFileView[]>("register_dropped_files", {
          paths: event.payload.paths,
        });
        setDroppedFiles((current) => [...current, ...registered]);
      } catch (reason) {
        setDropError(reason instanceof Error ? reason.message : String(reason));
      }
    }).then((dispose) => { unlisten = dispose; });
    return () => unlisten?.();
  }, [currentWindow.label, droppedFiles.length]);

  useEffect(() => {
    if (currentWindow.label !== "composer") return;
    void currentWindow.setSize(new LogicalSize(720, droppedFiles.length > 0 ? 138 : 76));
  }, [currentWindow.label, droppedFiles.length]);

  useEffect(() => {
    if (currentWindow.label !== "pet") return;

    const disposers: UnlistenFn[] = [];
    let replyTimer: ReturnType<typeof setTimeout> | undefined;
    const showQueuedMessages = (messages: string[]) => {
      clearTimeout(replyTimer);
      const [message, ...remaining] = messages.filter((item) => item.trim());
      if (!message) {
        setWelcomeMessage(undefined);
        return;
      }
      setThinkingVariant(undefined);
      setWelcomeMessage(message);
      replyTimer = setTimeout(
        () => showQueuedMessages(remaining),
        bubbleDurationMs(message),
      );
    };
    const register = async () => {
      disposers.push(await listen<string[]>("startup-briefing", (event) => {
        showQueuedMessages(event.payload);
      }));
      disposers.push(await listen("assistant-stream-start", () => {
        clearTimeout(replyTimer);
        setWelcomeMessage(undefined);
        setThinkingVariant((current) => transitionThinkingAnimation(current, "start"));
      }));
      disposers.push(await listen<string>("assistant-stream-delta", (event) => {
        setThinkingVariant((current) => transitionThinkingAnimation(current, "delta", Math.random, event.payload));
        setWelcomeMessage((current) => `${current ?? ""}${event.payload}`);
      }));
      disposers.push(await listen<string>("assistant-stream-end", (event) => {
        setThinkingVariant((current) => transitionThinkingAnimation(current, "end"));
        setWelcomeMessage(event.payload);
        replyTimer = setTimeout(() => setWelcomeMessage(undefined), bubbleDurationMs(event.payload));
      }));
      disposers.push(await listen<string>("assistant-stream-error", () => {
        setThinkingVariant((current) => transitionThinkingAnimation(current, "error"));
        setWelcomeMessage((current) => {
          const message = current && current !== "…"
            ? `${current}\n（回复中断）`
            : "这次没有连接成功。";
          replyTimer = setTimeout(() => setWelcomeMessage(undefined), bubbleDurationMs(message));
          return message;
        });
      }));
      disposers.push(
        await listen<{ state: PetMotionState; nearEdges?: string[] }>("pet-motion", (event) => {
          setMotion(event.payload.state);
          setPeekEdges(event.payload.nearEdges ?? []);
        }),
      );
      disposers.push(await listen<TodoReminder>("todo-reminder", (event) => {
        clearTimeout(replyTimer);
        setThinkingVariant(undefined);
        setWelcomeMessage(undefined);
        setReminder(event.payload);
      }));
      // 所有 pet 事件监听器就绪后再允许 Rust 投递启动问候和到期提醒。
      await invokeWithRetry("mark_pet_frontend_ready");
    };
    void register();

    return () => {
      disposers.forEach((dispose) => dispose());
      clearTimeout(replyTimer);
    };
  }, [currentWindow.label]);

  useEffect(() => {
    if (currentWindow.label !== "pet") return;

    let unlisten: UnlistenFn | undefined;
    let replyTimer: ReturnType<typeof setTimeout> | undefined;
    void listen<string>("assistant-message", (event) => {
      setWelcomeMessage(event.payload);
      setThinkingVariant(undefined);
      clearTimeout(replyTimer);
      replyTimer = setTimeout(() => setWelcomeMessage(undefined), bubbleDurationMs(event.payload));
    }).then((dispose) => {
      unlisten = dispose;
    });

    return () => {
      unlisten?.();
      clearTimeout(replyTimer);
    };
  }, [currentWindow.label]);

  useEffect(() => {
    if (currentWindow.label !== "panel") return;
    const disposers: UnlistenFn[] = [];
    void listen<number>("todo-review-request", (event) => setTodoReviewId(event.payload)).then((dispose) => disposers.push(dispose));
    void listen<string>("email-review-request", (event) => { setEmailReviewId(event.payload); setEmailReviewResult(undefined); }).then((dispose) => disposers.push(dispose));
    void listen<EmailDraftReview>("email-send-result", (event) => { setEmailReviewId(event.payload.id); setEmailReviewResult(event.payload); }).then((dispose) => disposers.push(dispose));
    return () => disposers.forEach((dispose) => dispose());
  }, [currentWindow.label]);

  if (currentWindow.label === "panel") {
    return (
      <ControlPanel
        onClose={() => void currentWindow.hide()}
        todoReviewId={todoReviewId}
        emailReviewId={emailReviewId}
        emailReviewResult={emailReviewResult}
        onSaveModel={(settings) => invoke("save_model_provider", { settings })}
        onLoadModel={() => invoke<SavedModelProvider | null>("get_model_provider")}
        onLoadMessages={() => invoke<ConversationMessage[]>("get_recent_messages", { limit: 200 })}
        onLoadMemories={() => invoke<MemoryAtom[]>("get_memory_atoms")}
        onUpdateMemory={(id, content) => invoke("update_memory", { id, content })}
        onDeleteMemory={(id) => invoke("delete_memory", { id })}
        onLoadTodos={() => invoke<TodoItem[]>("get_todos")}
        onConfirmTodo={(todo: TodoConfirmation) => invoke("confirm_todo_draft", {
          id: todo.id,
          title: todo.title,
          deadlineLocal: todo.deadlineLocal,
          remindBeforeMinutes: todo.remindBeforeMinutes,
        })}
        onDiscardTodo={(id) => invoke("discard_todo_draft", { id })}
        onCreateTodo={(title, deadlineLocal) => invoke("create_todo", { title, deadlineLocal })}
        onCompleteTodo={(id) => invoke("complete_todo_reminder", { id })}
        onLoadPrivacy={() => invoke<PrivacyConfig>("get_privacy_config")}
        onSavePrivacy={(config) => invoke<PrivacyConfig>("save_privacy_config", { config })}
        onLoadActivity={() => invoke<CyralActivityEvent[]>("get_cyral_activity_events")}
        onLoadThemes={() => invoke<ActivityTheme[]>("get_activity_themes")}
        onMuteTheme={(identityKey) => invoke("mute_activity_theme", { identityKey })}
        onDeleteTheme={(identityKey) => invoke("delete_activity_theme", { identityKey })}
        onLoadMode={() => invoke<ModeState>("get_mode_state")}
        onToggleCompanion={async (enabled) => {
          try {
            await invoke("set_companion_mode", { enabled });
            return undefined;
          } catch (reason) {
            return reason instanceof Error ? reason.message : String(reason);
          }
        }}
        onSaveVision={(settings) => invoke("save_vision_provider", { settings })}
        onLoadVision={() => invoke<SavedVisionProvider | null>("get_vision_provider")}
        onLoadSettings={() => invoke<AppSettings>("get_app_settings")}
        onSaveSettings={(settings) => invoke("save_app_settings", { settings })}
        onLoadEmailAccounts={() => invoke<EmailAccountView[]>("get_email_accounts")}
        onSaveEmailAccount={(account: EmailAccountInput, credential?: string) => invoke<EmailAccountView>("save_email_account", { account, credential })}
        onDeleteEmailAccount={(id) => invoke("delete_email_account", { id })}
        onConnectGmail={(id) => invoke<EmailAccountView>("connect_gmail_account", { id })}
        onDisconnectGmail={(id) => invoke<EmailAccountView>("disconnect_gmail_account", { id })}
        onTestEmailConnection={(id) => invoke<string>("test_email_account_connection", { id })}
        onLoadEmailDraft={(id) => invoke<EmailDraftReview>("get_email_draft", { id })}
        onUpdateEmailDraft={(review) => invoke<EmailDraftReview>("update_email_draft", {
          id: review.id,
          accountId: review.accountId,
          recipients: review.recipients,
          subject: review.subject,
          body: review.body,
        })}
        onCancelEmailDraft={(id) => invoke("cancel_email_draft", { id })}
        onSendEmailDraft={(id, revision) => invoke<EmailDraftReview>("send_email_draft", { id, revision })}
        onLoadUsage={() => invoke<MonthlyUsage>("get_monthly_usage")}
        onLoadUsageHistory={() => invoke<UsagePoint[]>("get_usage_history")}
      />
    );
  }

  if (currentWindow.label === "composer") {
    return (
      <ChatComposer
        droppedFiles={droppedFiles}
        externalError={dropError}
        onRemoveFile={(candidateId) => {
          setDroppedFiles((current) => current.filter((file) => file.candidateId !== candidateId));
          void invoke("discard_dropped_file", { candidateId });
        }}
        onClose={() => {
          for (const file of droppedFiles) {
            void invoke("discard_dropped_file", { candidateId: file.candidateId });
          }
          setDroppedFiles([]);
          setDropError(undefined);
          void invoke("hide_composer");
        }}
        onSend={(message, candidateIds) => runChatRequest(
          () => invoke("begin_chat_request"),
          () => invoke("send_message", { message, fileCandidateIds: candidateIds }),
          () => invoke("fail_chat_request"),
        ).then((result) => {
          setDroppedFiles([]);
          return result;
        })}
      />
    );
  }

  return (
    <PetSurface
      locked={locked}
      welcomeMessage={welcomeMessage}
      reminder={reminder}
      thinkingVariant={thinkingVariant}
      onToggleLock={() => {
        const nextLocked = !locked;
        setLocked(nextLocked);
        void invoke("set_pet_locked", { locked: nextLocked });
      }}
      onHide={() => void invoke("hide_pet")}
      onOpenPanel={() => void invoke("open_panel")}
      onOpenComposer={() => void invoke("open_composer")}
      onStartDragging={async () => {
        await invoke("pet_drag_started").catch(() => {});
        await currentWindow.startDragging();
      }}
      onPointerActive={() => void invoke("pet_pointer_active")}
      onRestoreFromCompact={() => void invoke("restore_from_compact", { openComposer: true })}
      motion={motion}
      idleSpecs={idleSpecs}
      walkSpecs={walkSpecs}
      compactSpec={compactSpec}
      enterSpec={enterSpec}
      exitSpec={exitSpec}
      ambientSpecs={ambientSpecs}
      peekEdges={peekEdges}
      onCompleteReminder={(id) => {
        void invoke("complete_todo_reminder", { id }).then(() => setReminder(undefined));
      }}
      onSnoozeReminder={(id) => {
        void invoke("snooze_todo_reminder", { id }).then(() => setReminder(undefined));
      }}
      onStopCompanion={() => void invoke("stop_companion_observation")}
    />
  );
}
