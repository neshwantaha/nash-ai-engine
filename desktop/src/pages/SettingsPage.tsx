import { useEffect, useState } from "react";
import { api, errorString } from "../api";

export default function SettingsPage() {
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [paths, setPaths] = useState<{ config: string; db: string } | null>(null);

  useEffect(() => {
    void (async () => {
      try {
        const [cfg, p] = await Promise.all([api.getConfig(), api.getPaths()]);
        setText(JSON.stringify(cfg, null, 2));
        setPaths(p);
      } catch (e) {
        setError(errorString(e));
      }
    })();
  }, []);

  async function save() {
    setError(null);
    setStatus(null);
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch (e) {
      setError(`invalid JSON: ${errorString(e)}`);
      return;
    }
    try {
      await api.saveConfig(parsed);
      setStatus("saved and validated");
    } catch (e) {
      setError(errorString(e));
    }
  }

  async function reload() {
    setError(null);
    setStatus(null);
    try {
      setText(JSON.stringify(await api.getConfig(), null, 2));
    } catch (e) {
      setError(errorString(e));
    }
  }

  return (
    <div>
      <div className="page-head">
        <div>
          <h1>Settings</h1>
          <div className="subtle">
            Edit the raw configuration. Secrets stay in the keychain; reference
            them here by <span className="mono">secret_ref</span>.
          </div>
        </div>
        <div className="row">
          <button onClick={reload}>Reload</button>
          <button className="primary" onClick={() => void save()}>
            Save
          </button>
        </div>
      </div>

      {paths && (
        <div className="card">
          <div className="mono subtle">config: {paths.config}</div>
          <div className="mono subtle">db: {paths.db}</div>
        </div>
      )}

      {error && <div className="card error-text prewrap">{error}</div>}
      {status && <div className="card subtle">{status}</div>}

      <textarea
        style={{ height: "60vh" }}
        value={text}
        spellCheck={false}
        onChange={(e) => setText(e.target.value)}
      />
    </div>
  );
}
