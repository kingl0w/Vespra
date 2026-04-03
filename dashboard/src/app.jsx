import { useState, lazy, Suspense } from "preact/compat";
import Router from "preact-router";
import { ChainProvider } from "./hooks/useChain.jsx";
import { Nav } from "./components/Nav.jsx";
import { Loader } from "./components/Card.jsx";
import { Overview } from "./pages/Overview.jsx";

const Agents = lazy(() => import("./pages/Agents.jsx").then(m => ({ default: m.Agents })));
const Pipelines = lazy(() => import("./pages/Pipelines.jsx").then(m => ({ default: m.Pipelines })));
const Goals = lazy(() => import("./pages/Goals.jsx").then(m => ({ default: m.Goals })));
const Wallets = lazy(() => import("./pages/Wallets.jsx").then(m => ({ default: m.Wallets })));
const TxLog = lazy(() => import("./pages/TxLog.jsx").then(m => ({ default: m.TxLog })));
const KillSwitch = lazy(() => import("./pages/KillSwitch.jsx").then(m => ({ default: m.KillSwitch })));
const Settings = lazy(() => import("./pages/Settings.jsx").then(m => ({ default: m.Settings })));
const Setup = lazy(() => import("./pages/Setup.jsx").then(m => ({ default: m.Setup })));

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
          <Suspense fallback={<Loader />}>
            <Router onChange={(e) => setUrl(e.url)}>
              <Overview path="/" />
              <Agents path="/agents" />
              <Pipelines path="/pipelines" />
              <Goals path="/goals" />
              <Wallets path="/wallets" />
              <TxLog path="/txlog" />
              <KillSwitch path="/killswitch" />
              <Settings path="/settings" />
              <Setup path="/setup" />
            </Router>
          </Suspense>
        </main>
      </div>
    </ChainProvider>
  );
}
