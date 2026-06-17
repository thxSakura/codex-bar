export type RateWindow = {
  id: string;
  title: string;
  usedPercent: number;
  windowMinutes?: number | null;
  resetAt?: string | null;
  resetDescription?: string | null;
};

export type CostSnapshot = {
  balance: number;
  currency: string;
  label: string;
};

export type UsageSnapshot = {
  accountEmail?: string | null;
  planType?: string | null;
  loginMethod?: string | null;
  primary: RateWindow;
  secondary?: RateWindow | null;
  modelSpecific?: RateWindow | null;
  extraRateWindows: RateWindow[];
  credits?: CostSnapshot | null;
  updatedAt: string;
  source: string;
  stale: boolean;
  error?: string | null;
};

export type HeatmapDay = {
  date: string;
  value: number;
  intensity: number;
};

export type AppSettings = {
  refreshIntervalSecs: number;
  autostart: boolean;
  privacyMode: boolean;
  hideAccount: boolean;
};
