import { useEffect, useRef, useState } from "react";
import { api, errorString, uuid } from "../api";
import {
  finishReasonLabel,
  textMessage,
  type ChatEvent,
  type ChatMessage,
  type ProviderSummary,
} from "../types";

interface Turn {
  role: "user" | "assistant";
  text: string;
  provider?: string;
  model?: string;
  finish?: string;
}

export default function ChatPage() {
  const [providers, setProviders] = useState<ProviderSummary[]>([]);
  const [provider, setProvider] = useState("");
  const [model, setModel] = useState("");
  const [models, setModels] = useState<string[]>([]);
  const [system, setSystem] = useState("");
  const [stream, setStream] = useState(true);
  const [save, setSave] = useState(false);
  const [input, setInput] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [meta, setMeta] = useState<string | null>(null);

  const requestId = useRef<string | null>(null);
  const sessionId = useRef<string | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const streamingRef = useRef(false);

  useEffect(() => {
    void (async () => {
      try {
        const list = await api.listProviders();
        setProviders(list);
        const first = list.find((p) => p.enabled && p.built) ?? list[0];
        if (first) {
          setProvider(first.id);
          setModel(first.model ?? "");
          setModels(first.model ? [first.model, ...first.models] : first.models);
          setStream(first.protocol !== "custom_rest");
        }
      } catch (e) {
        setError(errorString(e));
      }
    })();
  }, []);

  useEffect(() => {
    const unlisten = api.onChatEvent(handleEvent);
    return () => {
      void unlisten.then((f) => f());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  }, [turns]);

  function handleEvent(ev: ChatEvent) {
    if (ev.requestId !== requestId.current) return;
    switch (ev.kind) {
      case "delta":
        setTurns((t) => {
          const copy = t.slice();
          const last = copy[copy.length - 1];
          if (last && last.role === "assistant") {
            copy[copy.length - 1] = { ...last, text: last.text + (ev.text ?? "") };
          } else {
            copy.push({ role: "assistant", text: ev.text ?? "" });
          }
          return copy;
        });
        break;
      case "done":
        sessionId.current = ev.sessionId ?? sessionId.current;
        setTurns((t) => {
          const copy = t.slice();
          const last = copy[copy.length - 1];
          if (last && last.role === "assistant") {
            copy[copy.length - 1] = {
              ...last,
              provider: ev.provider,
              model: ev.model,
              finish: ev.finishReason,
            };
          }
          return copy;
        });
        setMeta(
          `${ev.provider ?? ""} · ${ev.model ?? ""}${
            ev.sessionId ? ` · saved ${ev.sessionId.slice(0, 8)}` : ""
          }`,
        );
        setBusy(false);
        streamingRef.current = false;
        break;
      case "cancelled":
        setMeta("cancelled");
        setBusy(false);
        streamingRef.current = false;
        break;
      case "usage":
        if (ev.usage) {
          setMeta(
            (m) =>
              `${m ? m + " · " : ""}${ev.usage!.total_tokens} tokens`,
          );
        }
        break;
      case "error":
        setError(ev.error ?? "unknown error");
        setBusy(false);
        streamingRef.current = false;
        break;
      default:
        break;
    }
  }

  function onProviderChange(id: string) {
    setProvider(id);
    const p = providers.find((x) => x.id === id);
    if (p) {
      setModel(p.model ?? "");
      setModels(p.model ? [p.model, ...p.models] : p.models);
    }
  }

  function buildMessages(): ChatMessage[] {
    const msgs: ChatMessage[] = [];
    if (system.trim()) msgs.push(textMessage("system", system));
    for (const t of turns) msgs.push(textMessage(t.role, t.text));
    msgs.push(textMessage("user", input));
    return msgs;
  }

  async function send() {
    if (!input.trim() || busy) return;
    setError(null);
    setMeta(null);
    const userTurn: Turn = { role: "user", text: input };
    const historyMessages = buildMessages();
    setTurns((t) => [...t, userTurn]);
    setInput("");
    setBusy(true);

    const id = uuid();
    requestId.current = id;
    const args = {
      requestId: id,
      provider: provider || null,
      model: model || null,
      messages: historyMessages,
      save,
      sessionId: sessionId.current,
    };

    if (stream) {
      streamingRef.current = true;
      setTurns((t) => [...t, { role: "assistant", text: "" }]);
      try {
        await api.chatStream(args);
      } catch (e) {
        setError(errorString(e));
        setBusy(false);
        streamingRef.current = false;
      }
    } else {
      try {
        const resp = await api.chat(args);
        setTurns((t) => [
          ...t,
          {
            role: "assistant",
            text: resp.content,
            provider: resp.provider,
            model: resp.model,
            finish: finishReasonLabel(resp.finish_reason),
          },
        ]);
        setMeta(`${resp.provider} · ${resp.model}`);
      } catch (e) {
        setError(errorString(e));
      } finally {
        setBusy(false);
      }
    }
  }

  async function cancel() {
    if (requestId.current) await api.cancelChat(requestId.current);
  }

  return (
    <div className="chat-layout">
      <div>
        <div className="page-head" style={{ marginBottom: 12 }}>
          <h1>Chat</h1>
          <div className="row wrap">
            <select
              style={{ width: 180 }}
              value={provider}
              onChange={(e) => onProviderChange(e.target.value)}
            >
              {providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.displayName || p.id}
                  {p.built ? "" : " (not built)"}
                </option>
              ))}
            </select>
            <input
              style={{ width: 220 }}
              list="model-list"
              placeholder="model"
              value={model}
              onChange={(e) => setModel(e.target.value)}
            />
            <datalist id="model-list">
              {models.map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>
            <label className="row" style={{ margin: 0, gap: 4 }}>
              <input
                type="checkbox"
                style={{ width: "auto" }}
                checked={stream}
                onChange={(e) => setStream(e.target.checked)}
              />
              stream
            </label>
            <label className="row" style={{ margin: 0, gap: 4 }}>
              <input
                type="checkbox"
                style={{ width: "auto" }}
                checked={save}
                onChange={(e) => setSave(e.target.checked)}
              />
              save
            </label>
          </div>
        </div>
        <input
          placeholder="system prompt (optional)"
          value={system}
          onChange={(e) => setSystem(e.target.value)}
        />
      </div>

      <div className="transcript" ref={scrollRef}>
        {turns.length === 0 && (
          <div className="subtle">No messages yet. Type below to start.</div>
        )}
        {turns.map((t, i) => (
          <div key={i} className={`bubble ${t.role}`}>
            {t.role === "assistant" && (t.provider || t.finish) && (
              <div className="meta">
                {[t.provider, t.model, t.finish].filter(Boolean).join(" · ")}
              </div>
            )}
            {t.text || (t.role === "assistant" && busy ? "…" : "")}
          </div>
        ))}
        {error && <div className="card error-text">{error}</div>}
        {meta && <div className="subtle">{meta}</div>}
      </div>

      <div className="row" style={{ alignItems: "flex-end" }}>
        <textarea
          className="grow"
          rows={3}
          placeholder="Type a message…  (Enter to send, Shift+Enter for newline)"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send();
            }
          }}
        />
        <div className="col">
          <button className="primary" onClick={() => void send()} disabled={busy}>
            Send
          </button>
          <button onClick={() => void cancel()} disabled={!busy}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
