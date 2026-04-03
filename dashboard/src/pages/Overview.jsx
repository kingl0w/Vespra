import { usePolling } from "../hooks/useApi.js";
import { api } from "../lib/api.js";
import { Card, StatusDot, Badge, Loader } from "../components/Card.jsx";

function ServiceCard({ name, data }) {
  if (!data) return null;
  const ok = data.status === "ok";
  return (
    <div class="flex items-center justify-between py-2">
      <div class="flex items-center gap-3">
        <StatusDot status={data.status} />
        <span class="text-sm font-medium capitalize">{name}</span>
      </div>
      <Badge variant={ok ? "green" : "red"}>{data.status}</Badge>
    </div>
  );
}

export function Overview() {
  const { data, loading } = usePolling(() => api.health(), 10000);
  const { data: goalsData } = usePolling(() => api.fetchGoals().catch(() => []), 10000);
  const { data: portfolio } = usePolling(() => api.fetchPortfolio().catch(() => null), 10000);

  if (loading && !data) return <Loader />;

  const services = data?.services || {};
  const agentList = services.gateway?.data?.agents || [];
  const dagData = services.boiler?.data || {};
  const goalsList = Array.isArray(goalsData) ? goalsData : goalsData?.goals || [];
  const activeGoals = goalsList.filter((g) => g.status === "Running");
  const latestRunning = activeGoals.sort((a, b) => (b.updated_at || b.created_at || "").localeCompare(a.updated_at || a.created_at || ""))[0];

  return (
    <div class="space-y-6">
      <h2 class="text-xl font-bold">Overview</h2>

      <div class="grid grid-cols-1 md:grid-cols-3 gap-4">
        <Card title="System Status">
          <div class="flex items-center gap-3 mb-4">
            <StatusDot status={data?.status} />
            <span class="text-2xl font-bold capitalize">{data?.status || "unknown"}</span>
          </div>
          <div class="divide-y divide-vespra-border">
            {Object.entries(services).map(([name, svc]) => (
              <ServiceCard key={name} name={name} data={svc} />
            ))}
          </div>
        </Card>

        <Card title="Agents">
          <div class="text-3xl font-bold text-vespra-accent mb-3">{agentList.length}</div>
          <div class="flex flex-wrap gap-1.5">
            {agentList.map((a) => (
              <Badge key={a} variant="accent">{a}</Badge>
            ))}
          </div>
        </Card>

        <Card title="NullBoiler">
          <div class="space-y-2 text-sm">
            <div class="flex justify-between">
              <span class="text-vespra-muted">Status</span>
              <Badge variant={services.boiler?.status === "ok" ? "green" : "red"}>
                {services.boiler?.status || "unknown"}
              </Badge>
            </div>
            {dagData.workers != null && (
              <div class="flex justify-between">
                <span class="text-vespra-muted">Workers</span>
                <span>{dagData.workers}</span>
              </div>
            )}
            {dagData.version && (
              <div class="flex justify-between">
                <span class="text-vespra-muted">Version</span>
                <span class="font-mono text-vespra-text">{dagData.version}</span>
              </div>
            )}
          </div>
        </Card>

        <Card title="Goals">
          <div class="space-y-2 text-sm">
            <div class="flex justify-between items-center">
              <span class="text-vespra-muted">Active Goals</span>
              <span class="text-lg font-bold text-vespra-accent">{activeGoals.length}</span>
            </div>
            <div class="flex justify-between items-center">
              <span class="text-vespra-muted">Current Step</span>
              <span class="font-mono text-vespra-text">{latestRunning?.current_step || "--"}</span>
            </div>
            {goalsList.length > 0 && (
              <a href="/goals" class="block text-xs text-vespra-accent hover:underline pt-1">
                View all goals &rarr;
              </a>
            )}
          </div>
        </Card>

        {portfolio && (
          <Card title="Portfolio">
            <div class="space-y-2 text-sm">
              <div class="flex justify-between items-center">
                <span class="text-vespra-muted">Total Capital</span>
                <span class="font-mono text-vespra-text">{portfolio.total_capital_eth != null ? `${portfolio.total_capital_eth} ETH` : "--"}</span>
              </div>
              <div class="flex justify-between items-center">
                <span class="text-vespra-muted">Total P&L</span>
                <span class={`font-mono ${(portfolio.total_pnl_eth ?? 0) >= 0 ? "text-vespra-green" : "text-vespra-red"}`}>
                  {portfolio.total_pnl_eth != null
                    ? `${portfolio.total_pnl_eth >= 0 ? "+" : ""}${portfolio.total_pnl_eth} ETH`
                    : "--"}
                </span>
              </div>
            </div>
          </Card>
        )}
      </div>

      <Card title="Quick Actions">
        <div class="flex flex-wrap gap-2 text-sm">
          <a href="/agents" class="px-4 py-2.5 min-h-[44px] inline-flex items-center bg-vespra-border rounded hover:bg-vespra-muted/30 transition-colors">
            Chat with Agent
          </a>
          <a href="/pipelines" class="px-4 py-2.5 min-h-[44px] inline-flex items-center bg-vespra-border rounded hover:bg-vespra-muted/30 transition-colors">
            Launch Pipeline
          </a>
          <a href="/wallets" class="px-4 py-2.5 min-h-[44px] inline-flex items-center bg-vespra-border rounded hover:bg-vespra-muted/30 transition-colors">
            View Wallets
          </a>
        </div>
      </Card>
    </div>
  );
}
