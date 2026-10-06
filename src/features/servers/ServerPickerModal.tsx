import { useEffect, useMemo, useRef, useState } from "react";
import { AIPromptHelper } from "../../components/ui/AIPromptHelper";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import type {
  OfferCandidate,
  OfferCountryAvailability,
  ServerPreferences,
} from "../../lib/types";
import { APP_PROMPTS } from "../../prompts/appPrompts";

interface Props {
  open: boolean;
  onClose: () => void;
  offers: OfferCandidate[];
  selectedOfferId: number | null;
  serverPreferences: ServerPreferences;
  storageGb: number;
  availableCountries: OfferCountryAvailability[];
  searchingOffers: boolean;
  offersPage: number;
  offersHasNextPage: boolean;
  busy: boolean;
  onSearchOffers: (page?: number) => Promise<void>;
  onNextPage: () => Promise<void>;
  onPreviousPage: () => Promise<void>;
  onManualLocationSave: (payload: {
    city: string;
    region: string;
    country: string;
    latitude: number;
    longitude: number;
  }) => Promise<void>;
  onSelectOffer: (offerId: number, storageGb: number) => Promise<void>;
  onUpdateServerPreferences: (
    payload: Partial<ServerPreferences>,
  ) => Promise<void>;
}

type CountryOption = {
  code: string;
  label: string;
  offerCount: number | null;
};

type SortMode =
  | "recommended"
  | "priceAsc"
  | "priceDesc"
  | "reliabilityDesc"
  | "reliabilityAsc";

const GLOBAL_COUNTRY_CODE = "GLOBAL";
const MIN_STORAGE_GB = 30;

/* Fallback shown while Vast availability is loading (or when it fails). */
const FALLBACK_COUNTRIES: CountryOption[] = [
  { code: GLOBAL_COUNTRY_CODE, label: "Global", offerCount: null },
  { code: "AU", label: "Australia", offerCount: null },
  { code: "BR", label: "Brazil", offerCount: null },
  { code: "CA", label: "Canada", offerCount: null },
  { code: "FR", label: "France", offerCount: null },
  { code: "DE", label: "Germany", offerCount: null },
  { code: "IT", label: "Italy", offerCount: null },
  { code: "JP", label: "Japan", offerCount: null },
  { code: "NL", label: "Netherlands", offerCount: null },
  { code: "NO", label: "Norway", offerCount: null },
  { code: "PL", label: "Poland", offerCount: null },
  { code: "SG", label: "Singapore", offerCount: null },
  { code: "ES", label: "Spain", offerCount: null },
  { code: "SE", label: "Sweden", offerCount: null },
  { code: "GB", label: "United Kingdom", offerCount: null },
  { code: "US", label: "United States", offerCount: null },
];

function countryLabel(code: string): string {
  if (!code || code.toUpperCase() === GLOBAL_COUNTRY_CODE) {
    return "Global";
  }

  try {
    const displayNames = new Intl.DisplayNames(["en"], { type: "region" });
    const label = displayNames.of(code.toUpperCase());
    if (label && label !== code.toUpperCase()) {
      return label;
    }
  } catch {
    // Older webviews fall through to the raw code.
  }
  return code.toUpperCase();
}

function formatHourlyPrice(price: number): string {
  if (!Number.isFinite(price) || price <= 0) {
    return "n/a";
  }

  return `$${price.toFixed(4)}/hr`;
}

function parseOptionalNumber(value: string): number | null {
  if (value.trim().length === 0) {
    return null;
  }

  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : null;
}

function clampPercent(value: number): number {
  return Math.min(100, Math.max(0, value));
}

function normalizedRange(minValue: string, maxValue: string, clamp?: (value: number) => number) {
  const rawMin = parseOptionalNumber(minValue);
  const rawMax = parseOptionalNumber(maxValue);
  const min = rawMin === null ? null : clamp ? clamp(rawMin) : rawMin;
  const max = rawMax === null ? null : clamp ? clamp(rawMax) : rawMax;

  if (min !== null && max !== null && min > max) {
    return { min: max, max: min, swapped: true };
  }

  return { min, max, swapped: false };
}

