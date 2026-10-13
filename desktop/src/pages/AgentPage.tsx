import { useEffect, useRef, useState } from "react";
import { api, errorString, uuid } from "../api";
import {
  finishReasonLabel,
  type AgentEvent,
  type ApprovalEvent,
  type ProviderSummary,
} from "../types";

interface LogLine {
  kind: "info" | "text" | "tool" | "result" | "error" | "done";
  text: string;
}

export default function AgentPage() {
  const [providers, setProviders] = useState<ProviderSummary[]>([]);
  const [model, setModel] = useState("");
  const [providerId, setProviderId] = useState("");
  const [task, setTask] = useState("");
  const [dir, setDir] = useState("");
  const [system, setSystem] = useState("");
  const [approve, setApprove] = useState("ask");
  const [maxSteps, setMaxSteps] = useState(12);
  const [lines, setLines] = useState<LogLine[]>([]);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [approval, setApproval] = useState<ApprovalEvent | null>(null);

  const requestId = useRef<string | null>(null);
  const logRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    void (async () => {
      try {
        setProviders(await api.listProviders());
      } catch (e) {
        setError(errorString(e));
      }
    })();
  }, []);

  useEffect(() => {
    const unlistenEvent = api.onAgentEvent(({ requestId: rid, event }) => {
      if (rid !== requestId.current) return;
      applyEvent(event);
    });
    const unlistenApproval = api.onAgentApproval((ev) => {
      if (ev.requestId !== requestId.current) return;
      setApproval(ev);
    });
    const unlistenDone = api.onAgentDone(({ requestId: rid, result }) => {
      if (rid !== requestId.current) return;
      append({
        kind: "done",
        text: `done · ${result.steps} steps, ${result.tool_calls} tool calls, ${result.denials} denied · ${finishReasonLabel(
          result.finish_reason,
        )}`,
      });
      if (result.content) append({ kind: "text", text: result.content });
      setRunning(false);
    });
    const unlistenError = api.onAgentError(({ requestId: rid, error: err }) => {
      if (rid !== requestId.current) return;
      append({ kind: "error", text: err });
      setError(err);
      setRunning(false);
    });
    return () => {
      void unlistenEvent.then((f) => f());
      void unlistenApproval.then((f) => f());
      void unlistenDone.then((f) => f());
      void unlistenError.then((f) => f());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight });
  }, [lines]);

  function append(line: LogLine) {
    setLines((l) => [...l, line]);
  }

  function applyEvent(event: AgentEvent) {
    switch (event.type) {
      case "step":
        append({ kind: "info", text: `— step ${event.index} —` });
        break;
      case "text_delta":
        setLines((l) => {
          const copy = l.slice();
          const last = copy[copy.length - 1];
          if (last && last.kind === "text") {
            copy[copy.length - 1] = { kind: "text", text: last.text + event.text };
          } else {
            copy.push({ kind: "text", text: event.text });
          }
          return copy;
        });
        break;
      case "reasoning_delta":
        break;
      case "tool_call":
        append({
          kind: "tool",
          text: `→ ${event.name} (${event.risk}) ${JSON.stringify(event.arguments)}`,
        });
        break;
      case "tool_result":
        append({
          kind: event.is_error ? "error" : "result",
          text: `← ${event.name}: ${event.content}`,
        });
        break;
      case "approval_denied":
        append({
          kind: "error",
          text: `denied ${event.name}: ${event.reason}`,
        });
        break;
      case "finished":
        break;
      default:
        break;
    }
  }

  function selectProvider(id: string) {
    setProviderId(id);
    const p = providers.find((x) => x.id === id);
    if (p) setModel(p.model ?? "");
  }

  async function run() {
    if (!task.trim() || running) return;
    setError(null);
    setLines([]);
    setRunning(true);
    const id = uuid();
    requestId.current = id;
    try {
      await api.agentRun({
        requestId: id,
        task,
        dir: dir || null,
        model: model || null,
        system: system || null,
        approve,
        maxSteps,
      });
    } catch (e) {
      setError(errorString(e));
      setRunning(false);
    }
  }

  async function decide(allow: boolean) {
    if (!approval) return;
    await api.resolveApproval(approval.token, allow);
    setApproval(null);
  }

  return (
    <div>
      <div className="page-head">
        <div>
          <h1>Agent</h1>
          <div className="subtle">
            A tool-using coding agent: read, search, write, edit, run.
          </div>
        </div>
      </div>

      <div className="agent-grid">
        <div className="card">
          <div className="col">
            <div>
              <label>Default model / provider</label>
              <select value={providerId} onChange={(e) => selectProvider(e.target.value)}>
                <option value="">(provider default)</option>
                {providers.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.displayName || p.id}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label>Model override</label>
              <input value={model} onChange={(e) => setModel(e.target.value)} />
            </div>
            <div>
              <label>Working directory</label>
              <input
                placeholder="(default)"
                value={dir}
                onChange={(e) => setDir(e.target.value)}
              />
            </div>
            <div>
              <label>Approval policy</label>
              <select value={approve} onChange={(e) => setApprove(e.target.value)}>
                <option value="ask">ask (prompt for risky tools)</option>
                <option value="auto">auto (allow everything)</option>
                <option value="deny">read-only (deny writes/shell)</option>
              </select>
            </div>
            <div className="grid-2">
              <div>
                <label>Max steps</label>
                <input
                  type="number"
                  min={1}
                  value={maxSteps}
                  onChange={(e) => setMaxSteps(Number(e.target.value) || 1)}
                />
              </div>
            </div>
            <div>
              <label>System prompt (optional)</label>
              <textarea
                rows={3}
                value={system}
                onChange={(e) => setSystem(e.target.value)}
              />
            </div>
            <div>
              <label>Task</label>
              <textarea
                rows={5}
                value={task}
                onChange={(e) => setTask(e.target.value)}
                placeholder="Describe what the agent should do…"
              />
            </div>
            <button className="primary" onClick={() => void run()} disabled={running}>
              {running ? "Running…" : "Run"}
            </button>
            {error && <div className="error-text prewrap">{error}</div>}
          </div>
        </div>

        <div className="log" ref={logRef}>
          {lines.length === 0 && <div className="subtle">Output will appear here.</div>}
          {lines.map((l, i) => (
            <div key={i} className={`line ${l.kind}`}>
              {l.text}
            </div>
          ))}
        </div>
      </div>

      {approval && (
        <div className="overlay">
          <div className="modal">
            <h1>Approval required</h1>
            <div className="row" style={{ margin: "8px 0" }}>
              <span className="badge warn">{approval.risk}</span>
              <strong>{approval.tool}</strong>
            </div>
            <div className="subtle prewrap" style={{ marginBottom: 8 }}>
              {approval.reason}
            </div>
            <pre className="mono prewrap" style={{ margin: "0 0 12px" }}>
              {JSON.stringify(approval.args, null, 2)}
            </pre>
            <div className="row">
              <button className="primary" onClick={() => void decide(true)}>
                Allow
              </button>
              <button className="danger" onClick={() => void decide(false)}>
                Deny
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
