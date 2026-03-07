"use client";

import { Check, ChevronDown, Copy, Trash2, X } from "lucide-react";
import { useEffect, useState } from "react";
import { codeToHtml } from "shiki";

interface Process {
  name: string;
  command: string;
  cwd: string;
  env: { key: string; value: string }[];
  env_file: string;
  restart: "" | "on_failure" | "always" | "never";
  max_restarts: string;
  min_uptime: string;
  stop_exit_codes: string;
  kill_signal: string;
  kill_timeout: string;
  health_check: string;
  max_memory: string;
  watch: string;
  watch_ignore: string;
  watch_debounce: string;
  depends_on: string;
  group: string;
  pre_start: string;
  post_stop: string;
  cron_restart: string;
  log_date_format: string;
}

function defaultProcess(): Process {
  return {
    name: "web",
    command: "",
    cwd: "",
    env: [],
    env_file: "",
    restart: "",
    max_restarts: "",
    min_uptime: "",
    stop_exit_codes: "",
    kill_signal: "",
    kill_timeout: "",
    health_check: "",
    max_memory: "",
    watch: "",
    watch_ignore: "",
    watch_debounce: "",
    depends_on: "",
    group: "",
    pre_start: "",
    post_stop: "",
    cron_restart: "",
    log_date_format: "",
  };
}

