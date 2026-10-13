import { useCallback, useEffect, useState } from "react";
import { api, errorString } from "../api";

export default function SecretsPage() {
  const [names, setNames] = useState<string[]>([]);
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      setNames(await api.listSecrets());
    } catch (e) {
      setError(errorString(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function save() {
    setError(null);
    setStatus(null);
    if (!name.trim() || !value) {
      setError("name and value are required");
      return;
    }
    try {
      await api.setSecret(name.trim(), value);
      setStatus(`stored '${name.trim()}'`);
      setName("");
      setValue("");
      await load();
    } catch (e) {
      setError(errorString(e));
    }
  }

  async function remove(n: string) {
    setError(null);
    setStatus(null);
    try {
      await api.removeSecret(n);
      await load();
    } catch (e) {
      setError(errorString(e));
    }
  }

  return (
    <div>
      <div className="page-head">
        <div>
          <h1>Secrets</h1>
          <div className="subtle">
            Credentials are stored in the OS keychain and referenced by name from
            provider <span className="mono">secret_ref</span>. Values are never
            written to config.
          </div>
        </div>
        <button onClick={load}>Refresh</button>
      </div>

      <div className="card">
        <h2>Add / update</h2>
        <div className="grid-2">
          <div>
            <label>Name (secret_ref)</label>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="openai_api_key"
            />
          </div>
          <div>
            <label>Value</label>
            <input
              type="password"
              value={value}
              onChange={(e) => setValue(e.target.value)}
            />
          </div>
        </div>
        <div className="row" style={{ marginTop: 10 }}>
          <button className="primary" onClick={() => void save()}>
            Save secret
          </button>
          {status && <span className="subtle">{status}</span>}
        </div>
        {error && <div className="error-text" style={{ marginTop: 8 }}>{error}</div>}
      </div>

      <div className="card">
        <h2>Stored secrets</h2>
        {names.length === 0 && <div className="subtle">No secrets stored.</div>}
        {names.map((n) => (
          <div className="list-item" key={n}>
            <span className="mono">{n}</span>
            <button className="danger" onClick={() => void remove(n)}>
              Remove
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}
