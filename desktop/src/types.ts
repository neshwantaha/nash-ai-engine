// Shared types mirroring the Rust IPC surface.

export type Role = "system" | "user" | "assistant" | "tool";

export type ContentPart =
  | { type: "text"; text: string }
  | { type: "image_url"; url: string; detail?: string | null };

export interface ToolCall {
  id: string;
  name: string;
  arguments: unknown;
}

export interface ChatMessage {
  role: Role;
  content: ContentPart[];
  tool_calls?: ToolCall[];
  tool_call_id?: string | null;
  name?: string | null;
}

export interface Usage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  reasoning_tokens?: number | null;
}

export interface ChatResponse {
  id?: string | null;
  provider: string;
  model: string;
  content: string;
  reasoning?: string | null;
  tool_calls: ToolCall[];
  finish_reason: string;
  usage?: Usage | null;
}

export interface ProviderSummary {
  id: string;
  displayName?: string | null;
  protocol: string;
  baseUrl: string;
  local: boolean;
  enabled: boolean;
  secretRef?: string | null;
  hasSecret: boolean;
  model?: string | null;
  models: string[];
  built: boolean;
  buildError?: string | null;
}

export interface ModelInfo {
  id: string;
  display_name?: string | null;
  context_window?: number | null;
  max_output_tokens?: number | null;
  owned_by?: string | null;
  created?: number | null;
}

export type CheckStatus = "pass" | "warn" | "fail" | "skip";

export interface CheckResult {
  name: string;
  status: CheckStatus;
  detail?: string | null;
  latency_ms?: number | null;
}

export interface ValidationReport {
  provider: string;
  checks: CheckResult[];
}

export interface Session {
  id: string;
  title?: string | null;
  provider?: string | null;
  model?: string | null;
  pinned: boolean;
  tags: string[];
  metadata: unknown;
  created_at: string;
  updated_at: string;
}

export interface StoredMessage {
  id: string;
  session_id: string;
  seq: number;
  role: string;
  message: ChatMessage;
  created_at: string;
}

export interface Paths {
  config: string;
  db: string;
}

export interface ChatArgs {
  requestId: string;
  provider?: string | null;
  model?: string | null;
  messages: ChatMessage[];
  temperature?: number | null;
  maxTokens?: number | null;
  save?: boolean;
  sessionId?: string | null;
}

export interface AgentArgs {
  requestId: string;
  task: string;
  dir?: string | null;
  model?: string | null;
  system?: string | null;
  approve?: string | null;
  maxSteps?: number | null;
}

export type ChatEventKind =
  | "started"
  | "delta"
  | "reasoning"
  | "usage"
  | "done"
  | "error"
  | "cancelled";

export interface ChatEvent {
  requestId: string;
  kind: ChatEventKind;
  text?: string;
  provider?: string;
  model?: string;
  finishReason?: string;
  usage?: Usage | null;
  sessionId?: string;
  error?: string;
}

export type Risk = "low" | "medium" | "high";

export type AgentEvent =
  | { type: "step"; index: number }
  | { type: "text_delta"; text: string }
  | { type: "reasoning_delta"; text: string }
  | { type: "tool_call"; id: string; name: string; arguments: unknown; risk: Risk }
  | { type: "tool_result"; id: string; name: string; content: string; is_error: boolean }
  | { type: "approval_denied"; id: string; name: string; reason: string }
  | {
      type: "finished";
      steps: number;
      tool_calls: number;
      denials: number;
      finish_reason: unknown;
    };

export interface AgentEventMsg {
  requestId: string;
  event: AgentEvent;
}

export interface ApprovalEvent {
  requestId: string;
  token: string;
  tool: string;
  args: unknown;
  risk: Risk;
  reason: string;
}

export interface AgentDone {
  requestId: string;
  result: {
    content: string;
    reasoning: string;
    provider: string;
    model: string;
    steps: number;
    tool_calls: number;
    denials: number;
    finish_reason: unknown;
    usage?: Usage | null;
  };
}

// Small helpers -------------------------------------------------------------

/// Human-readable label for a serialized FinishReason (unit variant string or
/// the `{ other: "..." }` object).
export function finishReasonLabel(value: unknown): string {
  if (typeof value === "string") return value;
  if (value && typeof value === "object" && "other" in value) {
    return String((value as { other: unknown }).other);
  }
  return String(value);
}

export function textOf(message: ChatMessage): string {
  return message.content
    .map((p) => (p.type === "text" ? p.text : `[image] ${p.url}`))
    .join("");
}

export function textMessage(role: Role, text: string): ChatMessage {
  return { role, content: [{ type: "text", text }] };
}
