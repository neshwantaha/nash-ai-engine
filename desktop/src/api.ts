// Typed wrappers around the Tauri IPC surface.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AgentArgs,
  AgentDone,
  AgentEventMsg,
  ApprovalEvent,
  ChatArgs,
  ChatEvent,
  ChatResponse,
  ModelInfo,
  Paths,
  ProviderSummary,
  Session,
  StoredMessage,
  ValidationReport,
} from "./types";

export const api = {
  // config -----------------------------------------------------------------
  getPaths: () => invoke<Paths>("get_paths"),
  getConfig: () => invoke<Record<string, unknown>>("get_config"),
  saveConfig: (config: unknown) => invoke<void>("save_config", { config }),

  // providers --------------------------------------------------------------
  listProviders: () => invoke<ProviderSummary[]>("list_providers"),
  listModels: (provider: string) => invoke<ModelInfo[]>("list_models", { provider }),
  testProvider: (provider: string) =>
    invoke<ValidationReport>("test_provider", { provider }),
  testAll: () => invoke<ValidationReport[]>("test_all"),

  // chat -------------------------------------------------------------------
  chat: (args: ChatArgs) => invoke<ChatResponse>("chat", { args }),
  chatStream: (args: ChatArgs) => invoke<void>("chat_stream", { args }),
  cancelChat: (requestId: string) => invoke<boolean>("cancel_chat", { requestId }),

  // secrets ----------------------------------------------------------------
  listSecrets: () => invoke<string[]>("list_secrets"),
  setSecret: (name: string, value: string) =>
    invoke<void>("set_secret", { name, value }),
  removeSecret: (name: string) => invoke<boolean>("remove_secret", { name }),
  setProviderKey: (provider: string, value: string) =>
    invoke<void>("set_provider_key", { provider, value }),

  // sessions ---------------------------------------------------------------
  listSessions: (limit?: number, offset?: number) =>
    invoke<Session[]>("list_sessions", { limit, offset }),
  getMessages: (sessionId: string) =>
    invoke<StoredMessage[]>("get_messages", { sessionId }),
  createSession: (title?: string) => invoke<Session>("create_session", { title }),
  deleteSession: (id: string) => invoke<boolean>("delete_session", { id }),
  renameSession: (id: string, title: string) =>
    invoke<void>("rename_session", { id, title }),
  setSessionPinned: (id: string, pinned: boolean) =>
    invoke<void>("set_session_pinned", { id, pinned }),

  // agent ------------------------------------------------------------------
  agentRun: (args: AgentArgs) => invoke<void>("agent_run", { args }),
  resolveApproval: (token: string, allow: boolean, reason?: string) =>
    invoke<boolean>("resolve_approval", { token, allow, reason }),

  // events -----------------------------------------------------------------
  onChatEvent: (cb: (e: ChatEvent) => void): Promise<UnlistenFn> =>
    listen<ChatEvent>("chat://event", (ev) => cb(ev.payload)),
  onAgentEvent: (cb: (e: AgentEventMsg) => void): Promise<UnlistenFn> =>
    listen<AgentEventMsg>("agent://event", (ev) => cb(ev.payload)),
  onAgentApproval: (cb: (e: ApprovalEvent) => void): Promise<UnlistenFn> =>
    listen<ApprovalEvent>("agent://approval", (ev) => cb(ev.payload)),
  onAgentDone: (cb: (e: AgentDone) => void): Promise<UnlistenFn> =>
    listen<AgentDone>("agent://done", (ev) => cb(ev.payload)),
  onAgentError: (cb: (e: { requestId: string; error: string }) => void): Promise<UnlistenFn> =>
    listen<{ requestId: string; error: string }>("agent://error", (ev) => cb(ev.payload)),
};

export function errorString(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}

export function uuid(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `${Date.now()}-${Math.random().toString(16).slice(2)}`;
}
