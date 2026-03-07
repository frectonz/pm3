import { ArrowRight, Github } from "lucide-react";
import { codeToHtml } from "shiki";
import { Link } from "waku";
import { AsciinemaPlayer } from "@/components/asciinema-player";
import { CopyButton } from "@/components/copy-button";

const features = [
  {
    title: "Simple Config",
    description:
      "Define your processes in a single pm3.toml file. No complex setup — just TOML.",
  },
  {
    title: "Smart Restarts",
    description:
      "Exponential backoff, health checks, memory limits — your processes stay up.",
  },
  {
    title: "Interactive TUI",
    description:
      "Monitor everything in real-time from your terminal with a full-featured TUI.",
  },
];

const exampleToml = `[web]
command = "node server.js"
cwd = "./frontend"
env = { PORT = "3000" }
health_check = "http://localhost:3000/health"

[api]
command = "python app.py"
restart = "always"
depends_on = ["web"]

[worker]
command = "node worker.js"
max_memory = "512M"
cron_restart = "0 3 * * *"`;

const installCommand = "curl -LsSf https://pm3.frectonz.et/instal.sh | sh";
const releaseTag = "v0.1.8";
const inlineCodeClass =
  "border border-fd-border bg-fd-card/80 px-1.5 py-0.5 font-mono text-[0.9em] text-fd-foreground";
const quickStartCommand = "pm3 start";

