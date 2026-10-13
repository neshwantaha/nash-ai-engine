import { useEffect, useState } from "react";
import { api, errorString } from "../api";
import type { Paths, ValidationReport } from "../types";

export default function DiagnosticsPage() {
  const [paths, setPaths] = useState<Paths | null>(null);
  const [reports, setReports] = useState<ValidationReport[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void (async () => {
      try {
        setPaths(await api.getPaths());
      } catch (e) {
        setError(errorString(e));
      }
    })();
  }, []);

  async function testAll() {
    setBusy(true);
    setError(null);
    try {
      setReports(await api.testAll());
    } catch (e) {
      setError(errorString(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <div className="page-head">
        <div>
          <h1>Diagnostics</h1>
          <div className="subtle">
            Validate connectivity and configuration for every provider.
          </div>
        </div>
        <button className="primary" onClick={() => void testAll()} disabled={busy}>
          {busy ? "Testing…" : "Test all providers"}
        </button>
      </div>

      {error && <div className="card error-text">{error}</div>}

      {paths && (
        <div className="card">
          <h2>Paths</h2>
          <div className="mono">config: {paths.config}</div>
          <div className="mono">db: {paths.db}</div>
        </div>
      )}

      {reports.map((r) => (
        <div className="card" key={r.provider}>
          <div className="card-head">
            <strong>{r.provider}</strong>
            <span className="badge">
              {r.checks.filter((c) => c.status === "pass").length}/
              {r.checks.length} pass
            </span>
          </div>
          <div className="checks" style={{ borderTop: "none", paddingTop: 0 }}>
            {r.checks.map((c, i) => (
              <div className="check" key={i}>
                <span className={`dot ${c.status}`} />
                <span>
                  <strong>{c.name}</strong>{" "}
                  <span className="subtle">{c.status}</span>
                  {c.latency_ms != null && (
                    <span className="subtle"> · {c.latency_ms} ms</span>
                  )}
                  {c.detail && <div className="subtle prewrap">{c.detail}</div>}
                </span>
              </div>
            ))}
          </div>
        </div>
      ))}

      {reports.length === 0 && !error && (
        <div className="card subtle">
          Run a test to see per-provider checks.
        </div>
      )}
    </div>
  );
}
