import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AlertCircle, Check, EyeOff, Loader2, RefreshCw, Settings, Shield, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import type { AppSettings, HeatmapDay, RateWindow, UsageSnapshot } from "./types";

const DEFAULT_SETTINGS: AppSettings = {
  refreshIntervalSecs: 300,
  autostart: false,
  privacyMode: true,
  hideAccount: true,
};

function formatAccount(email?: string | null, privacy = false, hidden = true) {
  if (privacy) return "Codex account";
  if (!email) return "Codex account";
  if (!hidden) return email;
  const [name, domain] = email.split("@");
  if (!domain) return "Hidden";
  return `${name.slice(0, 1)}***@${domain}`;
}

function windowLabel(minutes?: number | null) {
  if (!minutes) return "";
  if (minutes >= 7 * 24 * 60) return "7天";
  if (minutes >= 60) return `${Math.round(minutes / 60)}小时`;
  return `${minutes}分钟`;
}

function resetLabel(window: RateWindow) {
  if (window.resetDescription) return window.resetDescription;
  if (!window.resetAt) return "";
  const reset = new Date(window.resetAt);
  if (Number.isNaN(reset.getTime())) return "";
  return reset.toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function resetDisplayLabel(window: RateWindow) {
  return resetLabel(window);
}

function resetTooltipLabel(window: RateWindow) {
  const reset = parseResetAt(window);
  if (!reset) return "";
  if (isWeeklyWindow(window) && reset.getTime() - Date.now() >= 24 * 60 * 60 * 1000) {
    return `${reset.getMonth() + 1}月${reset.getDate()}日`;
  }
  return formatClockTime(reset);
}

function parseResetAt(window: RateWindow) {
  if (!window.resetAt) return null;
  const reset = new Date(window.resetAt);
  return Number.isNaN(reset.getTime()) ? null : reset;
}

function formatClockTime(date: Date) {
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

function isWeeklyWindow(window: RateWindow) {
  return (window.windowMinutes ?? 0) >= 7 * 24 * 60;
}

function formatTokenCount(value: number) {
  return `${formatTokenAmount(value)} Token`;
}

function formatTokenAmount(value: number) {
  if (value >= 100_000_000) {
    return `${trimFixed(value / 100_000_000)}亿`;
  }
  if (value < 10_000) {
    return value.toLocaleString("zh-CN", { maximumFractionDigits: 0 });
  }
  return `${trimFixed(value / 10_000)}万`;
}

function trimFixed(value: number) {
  return value.toLocaleString("zh-CN", {
    maximumFractionDigits: value >= 100 ? 0 : 1,
    minimumFractionDigits: 0,
  });
}

type UsageAccent = "green" | "amber" | "red";

function usageAccent(remaining: number): UsageAccent {
  if (remaining >= 60) return "green";
  if (remaining >= 30) return "amber";
  return "red";
}

function UsageBars({ win }: { win: RateWindow }) {
  const used = Math.max(0, Math.min(100, Math.round(win.usedPercent)));
  const remaining = 100 - used;
  const filled = Math.round(remaining / 4);
  const accent = usageAccent(remaining);
  const resetTooltip = resetTooltipLabel(win);
  return (
    <div className="usage-row">
      <div className="usage-label">{windowLabel(win.windowMinutes) || win.title}</div>
      <div className="pill-track" aria-label={`${win.title} 剩余 ${remaining}%`}>
        {Array.from({ length: 25 }).map((_, index) => (
          <span
            className={`pill ${index < filled ? `pill-${accent}` : ""}`}
            key={`${win.id}-${index}`}
          />
        ))}
      </div>
      <div className="usage-percent">{remaining}%</div>
      <div
        className="usage-reset"
        data-tooltip={resetTooltip || undefined}
      >
        {resetDisplayLabel(win)}
      </div>
    </div>
  );
}

function ProviderCard({ title, windows }: { title: string; windows: RateWindow[] }) {
  return (
    <section className="provider-card">
      <div className="provider-title">{title}</div>
      <div className="window-list">
        {windows.map((win) => (
          <UsageBars key={win.id} win={win} />
        ))}
      </div>
    </section>
  );
}

function Heatmap({ days }: { days: HeatmapDay[] }) {
  const total = days.reduce((sum, day) => sum + day.value, 0);

  return (
    <section className="heatmap-panel">
      <div className="heatmap-head">
        <div>
          <div className="heatmap-caption">近 210 天</div>
          <div className="heatmap-value">{formatTokenCount(total)}</div>
        </div>
        <span>{days[0]?.date ?? ""} - {days[days.length - 1]?.date ?? ""}</span>
      </div>
      <div className="heatmap-grid">
        {days.map((day, index) => {
          const column = Math.floor(index / 7);
          const totalColumns = Math.ceil(days.length / 7);
          const edgeClass =
            column >= totalColumns - 9 ? "heat-edge-right" : column <= 8 ? "heat-edge-left" : "";
          return (
            <span
              className={`heat heat-${day.intensity} ${edgeClass}`}
              aria-label={heatmapTooltip(day)}
              data-tooltip={heatmapTooltip(day)}
              key={day.date}
            />
          );
        })}
      </div>
    </section>
  );
}

function heatmapTooltip(day: HeatmapDay) {
  return `${formatHeatmapDate(day.date)}  使用了  ${formatTokenAmount(day.value)}  个 Token`;
}

function formatHeatmapDate(date: string) {
  const parsed = new Date(`${date}T00:00:00`);
  if (Number.isNaN(parsed.getTime())) return date;
  return `${parsed.getMonth() + 1}月${parsed.getDate()}日`;
}

function dedupeWindows(windows: RateWindow[]) {
  const seen = new Set<string>();
  return windows.filter((win) => {
    const key = windowKey(win);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function sameWindow(left: RateWindow, right: RateWindow) {
  return windowKey(left) === windowKey(right);
}

function windowKey(win: RateWindow) {
  return [
    win.windowMinutes ?? "unknown",
    Math.round(win.usedPercent),
    win.resetAt ?? "",
  ].join(":");
}

function SettingsPopover({
  settings,
  onChange,
}: {
  settings: AppSettings;
  onChange: (settings: Partial<AppSettings>) => void;
}) {
  return (
    <div className="settings-popover">
      <label>
        <span>刷新间隔</span>
        <select
          value={settings.refreshIntervalSecs}
          onChange={(event) => onChange({ refreshIntervalSecs: Number(event.target.value) })}
        >
          <option value={60}>1 分钟</option>
          <option value={300}>5 分钟</option>
          <option value={900}>15 分钟</option>
        </select>
      </label>
      <button
        className={`setting-toggle ${settings.privacyMode ? "active" : ""}`}
        onClick={() => onChange({ privacyMode: !settings.privacyMode })}
      >
        <Shield size={14} />
        隐私模式
        {settings.privacyMode ? <Check size={14} /> : null}
      </button>
      <button
        className={`setting-toggle ${settings.hideAccount ? "active" : ""}`}
        onClick={() => onChange({ hideAccount: !settings.hideAccount })}
      >
        <EyeOff size={14} />
        隐藏账号
        {settings.hideAccount ? <Check size={14} /> : null}
      </button>
    </div>
  );
}

export default function App() {
  const [usage, setUsage] = useState<UsageSnapshot | null>(null);
  const [heatmap, setHeatmap] = useState<HeatmapDay[]>([]);
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [loading, setLoading] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);

  const load = useCallback(async (refresh = false) => {
    setLoading(true);
    try {
      const [usageResult, heatmapResult, settingsResult] = await Promise.all([
        invoke<UsageSnapshot>(refresh ? "refresh_usage" : "get_usage"),
        invoke<HeatmapDay[]>("get_heatmap", { days: 210 }),
        invoke<AppSettings>("get_settings"),
      ]);
      setUsage(usageResult);
      setHeatmap(heatmapResult);
      setSettings(settingsResult);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load(true);
    const unlisten = listen<UsageSnapshot>("usage-refreshed", (event) => setUsage(event.payload));
    return () => {
      unlisten.then((off) => off());
    };
  }, [load]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") getCurrentWindow().hide();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    if (!settingsOpen) return;

    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Element | null;
      if (
        target?.closest(".settings-popover") ||
        target?.closest("[data-settings-trigger='true']")
      ) {
        return;
      }
      setSettingsOpen(false);
    };

    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [settingsOpen]);

  const grouped = useMemo(() => {
    if (!usage) return { codex: [], spark: [] };
    const codex = dedupeWindows([usage.primary, usage.secondary].filter(Boolean) as RateWindow[]);
    const spark = dedupeWindows(usage.extraRateWindows.filter((win) => win.id.includes("spark")));
    const other = usage.extraRateWindows
      .filter((win) => !win.id.includes("spark"))
      .filter((win) => !codex.some((base) => sameWindow(base, win)));
    return { codex: dedupeWindows(codex.concat(other)), spark };
  }, [usage]);

  async function updateSettings(patch: Partial<AppSettings>) {
    const updated = await invoke<AppSettings>("update_settings", { settings: patch });
    setSettings(updated);
  }

  const hasError = Boolean(usage?.error);

  return (
    <main className="shell">
      <header className="topbar">
        <div className="account">
          <div className="avatar-dot" />
          <span>{formatAccount(usage?.accountEmail, settings.privacyMode, settings.hideAccount)}</span>
        </div>
        <div className="top-actions">
          <span className="plan-chip">{usage?.loginMethod || usage?.planType || "Codex"}</span>
          <button className="icon-button" onClick={() => load(true)} title="刷新">
            {loading ? <Loader2 className="spin" size={16} /> : <RefreshCw size={16} />}
          </button>
          <button
            className="icon-button"
            data-settings-trigger="true"
            onClick={() => setSettingsOpen((open) => !open)}
            title="设置"
          >
            <Settings size={16} />
          </button>
          <button className="icon-button" onClick={() => getCurrentWindow().hide()} title="关闭">
            <X size={16} />
          </button>
        </div>
        {settingsOpen ? <SettingsPopover settings={settings} onChange={updateSettings} /> : null}
      </header>

      {usage ? (
        <>
          <div className="cards">
            <ProviderCard title="Codex" windows={grouped.codex} />
            {grouped.spark.length ? (
              <ProviderCard title="GPT-5.3-Codex-Spark" windows={grouped.spark} />
            ) : null}
          </div>
          <Heatmap days={heatmap} />
        </>
      ) : (
        <section className="empty-panel">
          <AlertCircle size={24} />
          <div>
            <strong>未找到 Codex 登录态</strong>
            <span>请先在终端运行 codex login</span>
          </div>
        </section>
      )}

      <footer className={`status ${hasError ? "status-error" : ""}`}>
        <span>{hasError ? <AlertCircle size={14} /> : <Check size={14} />}</span>
        <span>
          {usage?.error ||
            (usage ? `数据更新时间 ${new Date(usage.updatedAt).toLocaleTimeString("zh-CN")}` : "等待数据")}
        </span>
        {usage?.stale ? <em>缓存</em> : null}
      </footer>
    </main>
  );
}
