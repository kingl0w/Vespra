import { useState, useEffect } from "preact/hooks";
import { Card, Button, Badge, Loader } from "../components/Card.jsx";

const BASE_GW = import.meta.env.MODE === "production"
  ? "https://api.vespra.xyz"
  : "http://127.0.0.1:9001";

const PROVIDER_DEFAULTS = {
  deepseek: { model: "deepseek-chat", url: "https://api.deepseek.com" },
  openai: { model: "gpt-4o", url: "https://api.openai.com/v1" },
  anthropic: { model: "claude-sonnet-4-20250514", url: "https://api.anthropic.com" },
};

const ALL_CHAINS = ["base", "arbitrum", "optimism", "ethereum"];
const ORACLES = ["defillama", "coingecko"];
const STORAGE_KEY = "vespra-setup-wizard";
const COMPLETE_KEY = "vespra-setup-complete";

function StepIndicator({ step }) {
  return (
    <div class="flex items-center gap-1.5 sm:gap-2 mb-6" role="progressbar" aria-valuenow={step} aria-valuemin={1} aria-valuemax={5} aria-label={`Step ${step} of 5`}>
      {[1, 2, 3, 4, 5].map((s) => (
        <div key={s} class="flex items-center gap-1.5 sm:gap-2">
          <div
            class={`w-7 h-7 sm:w-8 sm:h-8 rounded-full flex items-center justify-center text-xs sm:text-sm font-bold shrink-0 ${
              s === step
                ? "bg-vespra-accent text-black"
                : s < step
                ? "bg-vespra-green/20 text-vespra-green"
                : "bg-vespra-border text-vespra-muted"
            }`}
          >
            {s < step ? "\u2713" : s}
          </div>
          {s < 5 && <div class={`w-4 sm:w-8 h-0.5 ${s < step ? "bg-vespra-green/40" : "bg-vespra-border"}`} />}
        </div>
      ))}
      <span class="ml-2 sm:ml-3 text-xs sm:text-sm text-vespra-muted whitespace-nowrap">Step {step} of 5</span>
    </div>
  );
}

function Toggle({ checked, onChange, label }) {
  return (
    <label class="flex items-center gap-3 cursor-pointer select-none">
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        class={`relative w-10 h-5 rounded-full transition-colors ${checked ? "bg-vespra-accent" : "bg-vespra-border"}`}
      >
        <span
          class={`absolute top-0.5 left-0.5 w-4 h-4 rounded-full bg-vespra-text transition-transform ${checked ? "translate-x-5" : ""}`}
        />
      </button>
      <span class="text-sm text-vespra-text">{label}</span>
    </label>
  );
}

function FieldLabel({ children }) {
  return <label class="text-xs text-vespra-muted block mb-1">{children}</label>;
}

function TextInput({ value, onChange, placeholder, type = "text", step, min }) {
  return (
    <input
      type={type}
      value={value}
      onInput={(e) => onChange(type === "number" ? (e.target.value === "" ? "" : Number(e.target.value)) : e.target.value)}
      placeholder={placeholder}
      step={step}
      min={min}
      class="bg-vespra-bg border border-vespra-border rounded px-3 py-2.5 min-h-[44px] text-sm text-vespra-text placeholder:text-vespra-muted focus:border-vespra-accent w-full"
    />
  );
}

