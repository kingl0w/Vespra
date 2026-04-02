export function Card({ title, children, className = "", actions }) {
  return (
    <div class={`bg-vespra-surface border border-vespra-border rounded-lg ${className}`}>
      {(title || actions) && (
        <div class="flex items-center justify-between px-4 py-3 border-b border-vespra-border">
          {title && <h3 class="text-sm font-semibold text-vespra-text">{title}</h3>}
          {actions && <div class="flex gap-2">{actions}</div>}
        </div>
      )}
      <div class="p-4">{children}</div>
    </div>
  );
}

export function StatusDot({ status }) {
  const color =
    status === "ok" || status === "active" || status === "healthy"
      ? "bg-vespra-green"
      : status === "degraded" || status === "warning"
      ? "bg-vespra-yellow"
      : "bg-vespra-red";
  return (
    <span class={`inline-block w-2 h-2 rounded-full ${color}`} role="img" aria-label={status || "unknown"} />
  );
}

export function Badge({ children, variant = "default" }) {
  const styles = {
    default: "bg-vespra-border text-vespra-muted",
    accent: "bg-vespra-accent/15 text-vespra-accent",
    green: "bg-vespra-green/15 text-vespra-green",
    red: "bg-vespra-red/15 text-vespra-red",
    yellow: "bg-vespra-yellow/15 text-vespra-yellow",
    orange: "bg-vespra-orange/15 text-vespra-orange",
  };
  return (
    <span class={`inline-block px-2 py-0.5 rounded text-xs font-medium ${styles[variant] || styles.default}`}>
      {children}
    </span>
  );
}

export function Button({ children, onClick, variant = "default", disabled, className = "" }) {
  const styles = {
    default: "bg-vespra-border hover:bg-vespra-muted/30 text-vespra-text",
    accent: "bg-vespra-accent hover:bg-vespra-accent-dim text-black font-semibold",
    danger: "bg-vespra-red/20 hover:bg-vespra-red/30 text-vespra-red border border-vespra-red/30",
    ghost: "hover:bg-vespra-border text-vespra-muted hover:text-vespra-text",
  };
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      class={`px-4 py-2.5 min-h-[44px] rounded text-sm font-medium transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${styles[variant]} ${className}`}
    >
      {children}
    </button>
  );
}

export function Loader() {
  return (
    <div class="flex items-center justify-center py-12" role="status">
      <div class="w-6 h-6 border-2 border-vespra-accent border-t-transparent rounded-full animate-spin" />
      <span class="sr-only">Loading...</span>
    </div>
  );
}