function escapeTomlString(s: string): string {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function generateToml(processes: Process[]): string {
  const sections: string[] = [];

  for (const proc of processes) {
    if (!proc.name) continue;

    const lines: string[] = [];
    lines.push(`[${proc.name}]`);

    if (proc.command) {
      lines.push(`command = "${escapeTomlString(proc.command)}"`);
    }
    if (proc.cwd) {
      lines.push(`cwd = "${escapeTomlString(proc.cwd)}"`);
    }

    // env as inline table
    const envPairs = proc.env.filter((e) => e.key.trim());
    if (envPairs.length > 0) {
      const pairs = envPairs
        .map((e) => `${e.key} = "${escapeTomlString(e.value)}"`)
        .join(", ");
      lines.push(`env = { ${pairs} }`);
    }

    if (proc.env_file) {
      const files = proc.env_file
        .split(",")
        .map((f) => f.trim())
        .filter(Boolean);
      if (files.length === 1) {
        lines.push(`env_file = "${escapeTomlString(files[0] as string)}"`);
      } else if (files.length > 1) {
        const arr = files.map((f) => `"${escapeTomlString(f)}"`).join(", ");
        lines.push(`env_file = [${arr}]`);
      }
    }

    if (proc.restart) {
      lines.push(`restart = "${proc.restart}"`);
    }
    if (proc.max_restarts) {
      lines.push(`max_restarts = ${proc.max_restarts}`);
    }
    if (proc.min_uptime) {
      lines.push(`min_uptime = ${proc.min_uptime}`);
    }

    if (proc.stop_exit_codes) {
      const codes = proc.stop_exit_codes
        .split(",")
        .map((c) => c.trim())
        .filter(Boolean);
      if (codes.length > 0) {
        lines.push(`stop_exit_codes = [${codes.join(", ")}]`);
      }
    }

    if (proc.kill_signal) {
      lines.push(`kill_signal = "${proc.kill_signal}"`);
    }
    if (proc.kill_timeout) {
      lines.push(`kill_timeout = ${proc.kill_timeout}`);
    }

    if (proc.health_check) {
      lines.push(`health_check = "${escapeTomlString(proc.health_check)}"`);
    }
    if (proc.max_memory) {
      lines.push(`max_memory = "${escapeTomlString(proc.max_memory)}"`);
    }

    if (proc.watch === "true") {
      lines.push("watch = true");
    } else if (proc.watch && proc.watch !== "disabled") {
      lines.push(`watch = "${escapeTomlString(proc.watch)}"`);
    }

    if (proc.watch_ignore) {
      const ignores = proc.watch_ignore
        .split(",")
        .map((i) => i.trim())
        .filter(Boolean);
      if (ignores.length > 0) {
        const arr = ignores.map((i) => `"${escapeTomlString(i)}"`).join(", ");
        lines.push(`watch_ignore = [${arr}]`);
      }
    }

    if (proc.watch_debounce) {
      lines.push(`watch_debounce = ${proc.watch_debounce}`);
    }

    if (proc.depends_on) {
      const deps = proc.depends_on
        .split(",")
        .map((d) => d.trim())
        .filter(Boolean);
      if (deps.length > 0) {
        const arr = deps.map((d) => `"${escapeTomlString(d)}"`).join(", ");
        lines.push(`depends_on = [${arr}]`);
      }
    }

    if (proc.group) {
      lines.push(`group = "${escapeTomlString(proc.group)}"`);
    }
    if (proc.pre_start) {
      lines.push(`pre_start = "${escapeTomlString(proc.pre_start)}"`);
    }
    if (proc.post_stop) {
      lines.push(`post_stop = "${escapeTomlString(proc.post_stop)}"`);
    }
    if (proc.cron_restart) {
      lines.push(`cron_restart = "${escapeTomlString(proc.cron_restart)}"`);
    }
    if (proc.log_date_format) {
      lines.push(
        `log_date_format = "${escapeTomlString(proc.log_date_format)}"`,
      );
    }

    sections.push(lines.join("\n"));
  }

  return sections.join("\n\n");
}

function Section({
  title,
  defaultOpen = false,
  children,
}: {
  title: string;
  defaultOpen?: boolean;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);

  return (
    <div className="border border-fd-border ">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="w-full flex items-center justify-between px-4 py-3 text-sm font-medium text-fd-foreground hover:bg-fd-accent/50 transition-colors "
      >
        {title}
        <ChevronDown
          className={`w-4 h-4 transition-transform ${open ? "rotate-180" : ""}`}
        />
      </button>
      {open && (
        <div className="px-4 pb-4 space-y-3 border-t border-fd-border pt-3">
          {children}
        </div>
      )}
    </div>
  );
}

function Field({
  id,
  label,
  hint,
  children,
}: {
  id: string;
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <label
        htmlFor={id}
        className="block text-sm font-medium text-fd-foreground mb-1"
      >
        {label}
      </label>
      {children}
      {hint && <p className="text-xs text-fd-muted-foreground mt-1">{hint}</p>}
    </div>
  );
}

const inputClass =
  "w-full px-3 py-2  border border-fd-border bg-fd-card text-fd-foreground text-sm placeholder:text-fd-muted-foreground focus:outline-none focus:ring-2 focus:ring-fd-primary/50 focus:border-fd-primary";

const selectClass =
  "w-full px-3 py-2  border border-fd-border bg-fd-card text-fd-foreground text-sm focus:outline-none focus:ring-2 focus:ring-fd-primary/50 focus:border-fd-primary";

function TomlPreview({ code }: { code: string }) {
  const [html, setHtml] = useState("");
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!code) {
      setHtml("");
      return;
    }
    let cancelled = false;
    codeToHtml(code, {
      lang: "toml",
      themes: { light: "github-light", dark: "github-dark" },
    }).then((result) => {
      if (!cancelled) setHtml(result);
    });
    return () => {
      cancelled = true;
    };
  }, [code]);

  function handleCopy() {
    navigator.clipboard.writeText(code).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }

  if (!code) {
    return (
      <div className="border border-fd-border bg-fd-card overflow-hidden">
        <pre className="p-4 font-mono text-sm text-fd-muted-foreground min-h-[200px]">
          Fill in the form to generate your pm3.toml
        </pre>
      </div>
    );
  }

  return (
    <div className="border border-fd-border bg-fd-card overflow-hidden relative">
      <div className="flex items-center justify-between px-4 py-2 border-b border-fd-border">
        <span className="text-xs font-mono text-fd-muted-foreground">
          pm3.toml
        </span>
        <button
          type="button"
          onClick={handleCopy}
          className="flex items-center gap-1.5 px-3 py-1 text-xs font-medium transition-colors bg-fd-primary text-fd-primary-foreground hover:opacity-90"
        >
          {copied ? (
            <>
              <Check className="w-3 h-3" /> Copied!
            </>
          ) : (
            <>
              <Copy className="w-3 h-3" /> Copy
            </>
          )}
        </button>
      </div>
      <div
        className="toml-preview px-4 text-sm overflow-x-auto min-h-[200px]"
        dangerouslySetInnerHTML={{ __html: html }}
      />
    </div>
  );
}

