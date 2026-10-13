import { useCallback, useEffect, useState } from "react";
import { api, errorString } from "../api";
import type { ModelInfo, ProviderSummary, ValidationReport } from "../types";

export default function ProvidersPage() {
  const [providers, setProviders] = useState<ProviderSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [models, setModels] = useState<Record<string, ModelInfo[]>>({});
  const [reports, setReports] = useState<Record<string, ValidationReport>>({});
  const [busy, setBusy] = useState<Record<string, string | null>>({});
  const [keys, setKeys] = useState<Record<string, string>>({});
  const [keySaved, setKeySaved] = useState<Record<string, boolean>>({});

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setProviders(await api.listProviders());
    } catch (e) {
      setError(errorString(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function runModels(id: string) {
    setBusy((b) => ({ ...b, [id]: "models" }));
    try {
      const list = await api.listModels(id);
      setModels((m) => ({ ...m, [id]: list }));
    } catch (e) {
      setError(errorString(e));
    } finally {
      setBusy((b) => ({ ...b, [id]: null }));
    }
  }

  async function runTest(id: string) {
    setBusy((b) => ({ ...b, [id]: "test" }));
    try {
      const report = await api.testProvider(id);
      setReports((r) => ({ ...r, [id]: report }));
    } catch (e) {
      setError(errorString(e));
    } finally {
      setBusy((b) => ({ ...b, [id]: null }));
    }
  }

  async function saveKey(id: string) {
    const value = (keys[id] ?? "").trim();
    if (!value) return;
    setError(null);
    try {
      await api.setProviderKey(id, value);
      setKeys((k) => ({ ...k, [id]: "" }));
      setKeySaved((s) => ({ ...s, [id]: true }));
      await load();
    } catch (e) {
      setError(errorString(e));
    }
  }

  return (
    <div>
      <div className="page-head">
        <div>
          <h1>Providers</h1>
          <div className="subtle">
            {providers.length} configured. Edit the JSON in Settings to add or
            change providers.
          </div>
        </div>
        <button onClick={load} disabled={loading}>
          {loading ? "Refreshing…" : "Refresh"}
        </button>
      </div>

      {error && <div className="card error-text">{error}</div>}

      {providers.map((p) => {
        const report = reports[p.id];
        const busyKind = busy[p.id];
        return (
          <div className="card" key={p.id}>
            <div className="card-head">
              <div className="row wrap">
                <strong>{p.displayName || p.id}</strong>
                <span className="badge">{p.protocol}</span>
                {p.local && <span className="badge">local</span>}
                {!p.enabled && <span className="badge warn">disabled</span>}
                {p.built ? (
                  <span className="badge good">ready</span>
                ) : (
                  <span className="badge bad">not built</span>
                )}
                {p.hasSecret ? (
                  <span className="badge good">key set</span>
                ) : p.secretRef ? (
                  <span className="badge warn">no key</span>
                ) : null}
              </div>
              <div className="row">
                <button
                  onClick={() => runModels(p.id)}
                  disabled={busyKind != null}
                >
                  {busyKind === "models" ? "Loading…" : "Models"}
                </button>
                <button
                  className="primary"
                  onClick={() => runTest(p.id)}
                  disabled={busyKind != null}
                >
                  {busyKind === "test" ? "Testing…" : "Test"}
                </button>
              </div>
            </div>

            <div className="mono subtle">{p.baseUrl}</div>
            {p.model && <div className="subtle">default model: {p.model}</div>}
            {p.buildError && (
              <div className="error-text mono">{p.buildError}</div>
            )}

            {p.secretRef && (
              <div className="row" style={{ marginTop: 8 }}>
                <input
                  type="password"
                  style={{ flex: 1 }}
                  placeholder={`Paste API key → stored as ${p.secretRef}`}
                  value={keys[p.id] ?? ""}
                  onChange={(e) =>
                    setKeys((k) => ({ ...k, [p.id]: e.target.value }))
                  }
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void saveKey(p.id);
                  }}
                />
                <button
                  className="primary"
                  onClick={() => void saveKey(p.id)}
                  disabled={!(keys[p.id] ?? "").trim()}
                >
                  Save key
                </button>
                {keySaved[p.id] && <span className="subtle">stored</span>}
              </div>
            )}

            {report && (
              <div className="checks">
                {report.checks.map((c, i) => (
                  <div className="check" key={i}>
                    <span className={`dot ${c.status}`} />
                    <span>
                      <strong>{c.name}</strong>{" "}
                      <span className="subtle">{c.status}</span>
                      {c.latency_ms != null && (
                        <span className="subtle"> · {c.latency_ms} ms</span>
                      )}
                      {c.detail && (
                        <div className="subtle prewrap">{c.detail}</div>
                      )}
                    </span>
                  </div>
                ))}
              </div>
            )}

            {models[p.id] && (
              <div className="checks">
                <div className="subtle">
                  {models[p.id].length} model(s)
                  {models[p.id].length > 0 && (
                    <>
                      :{" "}
                      <span className="mono">
                        {models[p.id]
                          .slice(0, 12)
                          .map((m) => m.id)
                          .join(", ")}
                        {models[p.id].length > 12 ? ", …" : ""}
                      </span>
                    </>
                  )}
                </div>
              </div>
            )}
          </div>
        );
      })}

      {!loading && providers.length === 0 && !error && (
        <div className="card subtle">
          No providers configured. Open Settings and add one to the JSON.
        </div>
      )}
    </div>
  );
}