export default async function Home() {
  const highlightedToml = await codeToHtml(exampleToml, {
    lang: "toml",
    themes: { light: "github-light", dark: "github-dark" },
  });

  return (
    <div className="flex flex-col min-h-screen">
      <title>pm3 - A modern process manager</title>
      <meta property="og:title" content="pm3 - A modern process manager" />
      <meta property="og:description" content="A modern process manager." />
      <meta property="og:image" content="/og/home.png" />
      <meta name="twitter:card" content="summary_large_image" />
      <meta name="twitter:image" content="/og/home.png" />
      {/* Hero */}
      <section className="home-hero border-b border-fd-border">
        <div className="relative mx-auto max-w-6xl px-6 pt-24 pb-20 sm:pt-32 sm:pb-28">
          <div className="grid grid-cols-1 items-center gap-12 lg:grid-cols-2 lg:gap-16">
            <div className="hero-copy">
              <a
                href="https://github.com/frectonz/pm3/releases"
                target="_blank"
                rel="noopener noreferrer"
                className="mb-10 inline-flex items-center gap-2 border border-fd-border bg-fd-card/80 px-3 py-1 text-xs text-fd-muted-foreground backdrop-blur-sm transition-colors hover:text-fd-foreground"
              >
                <span className="h-1.5 w-1.5 bg-fd-primary" />
                {releaseTag}
                <ArrowRight className="h-3 w-3" />
              </a>

              <h1 className="mb-4 font-mono text-5xl font-black tracking-tighter text-fd-foreground sm:text-6xl md:text-7xl lg:text-8xl">
                pm3
              </h1>
              <p className="mb-6 font-mono text-lg text-fd-primary sm:text-xl md:text-2xl">
                Define once. Start everything.
              </p>
              <p className="mb-8 max-w-lg text-base leading-relaxed text-fd-muted-foreground sm:text-lg">
                A process manager that keeps your services predictable. Put your
                stack in <code className={inlineCodeClass}>pm3.toml</code>, boot
                with one command, and watch health, restarts, memory, and logs
                in one TUI.
              </p>

              <div className="mb-8 flex flex-wrap gap-3">
                <Link
                  to="/docs/quick-start"
                  className="group inline-flex items-center gap-2 bg-fd-primary px-6 py-2.5 text-sm font-semibold text-fd-primary-foreground transition-opacity hover:opacity-90"
                >
                  Get Started
                  <ArrowRight className="h-4 w-4 transition-transform group-hover:translate-x-0.5" />
                </Link>
                <a
                  href="https://github.com/frectonz/pm3"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-2 border border-fd-border px-6 py-2.5 text-sm font-semibold text-fd-muted-foreground transition-colors hover:border-fd-foreground/30 hover:text-fd-foreground"
                >
                  <Github className="h-4 w-4" />
                  GitHub
                </a>
              </div>

              <div className="max-w-2xl overflow-hidden border border-fd-border bg-fd-card/80 px-4 py-2.5 backdrop-blur-sm">
                <div className="flex items-center gap-3">
                  <span className="flex-none font-mono text-xs text-fd-muted-foreground">
                    $
                  </span>
                  <div className="relative min-w-0 flex-1">
                    <code className="block overflow-x-auto whitespace-nowrap font-mono text-xs text-fd-muted-foreground">
                      {installCommand}
                    </code>
                    <div className="home-hero-install-fade" />
                  </div>
                  <div className="flex-none">
                    <CopyButton text={installCommand} />
                  </div>
                </div>
              </div>
            </div>

            <div className="hero-panel lg:translate-y-8 lg:translate-x-4">
              <div className="overflow-hidden border border-fd-border bg-fd-card/90 shadow-2xl shadow-black/10 dark:shadow-black/40">
                <div className="flex items-center gap-2.5 border-b border-fd-border bg-fd-muted/60 px-4 py-2.5">
                  <span className="h-2.5 w-2.5 bg-red-500" />
                  <span className="h-2.5 w-2.5 bg-yellow-500" />
                  <span className="h-2.5 w-2.5 bg-green-500" />
                  <span className="text-[11px] font-mono text-fd-muted-foreground">
                    pm3.toml
                  </span>
                </div>
                <div
                  className="toml-preview overflow-x-auto px-5 text-[13px] leading-relaxed"
                  dangerouslySetInnerHTML={{ __html: highlightedToml }}
                />
              </div>
            </div>
          </div>
        </div>

        <div className="home-hero-divider h-px" />
      </section>

      {/* Demo */}
      <section className="px-4 max-w-4xl mx-auto w-full">
        <AsciinemaPlayer />
      </section>

      {/* Features */}
      <section className="px-4 py-16 max-w-5xl mx-auto w-full">
        <div className="grid grid-cols-1 md:grid-cols-3 gap-6">
          {features.map((feature) => (
            <div
              key={feature.title}
              className=" border border-fd-border bg-fd-card p-6"
            >
              <h3 className="font-semibold text-fd-foreground mb-2">
                {feature.title}
              </h3>
              <p className="text-sm text-fd-muted-foreground">
                {feature.description}
              </p>
            </div>
          ))}
        </div>
      </section>

      {/* Quick Start */}
      <section className="px-4 py-16 max-w-3xl mx-auto w-full">
        <div className="mb-12 text-center">
          <p className="mb-3 font-mono text-xs font-medium uppercase tracking-[0.2em] text-fd-primary">
            Get Running In Seconds
          </p>
          <h2 className="text-2xl font-bold tracking-tight text-fd-foreground sm:text-3xl">
            Quick Start
          </h2>
        </div>

        <div className="space-y-4">
          <div className="overflow-hidden border border-fd-border bg-fd-card shadow-lg shadow-black/5 dark:shadow-black/25">
            <div className="flex items-center gap-2.5 border-b border-fd-border bg-fd-muted/60 px-4 py-2.5">
              <span className="flex h-5 w-5 items-center justify-center bg-fd-primary/15 font-mono text-[10px] font-bold text-fd-primary">
                1
              </span>
              <span className="text-[11px] font-mono text-fd-muted-foreground">
                pm3.toml
              </span>
            </div>
            <div
              className="toml-preview overflow-x-auto px-5 text-[13px] leading-relaxed"
              dangerouslySetInnerHTML={{ __html: highlightedToml }}
            />
          </div>

          <div className="overflow-hidden border border-fd-border bg-fd-card shadow-lg shadow-black/5 dark:shadow-black/25">
            <div className="flex items-center gap-2.5 border-b border-fd-border bg-fd-muted/60 px-4 py-2.5">
              <span className="flex h-5 w-5 items-center justify-center bg-emerald-500/15 font-mono text-[10px] font-bold text-emerald-600 dark:text-emerald-400">
                2
              </span>
              <span className="text-[11px] font-mono text-fd-muted-foreground">
                terminal
              </span>
            </div>
            <pre className="overflow-x-auto p-5 font-mono text-[13px] leading-relaxed">
              <span className="text-fd-muted-foreground">$ </span>
              <span className="text-fd-foreground">{quickStartCommand}</span>
            </pre>
          </div>
        </div>

        <p className="mt-8 text-center text-sm text-fd-muted-foreground">
          Then run <code className={inlineCodeClass}>pm3 list</code> or open{" "}
          <code className={inlineCodeClass}>pm3 tui</code> to monitor
          everything.
        </p>
      </section>

      {/* Footer */}
      <footer className="mt-auto border-t border-fd-border px-4 py-8">
        <div className="max-w-5xl mx-auto flex flex-col md:flex-row justify-between gap-8">
          <div>
            <span className="font-mono font-bold text-fd-foreground">pm3</span>
            <p className="text-sm text-fd-muted-foreground mt-1">
              A modern process manager.
            </p>
          </div>
          <div className="flex gap-12">
            <div>
              <h4 className="font-medium text-sm text-fd-foreground mb-2">
                Documentation
              </h4>
              <ul className="space-y-1">
                <li>
                  <Link
                    to="/docs/quick-start"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    Quick Start
                  </Link>
                </li>
                <li>
                  <Link
                    to="/docs/configuration"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    Configuration
                  </Link>
                </li>
                <li>
                  <Link
                    to="/docs/cli"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    CLI Reference
                  </Link>
                </li>
              </ul>
            </div>
            <div>
              <h4 className="font-medium text-sm text-fd-foreground mb-2">
                Links
              </h4>
              <ul className="space-y-1">
                <li>
                  <a
                    href="https://github.com/frectonz/pm3"
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    GitHub
                  </a>
                </li>
                <li>
                  <a
                    href="https://github.com/frectonz/pm3/releases"
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    Releases
                  </a>
                </li>
                <li>
                  <a
                    href="https://github.com/frectonz/pm3/blob/main/LICENSE"
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    MIT License
                  </a>
                </li>
              </ul>
            </div>
          </div>
        </div>
      </footer>
    </div>
  );
}

export const getConfig = async () => {
  return {
    render: "static",
  };
};
