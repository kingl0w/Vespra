import { useChain } from "../hooks/useChain.jsx";

const LINKS = [
  { href: "/", label: "Overview" },
  { href: "/agents", label: "Agents" },
  { href: "/pipelines", label: "Pipelines" },
  { href: "/wallets", label: "Wallets" },
  { href: "/txlog", label: "TX Log" },
  { href: "/killswitch", label: "Kill Switch" },
  { href: "/settings", label: "Settings" },
  { href: "/setup", label: "\u2699 Setup" },
];

export function Nav({ url }) {
  const { chain, setChain, chains } = useChain();

  return (
    <nav class="border-b border-vespra-border bg-vespra-surface/80 backdrop-blur sticky top-0 z-50">
      <div class="max-w-7xl mx-auto px-4 h-14 flex items-center justify-between">
        <div class="flex items-center">
          <span class="text-vespra-accent font-bold text-lg tracking-tight">VESPRA PROTOCOL</span>
        </div>
        <div class="flex items-center gap-1">
          {LINKS.map((l) => (
            <a
              key={l.href}
              href={l.href}
              class={`px-3 py-1.5 rounded text-sm transition-colors ${
                url === l.href || (l.href !== "/" && url?.startsWith(l.href))
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
              class="bg-vespra-bg border border-vespra-border rounded px-2 py-1 text-xs text-vespra-text focus:outline-none focus:border-vespra-accent cursor-pointer"
            >
              {chains.map((c) => (
                <option key={c.id} value={c.id}>{c.label}</option>
              ))}
            </select>
          </div>
        </div>
      </div>
    </nav>
  );
}
