import { useCallback, useEffect, useState } from "react";
import { api, errorString } from "../api";
import { textOf, type Session, type StoredMessage } from "../types";

export default function SessionsPage() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [messages, setMessages] = useState<StoredMessage[]>([]);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      setSessions(await api.listSessions());
    } catch (e) {
      setError(errorString(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function open(id: string) {
    setSelected(id);
    setError(null);
    try {
      setMessages(await api.getMessages(id));
    } catch (e) {
      setError(errorString(e));
    }
  }

  async function remove(id: string) {
    try {
      await api.deleteSession(id);
      if (selected === id) {
        setSelected(null);
        setMessages([]);
      }
      await load();
    } catch (e) {
      setError(errorString(e));
    }
  }

  async function rename(s: Session) {
    const title = window.prompt("New title", s.title ?? "");
    if (title == null) return;
    try {
      await api.renameSession(s.id, title);
      await load();
    } catch (e) {
      setError(errorString(e));
    }
  }

  async function togglePin(s: Session) {
    try {
      await api.setSessionPinned(s.id, !s.pinned);
      await load();
    } catch (e) {
      setError(errorString(e));
    }
  }

  return (
    <div>
      <div className="page-head">
        <div>
          <h1>Sessions</h1>
          <div className="subtle">{sessions.length} saved session(s).</div>
        </div>
        <button onClick={load}>Refresh</button>
      </div>

      {error && <div className="card error-text">{error}</div>}

      <div className="grid-2" style={{ alignItems: "start" }}>
        <div>
          {sessions.length === 0 && (
            <div className="card subtle">No sessions yet.</div>
          )}
          {sessions.map((s) => (
            <div
              key={s.id}
              className={`list-item ${selected === s.id ? "active" : ""}`}
            >
              <div
                className="grow"
                style={{ cursor: "pointer", overflow: "hidden" }}
                onClick={() => void open(s.id)}
              >
                <div>
                  {s.pinned ? "★ " : ""}
                  {s.title || "(untitled)"}
                </div>
                <div className="subtle mono">
                  {(s.provider ?? "") + (s.model ? ` · ${s.model}` : "")}
                </div>
              </div>
              <div className="row">
                <button onClick={() => void togglePin(s)}>
                  {s.pinned ? "Unpin" : "Pin"}
                </button>
                <button onClick={() => void rename(s)}>Rename</button>
                <button className="danger" onClick={() => void remove(s.id)}>
                  Delete
                </button>
              </div>
            </div>
          ))}
        </div>

        <div className="card" style={{ maxHeight: "70vh", overflow: "auto" }}>
          <h2>Transcript</h2>
          {!selected && <div className="subtle">Select a session.</div>}
          {messages.map((m) => (
            <div key={m.id} className={`bubble ${m.message.role}`}>
              <div className="meta">
                #{m.seq} · {m.message.role}
              </div>
              {textOf(m.message)}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
