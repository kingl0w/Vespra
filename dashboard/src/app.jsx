import { useState } from "preact/hooks";
import Router from "preact-router";
import { ChainProvider } from "./hooks/useChain.jsx";
import { Nav } from "./components/Nav.jsx";
import { Overview } from "./pages/Overview.jsx";
import { Agents } from "./pages/Agents.jsx";
import { Pipelines } from "./pages/Pipelines.jsx";
import { Wallets } from "./pages/Wallets.jsx";
import { TxLog } from "./pages/TxLog.jsx";
import { KillSwitch } from "./pages/KillSwitch.jsx";
import { Settings } from "./pages/Settings.jsx";

export function App() {
  const [url, setUrl] = useState("/");

  return (
    <ChainProvider>
      <div class="min-h-screen">
        <Nav url={url} />
        <main class="max-w-7xl mx-auto px-4 py-6">
          <Router onChange={(e) => setUrl(e.url)}>
            <Overview path="/" />
            <Agents path="/agents" />
            <Pipelines path="/pipelines" />
            <Wallets path="/wallets" />
            <TxLog path="/txlog" />
            <KillSwitch path="/killswitch" />
            <Settings path="/settings" />
          </Router>
        </main>
      </div>
    </ChainProvider>
  );
}
