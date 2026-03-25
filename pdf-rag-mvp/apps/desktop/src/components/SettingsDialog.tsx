import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

interface LlmConfig {
  provider: string;
  api_key: string | null;
  base_url: string | null;
  model: string | null;
}

interface EmbeddingConfig {
  provider: string;
  api_key: string | null;
  base_url: string | null;
  model: string | null;
  dimensions: number | null;
}

interface AppConfig {
  llm: LlmConfig;
  embedding: EmbeddingConfig;
}

interface SettingsDialogProps {
  open: boolean;
  onClose: () => void;
}

export function SettingsDialog({ open, onClose }: SettingsDialogProps) {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      invoke<AppConfig>("get_config").then(setConfig).catch(console.error);
    }
  }, [open]);

  if (!open || !config) return null;

  const updateLlm = (field: keyof LlmConfig, value: string) => {
    setConfig({
      ...config,
      llm: { ...config.llm, [field]: value || null },
    });
  };

  const updateEmbed = (field: keyof EmbeddingConfig, value: string) => {
    setConfig({
      ...config,
      embedding: {
        ...config.embedding,
        [field]: field === "dimensions" ? (value ? parseInt(value) : null) : (value || null),
      },
    });
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      await invoke("save_config", { config });
      setStatus("Saved");
      setTimeout(() => {
        setStatus(null);
        onClose();
      }, 1000);
    } catch (e) {
      setStatus(`Error: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-white dark:bg-gray-900 rounded-xl shadow-2xl w-[520px] max-h-[80vh] overflow-y-auto">
        <div className="flex items-center justify-between px-6 py-4 border-b border-gray-200 dark:border-gray-800">
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">
            Settings
          </h2>
          <button
            onClick={onClose}
            className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
          >
            ✕
          </button>
        </div>

        <div className="px-6 py-4 space-y-6">
          {/* LLM Section */}
          <section>
            <h3 className="text-sm font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider mb-3">
              LLM Provider
            </h3>
            <div className="space-y-3">
              <div>
                <label className="block text-xs text-gray-500 mb-1">Provider</label>
                <select
                  value={config.llm.provider}
                  onChange={(e) => updateLlm("provider", e.target.value)}
                  className="w-full px-3 py-2 border border-gray-300 dark:border-gray-700 rounded-lg text-sm bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100"
                >
                  <option value="claude">Claude CLI (local, no API key)</option>
                  <option value="openai">OpenAI Compatible</option>
                  <option value="anthropic">Anthropic API</option>
                </select>
              </div>

              {config.llm.provider !== "claude" && (
                <>
                  <Field
                    label="API Key"
                    type="password"
                    value={config.llm.api_key || ""}
                    onChange={(v) => updateLlm("api_key", v)}
                    placeholder="sk-..."
                  />
                  <Field
                    label="Base URL"
                    value={config.llm.base_url || ""}
                    onChange={(v) => updateLlm("base_url", v)}
                    placeholder={
                      config.llm.provider === "anthropic"
                        ? "https://api.anthropic.com/v1"
                        : "https://api.openai.com/v1"
                    }
                  />
                  <Field
                    label="Model"
                    value={config.llm.model || ""}
                    onChange={(v) => updateLlm("model", v)}
                    placeholder={
                      config.llm.provider === "anthropic"
                        ? "claude-sonnet-4-6"
                        : "gpt-4o"
                    }
                  />
                </>
              )}
            </div>
          </section>

          {/* Embedding Section */}
          <section>
            <h3 className="text-sm font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider mb-3">
              Embedding Model
            </h3>
            <div className="space-y-3">
              <div>
                <label className="block text-xs text-gray-500 mb-1">Provider</label>
                <select
                  value={config.embedding.provider}
                  onChange={(e) => updateEmbed("provider", e.target.value)}
                  className="w-full px-3 py-2 border border-gray-300 dark:border-gray-700 rounded-lg text-sm bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100"
                >
                  <option value="hash">Offline (hash, no API)</option>
                  <option value="api">API (OpenAI compatible)</option>
                </select>
              </div>

              {config.embedding.provider === "api" && (
                <>
                  <Field
                    label="API Key"
                    type="password"
                    value={config.embedding.api_key || ""}
                    onChange={(v) => updateEmbed("api_key", v)}
                    placeholder="sk-..."
                  />
                  <Field
                    label="Base URL"
                    value={config.embedding.base_url || ""}
                    onChange={(v) => updateEmbed("base_url", v)}
                    placeholder="https://api.openai.com/v1"
                  />
                  <Field
                    label="Model"
                    value={config.embedding.model || ""}
                    onChange={(v) => updateEmbed("model", v)}
                    placeholder="text-embedding-3-small"
                  />
                  <Field
                    label="Dimensions"
                    value={config.embedding.dimensions?.toString() || ""}
                    onChange={(v) => updateEmbed("dimensions", v)}
                    placeholder="1536"
                  />
                </>
              )}
            </div>
          </section>

          <p className="text-xs text-gray-400">
            Note: Changing embedding provider requires re-importing documents.
          </p>
        </div>

        <div className="flex items-center justify-between px-6 py-4 border-t border-gray-200 dark:border-gray-800">
          {status && (
            <span
              className={`text-sm ${
                status.startsWith("Error") ? "text-red-500" : "text-green-500"
              }`}
            >
              {status}
            </span>
          )}
          <div className="flex gap-2 ml-auto">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 rounded-lg transition-colors"
            >
              Cancel
            </button>
            <button
              onClick={handleSave}
              disabled={saving}
              className="px-4 py-2 text-sm bg-blue-600 text-white rounded-lg hover:bg-blue-700 disabled:opacity-50 transition-colors"
            >
              {saving ? "Saving..." : "Save"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function Field({
  label,
  value,
  onChange,
  placeholder,
  type = "text",
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  type?: string;
}) {
  return (
    <div>
      <label className="block text-xs text-gray-500 mb-1">{label}</label>
      <input
        type={type}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className="w-full px-3 py-2 border border-gray-300 dark:border-gray-700 rounded-lg text-sm bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500"
      />
    </div>
  );
}