function sortModeLabel(sortMode: SortMode): string {
  switch (sortMode) {
    case "priceAsc":
      return "lowest price first";
    case "priceDesc":
      return "highest price first";
    case "reliabilityDesc":
      return "most reliable first";
    case "reliabilityAsc":
      return "least reliable first";
    case "recommended":
    default:
      return "recommended order";
  }
}

function filterButtonVariant(active: boolean): "ghost" | "secondary" {
  return active ? "secondary" : "ghost";
}

function formatSpeed(mbps: number): string {
  if (!Number.isFinite(mbps) || mbps <= 0) {
    return "n/a";
  }

  return `${Math.round(mbps)} Mbps`;
}

function formatTimeRemaining(hours: number): string {
  if (hours <= 0) {
    return "Unknown";
  }

  const days = Math.floor(hours / 24);
  const remainingHours = Math.floor(hours % 24);
  return days > 0 ? `${days}d ${remainingHours}h` : `${remainingHours}h`;
}


export function ServerPickerModal({
  open,
  onClose,
  offers,
  selectedOfferId,
  serverPreferences,
  storageGb,
  availableCountries,
  searchingOffers,
  offersPage,
  offersHasNextPage,
  busy,
  onSearchOffers,
  onNextPage,
  onPreviousPage,
  onManualLocationSave,
  onSelectOffer,
  onUpdateServerPreferences,
}: Props) {
  const [countryCode, setCountryCode] = useState(
    serverPreferences.geolocationCountryCode || GLOBAL_COUNTRY_CODE,
  );
  const [sortMode, setSortMode] = useState<SortMode>("recommended");
  const [minPriceInput, setMinPriceInput] = useState("");
  const [maxPriceInput, setMaxPriceInput] = useState("");
  const [minReliabilityInput, setMinReliabilityInput] = useState("");
  const [maxReliabilityInput, setMaxReliabilityInput] = useState("");
  const [storageInput, setStorageInput] = useState(
    String(serverPreferences.storageGb || storageGb || ""),
  );
  const storageInputRef = useRef(storageInput);
  const [pendingOfferId, setPendingOfferId] = useState<number | null>(null);

  const countryOptions = useMemo<CountryOption[]>(() => {
    if (availableCountries.length === 0) {
      return FALLBACK_COUNTRIES;
    }
    const mapped: CountryOption[] = availableCountries
      .map(({ code, offerCount }) => ({
        code: code.toUpperCase(),
        label: countryLabel(code),
        offerCount,
      }))
      .filter((option) => option.code.length === 2);
    const globalCount = mapped.reduce(
      (total, option) => total + (option.offerCount ?? 0),
      0,
    );
    const current = (
      serverPreferences.geolocationCountryCode || GLOBAL_COUNTRY_CODE
    ).toUpperCase();
    if (
      current &&
      current !== GLOBAL_COUNTRY_CODE &&
      !mapped.some((option) => option.code === current)
    ) {
      mapped.push({ code: current, label: countryLabel(current), offerCount: 0 });
    }
    const sorted = mapped.sort((left, right) =>
      left.label.localeCompare(right.label),
    );
    return [
      { code: GLOBAL_COUNTRY_CODE, label: "Global", offerCount: globalCount },
      ...sorted,
    ];
  }, [availableCountries, serverPreferences.geolocationCountryCode]);

  useEffect(() => {
    setCountryCode(serverPreferences.geolocationCountryCode || GLOBAL_COUNTRY_CODE);
  }, [serverPreferences.geolocationCountryCode]);

  useEffect(() => {
    setStorageInput(String(serverPreferences.storageGb || ""));
  }, [serverPreferences.storageGb]);

  const activeFilterCount = useMemo(
    () =>
      [minPriceInput, maxPriceInput, minReliabilityInput, maxReliabilityInput].filter(
        (value) => value.trim().length > 0,
      ).length,
    [maxPriceInput, maxReliabilityInput, minPriceInput, minReliabilityInput],
  );

  const priceRange = useMemo(
    () => normalizedRange(minPriceInput, maxPriceInput),
    [maxPriceInput, minPriceInput],
  );

  const reliabilityRange = useMemo(
    () => normalizedRange(minReliabilityInput, maxReliabilityInput, clampPercent),
    [maxReliabilityInput, minReliabilityInput],
  );

  const displayedOffers = useMemo(() => {
    const filtered = offers.filter((offer) => {
      const reliabilityPercent = offer.reliability * 100;
      if (priceRange.min !== null && offer.hourlyPrice < priceRange.min) {
        return false;
      }
      if (priceRange.max !== null && offer.hourlyPrice > priceRange.max) {
        return false;
      }
      if (
        reliabilityRange.min !== null &&
        reliabilityPercent < reliabilityRange.min
      ) {
        return false;
      }
      if (
        reliabilityRange.max !== null &&
        reliabilityPercent > reliabilityRange.max
      ) {
        return false;
      }
      return true;
    });

    return [...filtered].sort((left, right) => {
      switch (sortMode) {
        case "priceAsc":
          return left.hourlyPrice - right.hourlyPrice;
        case "priceDesc":
          return right.hourlyPrice - left.hourlyPrice;
        case "reliabilityDesc":
          return right.reliability - left.reliability;
        case "reliabilityAsc":
          return left.reliability - right.reliability;
        case "recommended":
        default:
          return right.score - left.score;
      }
    });
  }, [
    offers,
    priceRange.max,
    priceRange.min,
    reliabilityRange.max,
    reliabilityRange.min,
    sortMode,
  ]);

  const commitStorageInput = async () => {
    const parsed = Number(storageInputRef.current);
    if (!Number.isFinite(parsed) || parsed <= 0) {
      return;
    }
    const clamped = Math.min(10000, Math.max(MIN_STORAGE_GB, Math.round(parsed)));
    await onUpdateServerPreferences({ storageGb: clamped });
  };

  async function confirmProvisioning() {
    if (pendingOfferId === null) {
      return;
    }

    const parsed = Number(storageInputRef.current);
    const effectiveStorage =
      Number.isFinite(parsed) && parsed > 0
        ? Math.min(10000, Math.max(MIN_STORAGE_GB, Math.round(parsed)))
        : storageGb;

    await commitStorageInput();
    const offerId = pendingOfferId;
    setPendingOfferId(null);
    await onSelectOffer(offerId, effectiveStorage);
  }

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape" && open) {
        onClose();
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [open, onClose]);


  if (!open) {
    return null;
  }

  async function runCountrySearch() {
    const isGlobal = countryCode === GLOBAL_COUNTRY_CODE;

    if (!isGlobal) {
      await onManualLocationSave({
        city: "",
        region: "",
        country: countryLabel(countryCode),
        latitude: 0,
        longitude: 0,
      });
    }

    await onUpdateServerPreferences({
      geolocationCountryCode: isGlobal ? GLOBAL_COUNTRY_CODE : countryCode,
    });

    await onSearchOffers(1);
  }

  return (
    <ModalFrame panelClassName="glass-panel pixel-frame max-w-6xl">
      <div className="flex shrink-0 items-center justify-between border-b-2 border-[#3e4270] px-5 py-4">
        <div className="flex items-center gap-3">
          <div>
            <h2
              className="pixel-heading glitch-title font-display text-sm text-white md:text-base"
              data-text="Select Server"
            >
              Select Server
            </h2>
            <p className="text-[1.05rem] leading-none text-[#b4c8de]">
              Choose a country and server.
            </p>
          </div>
          <AIPromptHelper
            topic="Server Selection Market"
            promptText={APP_PROMPTS.serverPickerModalHeader}
            variant="icon"
          />
        </div>
        <Button variant="ghost" onClick={onClose}>
          Close
        </Button>
      </div>

      <ModalBody className="px-5 py-4">
        {(busy || searchingOffers) && (
          <p
            className="mb-4 text-[1.1rem] text-[#9ec4df]"
            aria-live="polite"
            aria-busy="true"
          >
            {searchingOffers
              ? "Searching available offers..."
              : "Updating country search..."}
          </p>
        )}

        <div className="mb-3 grid gap-3 rounded border border-[#3e4270] p-3 md:grid-cols-[minmax(14rem,1fr)_auto] md:items-end">
          <label className="flex min-w-0 flex-col justify-end">
            <span className="block pb-1 text-[1.2rem] leading-none text-[#b4c8de]">
              Country
            </span>
            <select
              className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
              value={countryCode}
              onChange={(event) => setCountryCode(event.target.value)}
            >
              {countryOptions.map((option) => (
                <option key={option.code} value={option.code}>
                  {option.label} ({option.code})
                  {option.offerCount !== null
                    ? ` · ${option.offerCount.toLocaleString()} offers`
                    : ""}
                </option>
              ))}
            </select>
          </label>

          <Button
            variant="secondary"
            className="h-11"
            disabled={busy || searchingOffers || !countryCode}
            loading={searchingOffers}
            loadingText="Searching..."
            onClick={runCountrySearch}
          >
            Find Offers
          </Button>
        </div>

        <details className="mb-3 rounded border border-[#3e4270] bg-[#0b0f23]/50 p-3">
          <summary className="cursor-pointer list-none">
            <div className="flex items-center justify-between gap-3">
              <div>
                <p className="font-display text-[10px] uppercase tracking-[0.12em] text-[#9ad9ff]">
                  ▾ Advanced filters
                </p>
                <p className="mt-1 text-[1rem] leading-none text-[#9ec4df]">
                  {activeFilterCount} filter{activeFilterCount === 1 ? "" : "s"} · {sortModeLabel(sortMode)}
                </p>
              </div>
              <span className="border border-[#3f476c] px-3 py-2 font-display text-[10px] uppercase tracking-[0.12em] text-[#9ec4df]">
                Show
              </span>
            </div>
          </summary>

          <div className="mt-3 flex justify-end">
            <Button
              variant="ghost"
              onClick={() => {
                setSortMode("recommended");
                setMinPriceInput("");
                setMaxPriceInput("");
                setMinReliabilityInput("");
                setMaxReliabilityInput("");
              }}
            >
              Reset filters
            </Button>
          </div>

          <div className="mt-3 grid gap-2 md:grid-cols-2 xl:grid-cols-4">
            <div className="rounded border border-[#3e4270] bg-[#0b0f23]/70 p-3">
              <p className="font-display text-[10px] uppercase tracking-[0.12em] text-[#9ad9ff]">
                1. Sort by price
              </p>
              <div className="mt-3 grid gap-2">
                <Button
                  variant={sortMode === "priceAsc" ? "secondary" : "ghost"}
                  onClick={() => setSortMode("priceAsc")}
                >
                  Lowest price first
                </Button>
                <Button
                  variant={sortMode === "priceDesc" ? "secondary" : "ghost"}
                  onClick={() => setSortMode("priceDesc")}
                >
                  Highest price first
                </Button>
              </div>
            </div>

            <div className="rounded border border-[#3e4270] bg-[#0b0f23]/70 p-3">
              <p className="font-display text-[10px] uppercase tracking-[0.12em] text-[#9ad9ff]">
                2. Sort by reliability
              </p>
              <div className="mt-3 grid gap-2">
                <Button
                  variant={sortMode === "reliabilityDesc" ? "secondary" : "ghost"}
                  onClick={() => setSortMode("reliabilityDesc")}
                >
                  Most reliable first
                </Button>
                <Button
                  variant={sortMode === "reliabilityAsc" ? "secondary" : "ghost"}
                  onClick={() => setSortMode("reliabilityAsc")}
                >
                  Least reliable first
                </Button>
              </div>
            </div>

            <div className="rounded border border-[#3e4270] bg-[#0b0f23]/70 p-3">
              <p className="font-display text-[10px] uppercase tracking-[0.12em] text-[#9ad9ff]">
                3. Filter by price
              </p>
              {priceRange.swapped && (
                <p className="mt-1 text-[1rem] leading-none text-[#ffd78a]">
                  Min/max price are reversed, using ${priceRange.min?.toFixed(2)}–${priceRange.max?.toFixed(2)}.
                </p>
              )}
              <div className="mt-3 grid grid-cols-2 gap-2">
                <label className="text-[1.05rem] text-[#b4c8de]">
                  Min total $/hr
                  <input
                    type="number"
                    min={0}
                    step={0.01}
                    placeholder="0.20"
                    className="mt-1 h-9 w-full border border-[#3f476c] bg-[#0b0f23] px-2 text-[1.15rem] text-[#dff8ff]"
                    value={minPriceInput}
                    onChange={(event) => setMinPriceInput(event.target.value)}
                  />
                </label>
                <label className="text-[1.05rem] text-[#b4c8de]">
                  Max total $/hr
                  <input
                    type="number"
                    min={0}
                    step={0.01}
                    placeholder="0.60"
                    className="mt-1 h-9 w-full border border-[#3f476c] bg-[#0b0f23] px-2 text-[1.15rem] text-[#dff8ff]"
                    value={maxPriceInput}
                    onChange={(event) => setMaxPriceInput(event.target.value)}
                  />
                </label>
              </div>
              <div className="mt-3 grid grid-cols-3 gap-2">
                <Button
                  variant={filterButtonVariant(minPriceInput === "" && maxPriceInput === "0.30")}
                  onClick={() => {
                    setMinPriceInput("");
                    setMaxPriceInput("0.30");
                  }}
                >
                  Under $0.30
                </Button>
                <Button
                  variant={filterButtonVariant(minPriceInput === "" && maxPriceInput === "0.50")}
                  onClick={() => {
                    setMinPriceInput("");
                    setMaxPriceInput("0.50");
                  }}
                >
                  Under $0.50
                </Button>
                <Button
                  variant={filterButtonVariant(minPriceInput === "" && maxPriceInput === "")}
                  onClick={() => {
                    setMinPriceInput("");
                    setMaxPriceInput("");
                  }}
                >
                  Clear
                </Button>
              </div>
            </div>

            <div className="rounded border border-[#3e4270] bg-[#0b0f23]/70 p-3">
              <p className="font-display text-[10px] uppercase tracking-[0.12em] text-[#9ad9ff]">
                4. Filter by reliability
              </p>
              {reliabilityRange.swapped && (
                <p className="mt-1 text-[1rem] leading-none text-[#ffd78a]">
                  Min/max reliability are reversed, using {reliabilityRange.min?.toFixed(0)}–{reliabilityRange.max?.toFixed(0)}%.
                </p>
              )}
              <div className="mt-3 grid grid-cols-2 gap-2">
                <label className="text-[1.05rem] text-[#b4c8de]">
                  Min reliability %
                  <input
                    type="number"
                    min={0}
                    max={100}
                    step={1}
                    placeholder="95"
                    className="mt-1 h-9 w-full border border-[#3f476c] bg-[#0b0f23] px-2 text-[1.15rem] text-[#dff8ff]"
                    value={minReliabilityInput}
                    onChange={(event) => setMinReliabilityInput(event.target.value)}
                  />
                </label>
                <label className="text-[1.05rem] text-[#b4c8de]">
                  Max reliability %
                  <input
                    type="number"
                    min={0}
                    max={100}
                    step={1}
                    placeholder="100"
                    className="mt-1 h-9 w-full border border-[#3f476c] bg-[#0b0f23] px-2 text-[1.15rem] text-[#dff8ff]"
                    value={maxReliabilityInput}
                    onChange={(event) => setMaxReliabilityInput(event.target.value)}
                  />
                </label>
              </div>
              <div className="mt-3 grid grid-cols-3 gap-2">
                <Button
                  variant={filterButtonVariant(minReliabilityInput === "95" && maxReliabilityInput === "")}
                  onClick={() => {
                    setMinReliabilityInput("95");
                    setMaxReliabilityInput("");
                  }}
                >
                  95%+
                </Button>
                <Button
                  variant={filterButtonVariant(minReliabilityInput === "98" && maxReliabilityInput === "")}
                  onClick={() => {
                    setMinReliabilityInput("98");
                    setMaxReliabilityInput("");
                  }}
                >
                  98%+
                </Button>
                <Button
                  variant={filterButtonVariant(minReliabilityInput === "" && maxReliabilityInput === "")}
                  onClick={() => {
                    setMinReliabilityInput("");
                    setMaxReliabilityInput("");
                  }}
                >
                  Clear
                </Button>
              </div>
            </div>
          </div>
        </details>

        <p className="mb-3 text-[1rem] text-[#9ec4df]" aria-live="polite">
          Showing {displayedOffers.length} of {offers.length} · Page {offersPage}
        </p>

        <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
          {displayedOffers.length === 0 ? (
            <Card className="col-span-full text-[1.3rem] text-[#b4c8de]">
              No offers yet. Select a country and click Find Offers.
            </Card>
          ) : (
            displayedOffers.map((offer) => {
              const isSelected = offer.id === selectedOfferId;
              return (
                <Card
                  key={offer.id}
                  className={`border-2 transition ${
                    isSelected
                      ? "border-neon-lime shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#304126]"
                      : "border-[#3a4068]"
                  }`}
                >
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex items-center gap-1.5">
                      <h3 className="font-display text-[11px] leading-[1.45] text-white">
                        {offer.hostLabel}
                      </h3>
                      <AIPromptHelper
                        topic={`Instance Offering ${offer.hostLabel}`}
                        promptText={APP_PROMPTS.serverInstanceCard}
                        variant="icon"
                      />
                    </div>
                    <span className="border border-[#43508b] bg-[#1a2042] px-3 py-2 text-right font-display text-[13px] leading-none text-[#9ad9ff] shadow-[0_0_14px_rgba(154,217,255,0.12)]">
                      <span className="block text-[7px] uppercase tracking-[0.12em] text-[#7fa8cc]">
                        Total
                      </span>
                      {formatHourlyPrice(offer.hourlyPrice)}
                    </span>
                  </div>

                  <p className="mt-2 text-[1.45rem] leading-[1.02] text-neon-cyan">
                    {offer.gpuName}
                  </p>

                  <div className="mt-2 flex flex-wrap gap-1">
                    {offer.isVerified && (
                      <span className="border border-neon-lime/50 bg-neon-lime/10 px-1.5 py-0.5 text-[10px] text-neon-lime">
                        ✓ Verified
                      </span>
                    )}
                    <span className="border border-[#5a7fb5]/50 bg-[#5a7fb5]/10 px-1.5 py-0.5 text-[10px] text-[#9ad9ff]">
                      {offer.isDatacenter ? "🏢 Datacenter" : "🧩 Community Host"}
                    </span>
                    <span className="border border-[#f2b84a]/50 bg-[#f2b84a]/10 px-1.5 py-0.5 text-[10px] text-[#ffd78a]">
                      {offer.offerType || "on-demand"}
                    </span>
                    {offer.hasStaticIp && (
                      <span className="border border-[#6ae6ce]/50 bg-[#6ae6ce]/10 px-1.5 py-0.5 text-[10px] text-[#8df1df]">
                        🌐 Static IP
                      </span>
                    )}
                    {offer.hasAvx && (
                      <span className="border border-[#8ca8ff]/50 bg-[#8ca8ff]/10 px-1.5 py-0.5 text-[10px] text-[#b9c8ff]">
                        ⚙ AVX
                      </span>
                    )}
                    {offer.timeRemainingHours > 0 && (
                      <span className="border border-[#ffa500]/50 bg-[#ffa500]/10 px-1.5 py-0.5 text-[10px] text-[#ffa500]">
                        ⏱ {formatTimeRemaining(offer.timeRemainingHours)} left
                      </span>
                    )}
                  </div>

                  <div className="mt-3 grid grid-cols-2 gap-2 text-[1.2rem] leading-none text-[#c6dbf4]">
                    <p>Location: {offer.locationLabel}</p>
                    <p>VRAM: {(offer.gpuRamMb / 1024).toFixed(1)} GB</p>
                    <p>GPU count: {offer.gpuCount}</p>
                    <p>CPU: {offer.cpuName || "n/a"}</p>
                    <p>
                      Cores: {offer.cpuCores > 0 ? offer.cpuCores.toFixed(1) : "n/a"}
                    </p>
                    <p>Down: {formatSpeed(offer.internetDownMbps)}</p>
                    <p>Up: {formatSpeed(offer.internetUpMbps)}</p>
                    <p>Reliability: {(offer.reliability * 100).toFixed(1)}%</p>
                  </div>

                  <Button
                    className="mt-3 w-full"
                    variant={isSelected ? "secondary" : "primary"}
                    disabled={busy}
                    loading={busy && isSelected}
                    loadingText="Provisioning..."
                     onClick={() => {
                       const nextStorage = String(serverPreferences.storageGb || storageGb || "");
                       setStorageInput(nextStorage);
                       storageInputRef.current = nextStorage;
                       setPendingOfferId(offer.id);
                     }}
                  >
                    {isSelected ? "Provision" : "Select & provision"}
                  </Button>
                </Card>
              );
            })
          )}
        </div>

        <div className="mt-4 flex items-center justify-between gap-3 border-t border-[#3e4270] pt-3">
          <p className="font-display text-[10px] uppercase tracking-[0.12em] text-[#9ec4df]">
            Market Page {offersPage}
          </p>
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              disabled={busy || searchingOffers || offersPage <= 1}
              loading={searchingOffers}
              loadingText="Loading..."
              onClick={onPreviousPage}
            >
              Prev Page
            </Button>
            <Button
              variant="secondary"
              disabled={busy || searchingOffers || !offersHasNextPage}
              loading={searchingOffers}
              loadingText="Loading..."
              onClick={onNextPage}
            >
              Next Page
            </Button>
          </div>
        </div>

        {pendingOfferId !== null && (
          <ModalFrame
            panelClassName="glass-panel pixel-frame max-w-md"
            zIndexClassName="z-[60]"
            labelledBy="storage-picker-title"
          >
            <ModalBody className="p-5">
              <p className="font-display text-[10px] uppercase tracking-[0.14em] text-neon-cyan">
                Provisioning
              </p>
              <h3 id="storage-picker-title" className="mt-1 font-display text-base text-white">
                Choose storage
              </h3>
              <p className="mt-2 text-[1rem] leading-[1.3] text-[#b4c8de]">
                Set disk space for this instance.
              </p>
              <label className="mt-4 flex flex-col gap-1.5">
                <span className="font-display text-[11px] uppercase tracking-[0.1em] text-[#9ad9ff]">
                  Storage (GB)
                </span>
                <input
                  autoFocus
                  type="number"
                  min={MIN_STORAGE_GB}
                  max={10000}
                  step={1}
                  title="Minimum 30 GB · Maximum 10,000 GB"
                  className="min-h-11 border border-[#3f476c] bg-[#0b0f23] px-3 py-2 text-[1.1rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
                  value={storageInput}
                  onChange={(event) => {
                    const raw = event.target.value;
                    setStorageInput(raw);
                    storageInputRef.current = raw;
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      void confirmProvisioning();
                    }
                  }}
                />
                <span className="text-[0.95rem] text-[#7fa8cc]">
                  30–10,000 GB
                </span>
              </label>
              <div className="mt-5 flex justify-end gap-2">
                <Button variant="ghost" onClick={() => setPendingOfferId(null)}>
                  Cancel
                </Button>
                <Button
                  variant="secondary"
                  loading={busy}
                  loadingText="Provisioning..."
                  onClick={() => void confirmProvisioning()}
                >
                  Provision
                </Button>
              </div>
            </ModalBody>
          </ModalFrame>
        )}
      </ModalBody>
    </ModalFrame>
  );
}
