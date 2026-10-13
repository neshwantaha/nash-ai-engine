import { useState } from "react";
import ProvidersPage from "./pages/ProvidersPage";
import ChatPage from "./pages/ChatPage";
import AgentPage from "./pages/AgentPage";
import SecretsPage from "./pages/SecretsPage";
import SessionsPage from "./pages/SessionsPage";
import DiagnosticsPage from "./pages/DiagnosticsPage";
import SettingsPage from "./pages/SettingsPage";

type Tab =
  | "chat"
  | "agent"
  | "providers"
  | "diagnostics"
  | "sessions"
  | "secrets"
  | "settings";

const NAV: { id: Tab; label: string }[] = [
  { id: "chat", label: "Chat" },
  { id: "agent", label: "Agent" },
  { id: "providers", label: "Providers" },
  { id: "diagnostics", label: "Diagnostics" },
  { id: "sessions", label: "Sessions" },
  { id: "secrets", label: "Secrets" },
  { id: "settings", label: "Settings" },
];

export default function App() {
  const [tab, setTab] = useState<Tab>("chat");

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          Nash<span>AI</span>
        </div>
        {NAV.map((item) => (
          <button
            key={item.id}
            className={`nav-item ${tab === item.id ? "active" : ""}`}
            onClick={() => setTab(item.id)}
          >
            {item.label}
          </button>
        ))}
      </aside>
      <main className="content">
        {tab === "chat" && <ChatPage />}
        {tab === "agent" && <AgentPage />}
        {tab === "providers" && <ProvidersPage />}
        {tab === "diagnostics" && <DiagnosticsPage />}
        {tab === "sessions" && <SessionsPage />}
        {tab === "secrets" && <SecretsPage />}
        {tab === "settings" && <SettingsPage />}
      </main>
    </div>
  );
}
