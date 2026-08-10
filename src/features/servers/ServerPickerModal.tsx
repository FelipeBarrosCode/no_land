import { useEffect, useMemo, useState } from "react";
import { AIPromptHelper } from "../../components/ui/AIPromptHelper";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import type { OfferCandidate, ServerPreferences } from "../../lib/types";
import { APP_PROMPTS } from "../../prompts/appPrompts";

interface Props {
  open: boolean;
  onClose: () => void;
  offers: OfferCandidate[];
  selectedOfferId: number | null;
  serverPreferences: ServerPreferences;
  storageGb: number;
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
};

const FALLBACK_COUNTRIES: CountryOption[] = [
  { code: "AU", label: "Australia" },
  { code: "BR", label: "Brazil" },
  { code: "CA", label: "Canada" },
  { code: "FR", label: "France" },
  { code: "DE", label: "Germany" },
  { code: "IT", label: "Italy" },
  { code: "JP", label: "Japan" },
  { code: "NL", label: "Netherlands" },
  { code: "NO", label: "Norway" },
  { code: "PL", label: "Poland" },
  { code: "SG", label: "Singapore" },
  { code: "ES", label: "Spain" },
  { code: "SE", label: "Sweden" },
  { code: "GB", label: "United Kingdom" },
  { code: "US", label: "United States" },
];

const COUNTRY_OPTIONS = buildCountryOptions();

function buildCountryOptions(): CountryOption[] {
  try {
    const displayNames = new Intl.DisplayNames(["en"], { type: "region" });
    const countries: CountryOption[] = [];

    for (let first = 65; first <= 90; first += 1) {
      for (let second = 65; second <= 90; second += 1) {
        const code = String.fromCharCode(first, second);
        const label = displayNames.of(code);
        if (label && label !== code && !label.startsWith("Unknown Region")) {
          countries.push({ code, label });
        }
      }
    }

    if (countries.length > 100) {
      return countries.sort((left, right) =>
        left.label.localeCompare(right.label),
      );
    }
  } catch {
    // Older webviews use the stable fallback list below.
  }

  return FALLBACK_COUNTRIES;
}

function resolveCountryName(code: string): string {
  return (
    COUNTRY_OPTIONS.find((item) => item.code === code.toUpperCase())?.label ??
    code.toUpperCase()
  );
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

function offerSearchDocument(offer: OfferCandidate): string {
  const labels = [
    offer.isVerified ? "verified" : "unverified",
    offer.isDatacenter ? "datacenter" : "community host",
    offer.hasStaticIp ? "static ip" : "dynamic ip",
    offer.hasAvx ? "avx" : "no avx",
  ];

  return [
    offer.id,
    offer.hostId,
    offer.hostLabel,
    offer.locationLabel,
    offer.city,
    offer.region,
    offer.country,
    offer.gpuName,
    offer.gpuRamMb,
    offer.gpuCount,
    offer.cpuName,
    offer.cpuCores,
    offer.internetDownMbps,
    offer.internetUpMbps,
    offer.hourlyPrice,
    offer.availableStorageGb,
    offer.estimatedDistanceKm,
    offer.reliability,
    offer.score,
    offer.timeRemainingHours,
    offer.offerType,
    ...labels,
  ]
    .filter((value) => value !== null && value !== undefined)
    .join(" ")
    .toLocaleLowerCase();
}

export function ServerPickerModal({
  open,
  onClose,
  offers,
  selectedOfferId,
  serverPreferences,
  storageGb,
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
    serverPreferences.geolocationCountryCode || "US",
  );
  const [fullTextQuery, setFullTextQuery] = useState("");

  useEffect(() => {
    setCountryCode(serverPreferences.geolocationCountryCode || "US");
  }, [serverPreferences.geolocationCountryCode]);

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape" && open) {
        onClose();
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [open, onClose]);

  const filteredOffers = useMemo(() => {
    const terms = fullTextQuery
      .trim()
      .toLocaleLowerCase()
      .split(/\s+/u)
      .filter(Boolean);

    if (terms.length === 0) {
      return offers;
    }

    return offers.filter((offer) => {
      const document = offerSearchDocument(offer);
      return terms.every((term) => document.includes(term));
    });
  }, [fullTextQuery, offers]);

  if (!open) {
    return null;
  }

  async function runCountrySearch() {
    await onManualLocationSave({
      city: "",
      region: "",
      country: resolveCountryName(countryCode),
      latitude: 0,
      longitude: 0,
    });

    await onUpdateServerPreferences({
      minReliability: 0.8,
      minHourlyPrice: 0,
      maxHourlyPrice: 0,
      requireVerified: false,
      requireDatacenter: false,
      includeOnDemand: true,
      includeInterruptible: true,
      includeReserved: true,
      requireStaticIp: false,
      requireAvx: false,
      minGpuCount: 1,
      minGpuRamGb: 0,
      minCpuCores: 0,
      minInetDownMbps: 0,
      minInetUpMbps: 0,
      geolocationCountryCode: countryCode,
    });

    setFullTextQuery("");
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
            <p className="text-[1.25rem] leading-none text-[#b4c8de]">
              Search the market by country, then filter the returned servers by
              any text.
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

        <div className="mb-4 grid gap-3 rounded border border-[#3e4270] p-3 md:grid-cols-[minmax(14rem,0.8fr)_auto_minmax(16rem,1.2fr)] md:items-end">
          <label>
            <span className="block pb-1 text-[1.2rem] text-[#b4c8de]">
              Country
            </span>
            <select
              className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
              value={countryCode}
              onChange={(event) => setCountryCode(event.target.value)}
            >
              {COUNTRY_OPTIONS.map((option) => (
                <option key={option.code} value={option.code}>
                  {option.label} ({option.code})
                </option>
              ))}
            </select>
          </label>

          <Button
            variant="secondary"
            disabled={busy || searchingOffers || !countryCode}
            loading={searchingOffers}
            loadingText="Searching..."
            onClick={runCountrySearch}
          >
            Find Offers
          </Button>

          <label>
            <span className="block pb-1 text-[1.2rem] text-[#b4c8de]">
              Search returned offers
            </span>
            <input
              className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-3 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731] placeholder:text-[#647695]"
              type="search"
              value={fullTextQuery}
              onChange={(event) => setFullTextQuery(event.target.value)}
              placeholder="State, city, GPU, CPU, host, price..."
              aria-label="Search all returned offer fields"
            />
          </label>
        </div>

        <p className="mb-3 text-[1.05rem] text-[#9ec4df]" aria-live="polite">
          Showing {filteredOffers.length} of {offers.length} returned offers on
          market page {offersPage}.
        </p>

        <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
          {offers.length === 0 ? (
            <Card className="col-span-full text-[1.3rem] text-[#b4c8de]">
              No offers yet. Select a country and click Find Offers.
            </Card>
          ) : filteredOffers.length === 0 ? (
            <Card className="col-span-full text-[1.3rem] text-[#b4c8de]">
              No returned offers match “{fullTextQuery.trim()}”. Try a broader
              search or load another market page.
            </Card>
          ) : (
            filteredOffers.map((offer) => {
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
                    <span className="border border-[#43508b] bg-[#1a2042] px-2 py-1 font-display text-[10px] text-[#9ad9ff]">
                      ${offer.hourlyPrice.toFixed(3)}/hr
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
                    onClick={() => onSelectOffer(offer.id, storageGb)}
                  >
                    {isSelected ? "Provisioning" : "Select & Provision"}
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
      </ModalBody>
    </ModalFrame>
  );
}