export function ConfigBuilder() {
  const [processes, setProcesses] = useState<Process[]>([defaultProcess()]);
  const [activeTab, setActiveTab] = useState(0);
  const proc = processes[activeTab] ?? defaultProcess();
  const toml = generateToml(processes);

  function updateProcess(index: number, updates: Partial<Process>) {
    setProcesses((prev) =>
      prev.map((p, i) => (i === index ? { ...p, ...updates } : p)),
    );
  }

  function addProcess() {
    const names = processes.map((p) => p.name);
    let name = "process";
    let i = 1;
    while (names.includes(name)) {
      name = `process${i}`;
      i++;
    }
    setProcesses((prev) => [...prev, { ...defaultProcess(), name }]);
    setActiveTab(processes.length);
  }

  function removeProcess(index: number) {
    if (processes.length <= 1) return;
    setProcesses((prev) => prev.filter((_, i) => i !== index));
    setActiveTab((prev) => (prev >= index && prev > 0 ? prev - 1 : prev));
  }

  function addEnvVar() {
    const env = [...proc.env, { key: "", value: "" }];
    updateProcess(activeTab, { env });
  }

  function updateEnvVar(
    envIndex: number,
    field: "key" | "value",
    value: string,
  ) {
    const env = proc.env.map((e, i) =>
      i === envIndex ? { ...e, [field]: value } : e,
    );
    updateProcess(activeTab, { env });
  }

  function removeEnvVar(envIndex: number) {
    const env = proc.env.filter((_, i) => i !== envIndex);
    updateProcess(activeTab, { env });
  }

  return (
    <div className="min-h-screen">
      <div className="max-w-7xl mx-auto px-4 py-8">
        <div className="text-center mb-8">
          <h1 className="font-mono font-bold text-3xl md:text-4xl mb-2">
            Config Builder
          </h1>
          <p className="text-fd-muted-foreground">
            Visually build your pm3.toml configuration file.
          </p>
        </div>

        {/* Process tabs */}
        <div className="flex items-center gap-2 flex-wrap mb-4">
          {processes.map((p, i) => (
            <div
              key={`${p.name}-${i}`}
              className={`flex items-center  text-sm font-mono transition-colors ${
                i === activeTab
                  ? "bg-fd-primary text-fd-primary-foreground"
                  : "bg-fd-card border border-fd-border text-fd-foreground hover:bg-fd-accent/50"
              }`}
            >
              <button
                type="button"
                onClick={() => setActiveTab(i)}
                className="px-3 py-1.5"
              >
                {p.name || "unnamed"}
              </button>
              {processes.length > 1 && (
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    removeProcess(i);
                  }}
                  className={`pr-2 py-1.5 opacity-60 hover:opacity-100 transition-opacity ${
                    i === activeTab
                      ? "text-fd-primary-foreground"
                      : "text-fd-muted-foreground"
                  }`}
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              )}
            </div>
          ))}
          <button
            type="button"
            onClick={addProcess}
            className="px-3 py-1.5  text-sm border border-dashed border-fd-border text-fd-muted-foreground hover:text-fd-foreground hover:border-fd-foreground transition-colors"
          >
            + Add Process
          </button>
        </div>

        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          {/* Left: Form */}
          <div className="space-y-4">
            {/* Basic */}
            <Section title="Basic" defaultOpen>
              <Field
                id="process-name"
                label="Process Name"
                hint="Used as the TOML section header"
              >
                <input
                  id="process-name"
                  type="text"
                  className={inputClass}
                  placeholder="web"
                  value={proc.name}
                  onChange={(e) =>
                    updateProcess(activeTab, { name: e.target.value })
                  }
                />
              </Field>
              <Field
                id="command"
                label="Command"
                hint="The shell command to execute (required)"
              >
                <input
                  id="command"
                  type="text"
                  className={inputClass}
                  placeholder="node server.js"
                  value={proc.command}
                  onChange={(e) =>
                    updateProcess(activeTab, { command: e.target.value })
                  }
                />
              </Field>
              <Field
                id="cwd"
                label="Working Directory"
                hint="Defaults to pm3.toml directory"
              >
                <input
                  id="cwd"
                  type="text"
                  className={inputClass}
                  placeholder="./frontend"
                  value={proc.cwd}
                  onChange={(e) =>
                    updateProcess(activeTab, { cwd: e.target.value })
                  }
                />
              </Field>
            </Section>

            {/* Environment */}
            <Section title="Environment">
              <Field id="env" label="Environment Variables">
                <div className="space-y-2">
                  {proc.env.map((envVar, i) => (
                    <div key={`${i}-${envVar.key}`} className="flex gap-2">
                      <input
                        type="text"
                        className={`${inputClass} flex-1`}
                        placeholder="KEY"
                        value={envVar.key}
                        onChange={(e) => updateEnvVar(i, "key", e.target.value)}
                      />
                      <input
                        type="text"
                        className={`${inputClass} flex-1`}
                        placeholder="value"
                        value={envVar.value}
                        onChange={(e) =>
                          updateEnvVar(i, "value", e.target.value)
                        }
                      />
                      <button
                        type="button"
                        onClick={() => removeEnvVar(i)}
                        className="px-2 text-fd-muted-foreground hover:text-fd-foreground transition-colors"
                      >
                        <X className="w-4 h-4" />
                      </button>
                    </div>
                  ))}
                  <button
                    type="button"
                    onClick={addEnvVar}
                    className="text-sm text-fd-primary hover:underline"
                  >
                    + Add Variable
                  </button>
                </div>
              </Field>
              <Field
                id="env-file"
                label="Env File"
                hint="Comma-separated paths to .env files"
              >
                <input
                  id="env-file"
                  type="text"
                  className={inputClass}
                  placeholder=".env, .env.local"
                  value={proc.env_file}
                  onChange={(e) =>
                    updateProcess(activeTab, { env_file: e.target.value })
                  }
                />
              </Field>
            </Section>

            {/* Restart Policy */}
            <Section title="Restart Policy">
              <Field id="restart" label="Policy">
                <select
                  id="restart"
                  className={selectClass}
                  value={proc.restart}
                  onChange={(e) =>
                    updateProcess(activeTab, {
                      restart: e.target.value as Process["restart"],
                    })
                  }
                >
                  <option value="">Default (on_failure)</option>
                  <option value="on_failure">on_failure</option>
                  <option value="always">always</option>
                  <option value="never">never</option>
                </select>
              </Field>
              <Field id="max-restarts" label="Max Restarts" hint="Default: 15">
                <input
                  id="max-restarts"
                  type="number"
                  className={inputClass}
                  placeholder="15"
                  value={proc.max_restarts}
                  onChange={(e) =>
                    updateProcess(activeTab, { max_restarts: e.target.value })
                  }
                />
              </Field>
              <Field
                id="min-uptime"
                label="Min Uptime (ms)"
                hint="Reset restart counter after this duration. Default: 1000"
              >
                <input
                  id="min-uptime"
                  type="number"
                  className={inputClass}
                  placeholder="1000"
                  value={proc.min_uptime}
                  onChange={(e) =>
                    updateProcess(activeTab, { min_uptime: e.target.value })
                  }
                />
              </Field>
              <Field
                id="stop-exit-codes"
                label="Stop Exit Codes"
                hint="Comma-separated exit codes that should not trigger a restart"
              >
                <input
                  id="stop-exit-codes"
                  type="text"
                  className={inputClass}
                  placeholder="0, 143"
                  value={proc.stop_exit_codes}
                  onChange={(e) =>
                    updateProcess(activeTab, {
                      stop_exit_codes: e.target.value,
                    })
                  }
                />
              </Field>
            </Section>

            {/* Shutdown */}
            <Section title="Shutdown">
              <Field id="kill-signal" label="Kill Signal">
                <select
                  id="kill-signal"
                  className={selectClass}
                  value={proc.kill_signal}
                  onChange={(e) =>
                    updateProcess(activeTab, { kill_signal: e.target.value })
                  }
                >
                  <option value="">Default (SIGTERM)</option>
                  <option value="SIGTERM">SIGTERM</option>
                  <option value="SIGINT">SIGINT</option>
                  <option value="SIGHUP">SIGHUP</option>
                  <option value="SIGKILL">SIGKILL</option>
                </select>
              </Field>
              <Field
                id="kill-timeout"
                label="Kill Timeout (ms)"
                hint="Time before SIGKILL after kill signal. Default: 5000"
              >
                <input
                  id="kill-timeout"
                  type="number"
                  className={inputClass}
                  placeholder="5000"
                  value={proc.kill_timeout}
                  onChange={(e) =>
                    updateProcess(activeTab, { kill_timeout: e.target.value })
                  }
                />
              </Field>
            </Section>

            {/* Health & Monitoring */}
            <Section title="Health & Monitoring">
              <Field id="health-check" label="Health Check URL">
                <input
                  id="health-check"
                  type="text"
                  className={inputClass}
                  placeholder="http://localhost:3000/health"
                  value={proc.health_check}
                  onChange={(e) =>
                    updateProcess(activeTab, { health_check: e.target.value })
                  }
                />
              </Field>
              <Field
                id="max-memory"
                label="Max Memory"
                hint='e.g. "512M", "1G"'
              >
                <input
                  id="max-memory"
                  type="text"
                  className={inputClass}
                  placeholder="512M"
                  value={proc.max_memory}
                  onChange={(e) =>
                    updateProcess(activeTab, { max_memory: e.target.value })
                  }
                />
              </Field>
            </Section>

            {/* File Watching */}
            <Section title="File Watching">
              <Field id="watch" label="Watch">
                <select
                  id="watch"
                  className={selectClass}
                  value={
                    proc.watch === "" || proc.watch === "disabled"
                      ? "disabled"
                      : proc.watch === "true"
                        ? "true"
                        : "custom"
                  }
                  onChange={(e) => {
                    const v = e.target.value;
                    if (v === "disabled") {
                      updateProcess(activeTab, { watch: "" });
                    } else if (v === "true") {
                      updateProcess(activeTab, { watch: "true" });
                    } else {
                      updateProcess(activeTab, { watch: "./" });
                    }
                  }}
                >
                  <option value="disabled">Disabled</option>
                  <option value="true">Watch cwd (true)</option>
                  <option value="custom">Custom path</option>
                </select>
              </Field>
              {proc.watch !== "" &&
                proc.watch !== "disabled" &&
                proc.watch !== "true" && (
                  <Field id="watch-path" label="Watch Path">
                    <input
                      id="watch-path"
                      type="text"
                      className={inputClass}
                      placeholder="./src"
                      value={proc.watch}
                      onChange={(e) =>
                        updateProcess(activeTab, { watch: e.target.value })
                      }
                    />
                  </Field>
                )}
              <Field
                id="watch-debounce"
                label="Watch Debounce (ms)"
                hint="Debounce duration before restart after file changes. Default: 500"
              >
                <input
                  id="watch-debounce"
                  type="number"
                  className={inputClass}
                  placeholder="500"
                  value={proc.watch_debounce}
                  onChange={(e) =>
                    updateProcess(activeTab, {
                      watch_debounce: e.target.value,
                    })
                  }
                />
              </Field>
              <Field
                id="watch-ignore"
                label="Watch Ignore"
                hint="Comma-separated patterns to ignore"
              >
                <input
                  id="watch-ignore"
                  type="text"
                  className={inputClass}
                  placeholder="node_modules, .git"
                  value={proc.watch_ignore}
                  onChange={(e) =>
                    updateProcess(activeTab, { watch_ignore: e.target.value })
                  }
                />
              </Field>
            </Section>

            {/* Dependencies & Groups */}
            <Section title="Dependencies & Groups">
              <Field
                id="depends-on"
                label="Depends On"
                hint="Comma-separated process names"
              >
                <input
                  id="depends-on"
                  type="text"
                  className={inputClass}
                  placeholder="database, cache"
                  value={proc.depends_on}
                  onChange={(e) =>
                    updateProcess(activeTab, { depends_on: e.target.value })
                  }
                />
              </Field>
              <Field id="group" label="Group">
                <input
                  id="group"
                  type="text"
                  className={inputClass}
                  placeholder="web"
                  value={proc.group}
                  onChange={(e) =>
                    updateProcess(activeTab, { group: e.target.value })
                  }
                />
              </Field>
            </Section>

            {/* Lifecycle Hooks */}
            <Section title="Lifecycle Hooks">
              <Field
                id="pre-start"
                label="Pre Start"
                hint="Command to run before the process starts"
              >
                <input
                  id="pre-start"
                  type="text"
                  className={inputClass}
                  placeholder="npm run build"
                  value={proc.pre_start}
                  onChange={(e) =>
                    updateProcess(activeTab, { pre_start: e.target.value })
                  }
                />
              </Field>
              <Field
                id="post-stop"
                label="Post Stop"
                hint="Command to run after the process stops"
              >
                <input
                  id="post-stop"
                  type="text"
                  className={inputClass}
                  placeholder="echo 'stopped'"
                  value={proc.post_stop}
                  onChange={(e) =>
                    updateProcess(activeTab, { post_stop: e.target.value })
                  }
                />
              </Field>
            </Section>

            {/* Scheduling */}
            <Section title="Scheduling">
              <Field
                id="cron-restart"
                label="Cron Restart"
                hint="Cron expression to periodically restart the process"
              >
                <input
                  id="cron-restart"
                  type="text"
                  className={inputClass}
                  placeholder="0 3 * * *"
                  value={proc.cron_restart}
                  onChange={(e) =>
                    updateProcess(activeTab, { cron_restart: e.target.value })
                  }
                />
              </Field>
            </Section>

            {/* Logging */}
            <Section title="Logging">
              <Field
                id="log-date-format"
                label="Log Date Format"
                hint="strftime format for log timestamps"
              >
                <input
                  id="log-date-format"
                  type="text"
                  className={inputClass}
                  placeholder="%Y-%m-%d %H:%M:%S"
                  value={proc.log_date_format}
                  onChange={(e) =>
                    updateProcess(activeTab, {
                      log_date_format: e.target.value,
                    })
                  }
                />
              </Field>
            </Section>
          </div>

          {/* Right: TOML Preview */}
          <div className="lg:self-start lg:sticky lg:top-20">
            <TomlPreview code={toml} />
          </div>
        </div>
      </div>
    </div>
  );
}
