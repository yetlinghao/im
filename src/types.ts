// Mirrors src-tauri/src/model.rs. Field names are snake_case on the wire.

export type Protocol = "chat" | "anthropic" | "responses";
export type Role = "system" | "user" | "assistant";
export type Appearance = "system" | "light" | "dark";

export interface Usage {
  /** The whole prompt for this call, cached part included. */
  input_tokens?: number;
  /** How much of `input_tokens` the provider served from its prompt cache. */
  cached_input_tokens?: number;
  output_tokens?: number;
  reasoning_tokens?: number;
}

export interface TurnMeta {
  provider_id: string;
  protocol: Protocol;
  model: string;
  created_at: string;
  latency_ms?: number;
  ttft_ms?: number;
  thinking_ms?: number;
  usage?: Usage;
  finish_reason?: string;
  error?: string;
}

/** One piece of a multimodal message, in the Chat Completions wire shape. Image URLs are `data:` URLs. */
export type Part =
  | { type: "text"; text: string }
  | { type: "image_url"; image_url: { url: string; detail?: string } };

/** A plain string until a message carries images; then parts. */
export type Content = string | Part[];

export interface Message {
  role: Role;
  content: Content;
  created_at?: string;
  /** Chat-completions field name, so a stored trajectory replays as-is. */
  reasoning_content?: string;
  meta?: TurnMeta;
}

export interface Session {
  schema_version: number;
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  provider_id: string;
  model: string;
  system?: string;
  messages: Message[];
}

export interface SessionSummary {
  id: string;
  title: string;
  updated_at: string;
  provider_id: string;
  model: string;
  message_count: number;
}

export interface Provider {
  id: string;
  name: string;
  protocol: Protocol;
  base_url: string;
  models: string[];
}

export interface ProviderView extends Provider {
  has_key: boolean;
}

export type ProbeErrorCategory =
  | "invalid_url"
  | "connection"
  | "tls"
  | "timeout"
  | "authentication"
  | "endpoint"
  | "rate_limited"
  | "model_unavailable"
  | "unexpected_response"
  | "incomplete_stream"
  | "provider";

export interface ProviderProbeResult {
  ok: boolean;
  phase: "validation" | "models" | "stream";
  status: number | null;
  duration_ms: number;
  model_count: number | null;
  stream_ok: boolean;
  error_category: ProbeErrorCategory | null;
  message: string;
  detail: string | null;
  models_warning: string | null;
  models: string[] | null;
}
export interface ProviderInput extends Provider {
  /** undefined leaves the stored key alone; "" clears it. */
  api_key?: string;
}

export interface Settings {
  schema_version: number;
  appearance: Appearance;
  default_provider_id?: string;
  default_model?: string;
  system_prompt?: string;
  max_tokens: number;
  sidebar_visible: boolean;
  /** Column widths in CSS px; absent = the stylesheet default. */
  sidebar_width?: number;
  inspector_visible: boolean;
  inspector_width?: number;
  /** Width of the transcript/composer column in CSS px. */
  column_width?: number;
  /** System-wide shortcut for the quick-input panel, accelerator syntax (`Alt+Space`); "" = off. */
  quick_shortcut: string;
}

export type TurnKind =
  | {
      kind: "send";
      session_id?: string | null;
      provider_id: string;
      model: string;
      content: string;
      images?: string[];
    }
  | { kind: "regenerate"; session_id: string }
  | { kind: "edit"; session_id: string; content: string; images?: string[] };

export type TurnEvent =
  | { type: "started"; session: Session }
  | { type: "reasoning"; session_id: string; delta: string }
  | { type: "text"; session_id: string; delta: string }
  | {
      type: "done";
      session_id: string;
      message?: Message;
      error?: string;
      updated_at: string;
    };

/** A newer build offered by the release feed, and how far along installing it is. */
export interface UpdateState {
  version: string;
  notes?: string;
  phase: "available" | "downloading" | "installing" | "failed";
  /** 0–1 while downloading. */
  progress?: number;
  error?: string;
}

export interface ContextItem {
  id?: string;
  label?: string;
  enabled?: boolean;
  separator?: boolean;
}