export function Setup() {
  const [step, setStep] = useState(1);
  const [loading, setLoading] = useState(true);
  const [original, setOriginal] = useState(null);
  const [config, setConfig] = useState(null);
  const [saving, setSaving] = useState(false);
  const [saveResult, setSaveResult] = useState(null);
  const [validationError, setValidationError] = useState(null);

  //load config from api, then overlay any saved wizard state
  useEffect(() => {
    fetch(`${BASE_GW}/config`)
      .then((r) => r.json())
      .then((data) => {
        setOriginal(data);
        const saved = localStorage.getItem(STORAGE_KEY);
        if (saved) {
          try {
            const partial = JSON.parse(saved);
            setConfig({ ...data, ...partial });
            if (partial._step) setStep(partial._step);
          } catch {
            setConfig({ ...data });
          }
        } else {
          setConfig({ ...data });
        }
      })
      .catch(() => {
        //start with empty defaults if api is down
        const defaults = {
          llm_provider: "deepseek",
          llm_model: "deepseek-chat",
          llm_base_url: "https://api.deepseek.com",
          chains: ["base", "arbitrum"],
          default_custody: "safe",
          trade_up_enabled: true,
          trade_up_max_eth: 0.02,
          trade_up_min_gain_pct: 0.3,
          trade_up_stop_loss_pct: 10.0,
          trade_up_cycle_interval_secs: 300,
          auto_execute_enabled: false,
          auto_execute_max_eth: 0.05,
          yield_auto_rotate_enabled: false,
          price_oracle: "defillama",
          price_oracle_fallback: "coingecko",
        };
        setOriginal(defaults);
        setConfig({ ...defaults });
      })
      .finally(() => setLoading(false));
  }, []);

  //persist wizard state on every change
  useEffect(() => {
    if (!config) return;
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...config, _step: step }));
  }, [config, step]);

  const update = (key, value) => {
    setConfig((c) => ({ ...c, [key]: value }));
    setValidationError(null);
  };

  const setProvider = (provider) => {
    const defaults = PROVIDER_DEFAULTS[provider];
    setConfig((c) => ({
      ...c,
      llm_provider: provider,
      llm_model: defaults.model,
      llm_base_url: defaults.url,
    }));
  };

  const toggleChain = (chain) => {
    setConfig((c) => {
      const chains = c.chains || [];
      return {
        ...c,
        chains: chains.includes(chain) ? chains.filter((x) => x !== chain) : [...chains, chain],
      };
    });
    setValidationError(null);
  };

  const validateStep = () => {
    if (step === 2 && (!config.chains || config.chains.length === 0)) {
      setValidationError("Select at least one chain.");
      return false;
    }
    if (step === 3) {
      if (config.trade_up_enabled && (!config.trade_up_max_eth || config.trade_up_max_eth <= 0)) {
        setValidationError("Trade-up max ETH must be > 0.");
        return false;
      }
      if (config.auto_execute_enabled && (!config.auto_execute_max_eth || config.auto_execute_max_eth <= 0)) {
        setValidationError("Auto-execute max ETH must be > 0.");
        return false;
      }
    }
    setValidationError(null);
    return true;
  };

  const next = () => {
    if (validateStep()) setStep((s) => Math.min(s + 1, 5));
  };
  const back = () => setStep((s) => Math.max(s - 1, 1));

  const getChangedFields = () => {
    if (!original || !config) return {};
    const diff = {};
    for (const key of Object.keys(config)) {
      if (key === "_step") continue;
      const o = original[key];
      const n = config[key];
      if (Array.isArray(o) && Array.isArray(n)) {
        if (JSON.stringify([...o].sort()) !== JSON.stringify([...n].sort())) diff[key] = n;
      } else if (o !== n) {
        diff[key] = n;
      }
    }
    return diff;
  };

  const save = async () => {
    const changed = getChangedFields();
    if (Object.keys(changed).length === 0) {
      setSaveResult({ ok: true, msg: "No changes to save." });
      return;
    }
    setSaving(true);
    setSaveResult(null);
    try {
      const res = await fetch(`${BASE_GW}/config`, {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(changed),
      });
      if (!res.ok) {
        const err = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
        throw err;
      }
      localStorage.setItem(COMPLETE_KEY, "true");
      localStorage.removeItem(STORAGE_KEY);
      setSaveResult({ ok: true, msg: "Configuration saved." });
    } catch (err) {
      setSaveResult({ ok: false, msg: err.error || err.message || "Save failed" });
    } finally {
      setSaving(false);
    }
  };

  if (loading || !config) return <Loader />;

  return (
    <div class="space-y-6 max-w-2xl mx-auto">
      <h2 class="text-xl font-bold">Setup Wizard</h2>
      <StepIndicator step={step} />

      {/* Step 1: LLM Provider */}
      {step === 1 && (
        <Card title="LLM Provider">
          <div class="space-y-4">
            <div>
              <FieldLabel>Provider</FieldLabel>
              <div class="flex gap-4">
                {["deepseek", "openai", "anthropic"].map((p) => (
                  <label key={p} class="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="llm_provider"
                      checked={config.llm_provider === p}
                      onChange={() => setProvider(p)}
                      class="accent-vespra-accent"
                    />
                    <span class="text-sm text-vespra-text capitalize">{p}</span>
                  </label>
                ))}
              </div>
            </div>
            <div>
              <FieldLabel>Model</FieldLabel>
              <TextInput value={config.llm_model || ""} onChange={(v) => update("llm_model", v)} />
            </div>
            <div>
              <FieldLabel>Base URL</FieldLabel>
              <TextInput value={config.llm_base_url || ""} onChange={(v) => update("llm_base_url", v)} />
            </div>
          </div>
        </Card>
      )}

      {/* Step 2: Chains & Custody */}
      {step === 2 && (
        <Card title="Chains & Custody">
          <div class="space-y-4">
            <div>
              <FieldLabel>Active Chains</FieldLabel>
              <div class="flex gap-4">
                {ALL_CHAINS.map((c) => (
                  <label key={c} class="flex items-center gap-2 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={(config.chains || []).includes(c)}
                      onChange={() => toggleChain(c)}
                      class="accent-vespra-accent"
                    />
                    <span class="text-sm text-vespra-text">{c}</span>
                  </label>
                ))}
              </div>
            </div>
            <div>
              <FieldLabel>Default Custody</FieldLabel>
              <div class="flex gap-4">
                {[
                  { id: "safe", label: "Gnosis Safe multi-sig (recommended)" },
                  { id: "operator", label: "Keymaster EOA wallet (simpler)" },
                ].map((opt) => (
                  <label key={opt.id} class="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="custody"
                      checked={config.default_custody === opt.id}
                      onChange={() => update("default_custody", opt.id)}
                      class="accent-vespra-accent"
                    />
                    <span class="text-sm text-vespra-text">{opt.label}</span>
                  </label>
                ))}
              </div>
            </div>
          </div>
        </Card>
      )}

      {/* Step 3: Trade-Up Strategy */}
      {step === 3 && (
        <Card title="Trade-Up Strategy">
          <div class="space-y-4">
            <Toggle
              checked={config.trade_up_enabled}
              onChange={(v) => update("trade_up_enabled", v)}
              label="Trade-Up Enabled"
            />
            <div class="grid grid-cols-2 gap-4">
              <div>
                <FieldLabel>Max ETH per trade</FieldLabel>
                <TextInput
                  type="number"
                  step="0.001"
                  min="0"
                  value={config.trade_up_max_eth}
                  onChange={(v) => update("trade_up_max_eth", v)}
                />
              </div>
              <div>
                <FieldLabel>Min Gain %</FieldLabel>
                <TextInput
                  type="number"
                  step="0.1"
                  value={config.trade_up_min_gain_pct}
                  onChange={(v) => update("trade_up_min_gain_pct", v)}
                />
              </div>
              <div>
                <FieldLabel>Stop Loss %</FieldLabel>
                <TextInput
                  type="number"
                  step="0.5"
                  value={config.trade_up_stop_loss_pct}
                  onChange={(v) => update("trade_up_stop_loss_pct", v)}
                />
              </div>
              <div>
                <FieldLabel>Cycle Interval (minutes)</FieldLabel>
                <TextInput
                  type="number"
                  step="1"
                  min="1"
                  value={config.trade_up_cycle_interval_secs ? config.trade_up_cycle_interval_secs / 60 : ""}
                  onChange={(v) => update("trade_up_cycle_interval_secs", v === "" ? "" : v * 60)}
                />
              </div>
            </div>
            <div class="border-t border-vespra-border pt-4">
              <Toggle
                checked={config.auto_execute_enabled}
                onChange={(v) => update("auto_execute_enabled", v)}
                label="Auto-Execute"
              />
              {config.auto_execute_enabled && (
                <div class="mt-3 w-1/2">
                  <FieldLabel>Max ETH per auto-execution</FieldLabel>
                  <TextInput
                    type="number"
                    step="0.01"
                    min="0"
                    value={config.auto_execute_max_eth}
                    onChange={(v) => update("auto_execute_max_eth", v)}
                  />
                </div>
              )}
            </div>
          </div>
        </Card>
      )}

      {/* Step 4: Yield & Oracles */}
      {step === 4 && (
        <Card title="Yield & Oracles">
          <div class="space-y-4">
            <Toggle
              checked={config.yield_auto_rotate_enabled}
              onChange={(v) => update("yield_auto_rotate_enabled", v)}
              label="Auto-Rotate Yield"
            />
            <div class="grid grid-cols-2 gap-4">
              <div>
                <FieldLabel>Primary Oracle</FieldLabel>
                <select
                  value={config.price_oracle}
                  onChange={(e) => update("price_oracle", e.target.value)}
                  class="bg-vespra-bg border border-vespra-border rounded px-3 py-2.5 min-h-[44px] text-sm text-vespra-text focus:border-vespra-accent w-full"
                >
                  {ORACLES.map((o) => (
                    <option key={o} value={o}>{o}</option>
                  ))}
                </select>
              </div>
              <div>
                <FieldLabel>Fallback Oracle</FieldLabel>
                <select
                  value={config.price_oracle_fallback}
                  onChange={(e) => update("price_oracle_fallback", e.target.value)}
                  class="bg-vespra-bg border border-vespra-border rounded px-3 py-2.5 min-h-[44px] text-sm text-vespra-text focus:border-vespra-accent w-full"
                >
                  {ORACLES.map((o) => (
                    <option key={o} value={o}>{o}</option>
                  ))}
                </select>
              </div>
            </div>
          </div>
        </Card>
      )}

      {/* Step 5: Review & Save */}
      {step === 5 && (
        <Card title="Review & Save">
          <div class="space-y-4">
            {(() => {
              const changed = getChangedFields();
              const keys = Object.keys(changed);
              if (keys.length === 0) {
                return <p class="text-sm text-vespra-muted">No changes from current configuration.</p>;
              }
              return (
                <div class="overflow-x-auto">
                  <table class="w-full text-sm">
                    <thead>
                      <tr class="text-left text-xs text-vespra-muted border-b border-vespra-border">
                        <th scope="col" class="py-2 px-3 font-medium">Field</th>
                        <th scope="col" class="py-2 px-3 font-medium">Current</th>
                        <th scope="col" class="py-2 px-3 font-medium">New</th>
                      </tr>
                    </thead>
                    <tbody>
                      {keys.map((key) => (
                        <tr key={key} class="border-b border-vespra-border">
                          <td class="py-2 px-3 font-mono text-xs text-vespra-muted">{key}</td>
                          <td class="py-2 px-3 text-vespra-red">
                            {JSON.stringify(original[key])}
                          </td>
                          <td class="py-2 px-3 text-vespra-green">
                            {JSON.stringify(changed[key])}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              );
            })()}

            <div class="flex items-center gap-3 pt-2">
              <Button variant="accent" onClick={save} disabled={saving}>
                {saving ? "Saving..." : "Save Configuration"}
              </Button>
              {saveResult && (
                <span class={`text-sm ${saveResult.ok ? "text-vespra-green" : "text-vespra-red"}`}>
                  {saveResult.msg}
                </span>
              )}
            </div>

            {saveResult?.ok && saveResult.msg === "Configuration saved." && (
              <a
                href="/"
                class="inline-block mt-2 px-4 py-2 bg-vespra-accent/15 text-vespra-accent rounded text-sm hover:bg-vespra-accent/25 transition-colors"
              >
                Go to Overview
              </a>
            )}
          </div>
        </Card>
      )}

      {/* Validation error */}
      {validationError && (
        <div class="text-sm text-vespra-red">{validationError}</div>
      )}

      {/* Navigation */}
      <div class="flex justify-between">
        <Button variant="ghost" onClick={back} disabled={step === 1}>
          Back
        </Button>
        {step < 5 && (
          <Button variant="accent" onClick={next}>
            Next
          </Button>
        )}
      </div>
    </div>
  );
}
