import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import enUS from "../locales/en-US.json";
import ptBR from "../locales/pt-BR.json";
import es from "../locales/es.json";
import fr from "../locales/fr.json";
import de from "../locales/de.json";
import it from "../locales/it.json";
import ja from "../locales/ja.json";
import ko from "../locales/ko.json";
import zhCN from "../locales/zh-CN.json";

export const FALLBACK_LOCALE = "en-US" as const;
export const LOCALE_STORAGE_KEY = "noland.locale";

export const LOCALE_OPTIONS = [
  { value: "system", label: "System Default" },
  { value: "en-US", label: "English" },
  { value: "pt-BR", label: "Português (Brasil)" },
  { value: "es", label: "Español" },
  { value: "fr", label: "Français" },
  { value: "de", label: "Deutsch" },
  { value: "it", label: "Italiano" },
  { value: "ja", label: "日本語" },
  { value: "ko", label: "한국어" },
  { value: "zh-CN", label: "简体中文" },
  { value: "en-XA", label: "Pseudo (en-XA)" },
] as const;

export type LocalePreference = (typeof LOCALE_OPTIONS)[number]["value"];
type Messages = Record<string, string>;

const bundles: Record<string, Messages> = {
  "en-US": enUS,
  "pt-BR": ptBR,
  es,
  fr,
  de,
  it,
  ja,
  ko,
  "zh-CN": zhCN,
  "en-XA": enUS,
};
let activeLocale: string = FALLBACK_LOCALE;
const sourceKeyByMessage = new Map(
  Object.entries(enUS).map(([key, message]) => [message, key]),
);

export function translate(key: string, values: Record<string, string | number> = {}): string {
  const message = formatMessage(bundles[activeLocale]?.[key] ?? bundles[FALLBACK_LOCALE][key] ?? key, values);
  return activeLocale === "en-XA" ? pseudoLocalize(message) : message;
}

export function translateSource(message: string): string {
  const key = sourceKeyByMessage.get(message);
  return key ? translate(key) : message;
}

export function translateOptional(key: string): string | null {
  return key in (bundles[activeLocale] ?? {}) || key in bundles[FALLBACK_LOCALE]
    ? translate(key)
    : null;
}

export function getActiveLocale(): string {
  return activeLocale;
}

function pseudoLocalize(value: string): string {
  const accents: Record<string, string> = { a: "å", e: "ë", i: "ï", o: "ø", u: "ü", A: "Å", E: "Ë", I: "Ï", O: "Ø", U: "Ü" };
  return `[ ${value.replace(/[aeiouAEIOU]/g, (letter) => accents[letter] ?? letter)} ——— ]`;
}

function systemLocale(): string {
  return typeof navigator === "undefined" ? FALLBACK_LOCALE : navigator.language || FALLBACK_LOCALE;
}

export function resolveLocale(preference: LocalePreference): string {
  const requested = preference === "system" ? systemLocale() : preference;
  if (bundles[requested]) return requested;
  const base = requested.split("-")[0].toLowerCase();
  return Object.keys(bundles).find((locale) => locale.split("-")[0].toLowerCase() === base) ?? FALLBACK_LOCALE;
}

function initialPreference(): LocalePreference {
  const stored = typeof window === "undefined" ? null : window.localStorage.getItem(LOCALE_STORAGE_KEY);
  return LOCALE_OPTIONS.some((option) => option.value === stored)
    ? (stored as LocalePreference)
    : "system";
}

function formatMessage(template: string, values: Record<string, string | number>): string {
  let result = template.replace(/\{(\w+),\s*plural,\s*((?:[^{}]|\{[^{}]*\})*)\}/g, (_, name: string, body: string) => {
    const amount = Number(values[name] ?? 0);
    const exact = body.match(new RegExp(`=${amount}\\s*\\{([^}]*)\\}`));
    const other = body.match(/other\s*\{([^}]*)\}/);
    return (exact?.[1] ?? other?.[1] ?? "").split("#").join(String(amount));
  });
  return result.replace(/\{(\w+)\}/g, (_, name: string) => String(values[name] ?? `{${name}}`));
}

interface LocalizationContextValue {
  preference: LocalePreference;
  locale: string;
  setLocale: (locale: LocalePreference) => void;
  t: (key: string, values?: Record<string, string | number>) => string;
  formatDate: (value: Date | number, options?: Intl.DateTimeFormatOptions) => string;
  formatNumber: (value: number, options?: Intl.NumberFormatOptions) => string;
  formatCurrency: (value: number, currency: string) => string;
}

const LocalizationContext = createContext<LocalizationContextValue | null>(null);

export function LocalizationProvider({ children }: { children: ReactNode }) {
  const [preference, setPreference] = useState<LocalePreference>(initialPreference);
  const locale = resolveLocale(preference);
  activeLocale = locale;
  useEffect(() => {
    document.documentElement.lang = locale;
    document.documentElement.dir = ["ar", "he", "fa", "ur"].includes(locale.split("-")[0]) ? "rtl" : "ltr";
  }, [locale]);
  const value = useMemo<LocalizationContextValue>(() => ({
    preference,
    locale,
    setLocale: (next) => {
      setPreference(next);
      window.localStorage.setItem(LOCALE_STORAGE_KEY, next);
    },
    t: translate,
    formatDate: (date, options) => new Intl.DateTimeFormat(locale, options).format(date),
    formatNumber: (number, options) => new Intl.NumberFormat(locale, options).format(number),
    formatCurrency: (number, currency) => new Intl.NumberFormat(locale, { style: "currency", currency }).format(number),
  }), [locale, preference]);

  return <LocalizationContext.Provider value={value}>{children}</LocalizationContext.Provider>;
}

export function useLocalization(): LocalizationContextValue {
  const context = useContext(LocalizationContext);
  if (!context) throw new Error("useLocalization must be used inside LocalizationProvider");
  return context;
}
