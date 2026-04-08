import { useState, useEffect } from "preact/hooks";
import { useChain } from "../hooks/useChain.jsx";

const LINKS = [
  { href: "/", label: "Overview" },
  { href: "/agents", label: "Agents" },
  { href: "/pipelines", label: "Pipelines" },
  { href: "/goals", label: "Goals" },
  { href: "/backtest", label: "Backtest" },
  { href: "/wallets", label: "Wallets" },
  { href: "/txlog", label: "TX Log" },
  { href: "/killswitch", label: "Kill Switch" },
  { href: "/settings", label: "Settings" },
  { href: "/setup", label: "\u2699 Setup" },
];

function isActive(url, href) {
  return url === href || (href !== "/" && url?.startsWith(href));
}

export function Nav({ url }) {
  const { chain, setChain, chains } = useChain();
  const [open, setOpen] = useState(false);

  // Close drawer on route change
  useEffect(() => {
    setOpen(false);
  }, [url]);

  // Prevent body scroll when drawer is open
  useEffect(() => {
    if (open) {
      document.body.style.overflow = "hidden";
      return () => { document.body.style.overflow = ""; };
    }
  }, [open]);

  return (
    <nav class="border-b border-vespra-border bg-vespra-surface/80 backdrop-blur sticky top-0 z-50" aria-label="Main navigation">
      <div class="max-w-7xl mx-auto px-4 h-14 flex items-center justify-between">
        <div class="flex items-center gap-3">
          <button
            onClick={() => setOpen(!open)}
            class="lg:hidden flex items-center justify-center w-10 h-10 -ml-2 rounded text-vespra-muted hover:text-vespra-text hover:bg-vespra-border/50 transition-colors"
            aria-label={open ? "Close menu" : "Open menu"}
            aria-expanded={open}
          >
            {open ? (
              <svg class="w-5 h-5" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true">
                <path d="M4.293 4.293a1 1 0 011.414 0L10 8.586l4.293-4.293a1 1 0 111.414 1.414L11.414 10l4.293 4.293a1 1 0 01-1.414 1.414L10 11.414l-4.293 4.293a1 1 0 01-1.414-1.414L8.586 10 4.293 5.707a1 1 0 010-1.414z" />
              </svg>
            ) : (
              <svg class="w-5 h-5" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true">
                <path fill-rule="evenodd" d="M3 5a1 1 0 011-1h12a1 1 0 110 2H4a1 1 0 01-1-1zm0 5a1 1 0 011-1h12a1 1 0 110 2H4a1 1 0 01-1-1zm0 5a1 1 0 011-1h12a1 1 0 110 2H4a1 1 0 01-1-1z" clip-rule="evenodd" />
              </svg>
            )}
          </button>
          <h1 class="text-vespra-accent font-bold text-lg tracking-tight">VESPRA PROTOCOL</h1>
        </div>

        {/* Desktop nav */}
        <div class="hidden lg:flex items-center gap-1">
          {LINKS.map((l) => (
            <a
              key={l.href}
              href={l.href}
              aria-current={isActive(url, l.href) ? "page" : undefined}
              class={`px-3 py-2 rounded text-sm transition-colors ${
                isActive(url, l.href)
                  ? "bg-vespra-accent/15 text-vespra-accent"
                  : "text-vespra-muted hover:text-vespra-text hover:bg-vespra-border/50"
              } ${l.label === "Kill Switch" ? "text-vespra-red hover:bg-vespra-red/10" : ""}`}
            >
              {l.label}
            </a>
          ))}
          <div class="ml-3 pl-3 border-l border-vespra-border">
            <select
              value={chain}
              onChange={(e) => setChain(e.target.value)}
              aria-label="Select chain"
              class="bg-vespra-bg border border-vespra-border rounded px-2 py-1.5 text-xs text-vespra-text cursor-pointer"
            >
              {chains.map((c) => (
                <option key={c.id} value={c.id}>{c.label}</option>
              ))}
            </select>
          </div>
        </div>

        {/* Mobile chain selector (always visible) */}
        <div class="lg:hidden">
          <select
            value={chain}
            onChange={(e) => setChain(e.target.value)}
            aria-label="Select chain"
            class="bg-vespra-bg border border-vespra-border rounded px-2 py-1.5 text-xs text-vespra-text cursor-pointer"
          >
            {chains.map((c) => (
              <option key={c.id} value={c.id}>{c.label}</option>
            ))}
          </select>
        </div>
      </div>

      {/* Mobile drawer */}
      {open && (
        <>
          <div
            class="fixed inset-0 top-14 bg-black/60 z-40 lg:hidden"
            onClick={() => setOpen(false)}
            aria-hidden="true"
          />
          <div class="fixed top-14 left-0 bottom-0 w-64 bg-vespra-surface border-r border-vespra-border z-50 lg:hidden overflow-y-auto" role="menu">
            <div class="py-2 px-2 space-y-1">
              {LINKS.map((l) => (
                <a
                  key={l.href}
                  href={l.href}
                  role="menuitem"
                  aria-current={isActive(url, l.href) ? "page" : undefined}
                  class={`block px-4 py-3 rounded text-sm transition-colors ${
                    isActive(url, l.href)
                      ? "bg-vespra-accent/15 text-vespra-accent"
                      : "text-vespra-muted hover:text-vespra-text hover:bg-vespra-border/50"
                  } ${l.label === "Kill Switch" ? "text-vespra-red hover:bg-vespra-red/10" : ""}`}
                >
                  {l.label}
                </a>
              ))}
            </div>
          </div>
        </>
      )}
    </nav>
  );
}
