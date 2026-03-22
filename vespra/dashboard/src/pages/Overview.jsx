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

  if (loading && !data) return <Loader />;

  const services = data?.services || {};
  const agentList = services.gateway?.data?.agents || [];
  const dagData = services.boiler?.data || {};

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
                <span class="text-vespra-muted">{dagData.version}</span>
              </div>
            )}
          </div>
        </Card>
      </div>

      <Card title="Quick Actions">
        <div class="flex flex-wrap gap-2 text-sm">
          <a href="/agents" class="px-3 py-2 bg-vespra-border rounded hover:bg-vespra-muted/30 transition-colors">
            Chat with Agent
          </a>
          <a href="/pipelines" class="px-3 py-2 bg-vespra-border rounded hover:bg-vespra-muted/30 transition-colors">
            Launch Pipeline
          </a>
          <a href="/wallets" class="px-3 py-2 bg-vespra-border rounded hover:bg-vespra-muted/30 transition-colors">
            View Wallets
          </a>
        </div>
      </Card>
    </div>
  );
}
