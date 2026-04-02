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
import { Setup } from "./pages/Setup.jsx";

function SetupBanner() {
  const [dismissed, setDismissed] = useState(false);
  if (dismissed || localStorage.getItem("vespra-setup-complete") || localStorage.getItem("vespra-setup-dismissed")) return null;
  return (
    <div class="bg-vespra-accent/10 border border-vespra-accent/30 rounded-lg px-4 py-3 mb-4 flex items-center justify-between" role="alert">
      <span class="text-sm text-vespra-text">
        First time? <a href="/setup" class="text-vespra-accent underline underline-offset-2 font-medium">Run the setup wizard &rarr;</a>
      </span>
      <button
        onClick={() => { localStorage.setItem("vespra-setup-dismissed", "true"); setDismissed(true); }}
        class="flex items-center justify-center w-8 h-8 text-vespra-muted hover:text-vespra-text text-lg leading-none rounded transition-colors hover:bg-vespra-border/50 shrink-0"
        aria-label="Dismiss setup banner"
      >
        &times;
      </button>
    </div>
  );
}

export function App() {
  const [url, setUrl] = useState("/");

  return (
    <ChainProvider>
      <div class="min-h-screen">
        <a href="#main-content" class="sr-only focus:not-sr-only focus:fixed focus:top-2 focus:left-2 focus:z-[100] focus:px-4 focus:py-2 focus:bg-vespra-accent focus:text-black focus:rounded focus:text-sm focus:font-semibold">
          Skip to main content
        </a>
        <Nav url={url} />
        <main id="main-content" class="max-w-7xl mx-auto px-4 py-6">
          <SetupBanner />
          <Router onChange={(e) => setUrl(e.url)}>
            <Overview path="/" />
            <Agents path="/agents" />
            <Pipelines path="/pipelines" />
            <Wallets path="/wallets" />
            <TxLog path="/txlog" />
            <KillSwitch path="/killswitch" />
            <Settings path="/settings" />
            <Setup path="/setup" />
          </Router>
        </main>
      </div>
    </ChainProvider>
  );
}
