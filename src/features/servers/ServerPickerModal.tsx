import { useEffect, useMemo, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import type { LocationState, OfferCandidate, ServerPreferences } from "../../lib/types";

interface Props {
  open: boolean;
  onClose: () => void;
  offers: OfferCandidate[];
  selectedOfferId: number | null;
  location: LocationState;
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
  onUpdateServerPreferences: (payload: Partial<ServerPreferences>) => Promise<void>;
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

  if (days > 0) {
    return `${days}d ${remainingHours}h`;
  }

  return `${remainingHours}h`;
}

const GEOLOCATION_OPTIONS = [
  { code: "US", label: "United States" },
  { code: "CA", label: "Canada" },
  { code: "GB", label: "United Kingdom" },
  { code: "DE", label: "Germany" },
  { code: "FR", label: "France" },
  { code: "NL", label: "Netherlands" },
  { code: "SE", label: "Sweden" },
  { code: "NO", label: "Norway" },
  { code: "ES", label: "Spain" },
  { code: "IT", label: "Italy" },
  { code: "PL", label: "Poland" },
  { code: "JP", label: "Japan" },
  { code: "SG", label: "Singapore" },
  { code: "AU", label: "Australia" },
  { code: "BR", label: "Brazil" }
];

function resolveCountryName(code: string): string {
  const option = GEOLOCATION_OPTIONS.find((item) => item.code === code.toUpperCase());
  return option?.label ?? code.toUpperCase();
}

export function ServerPickerModal({
  open,
  onClose,
  offers,
  selectedOfferId,
  location,
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
  onUpdateServerPreferences
}: Props) {
  const [region, setRegion] = useState(location.region);
  const [geolocationCountryCode, setGeolocationCountryCode] = useState(
    serverPreferences.geolocationCountryCode || "US"
  );
  const [selectedStorage, setSelectedStorage] = useState(storageGb);
  const [minPrice, setMinPrice] = useState(serverPreferences.minHourlyPrice.toString());
  const [maxPrice, setMaxPrice] = useState(
    serverPreferences.maxHourlyPrice > 0 ? serverPreferences.maxHourlyPrice.toString() : ""
  );
  const [minGpuRamGb, setMinGpuRamGb] = useState(serverPreferences.minGpuRamGb.toString());
  const [minCpuCores, setMinCpuCores] = useState(serverPreferences.minCpuCores.toString());
  const [minDown, setMinDown] = useState(serverPreferences.minInetDownMbps.toString());
  const [minUp, setMinUp] = useState(serverPreferences.minInetUpMbps.toString());

  useEffect(() => {
    setRegion(location.region);
  }, [location.region]);

  useEffect(() => {
    setMinPrice(serverPreferences.minHourlyPrice.toString());
    setMaxPrice(serverPreferences.maxHourlyPrice > 0 ? serverPreferences.maxHourlyPrice.toString() : "");
    setMinGpuRamGb(serverPreferences.minGpuRamGb.toString());
    setMinCpuCores(serverPreferences.minCpuCores.toString());
    setMinDown(serverPreferences.minInetDownMbps.toString());
    setMinUp(serverPreferences.minInetUpMbps.toString());
    setGeolocationCountryCode(serverPreferences.geolocationCountryCode || "US");
  }, [
    serverPreferences.minHourlyPrice,
    serverPreferences.maxHourlyPrice,
    serverPreferences.minGpuRamGb,
    serverPreferences.minCpuCores,
    serverPreferences.minInetDownMbps,
    serverPreferences.minInetUpMbps,
    serverPreferences.geolocationCountryCode
  ]);

  const disabledSearch = useMemo(
    () => busy || searchingOffers || !geolocationCountryCode.trim(),
    [busy, searchingOffers, geolocationCountryCode]
  );

  if (!open) {
    return null;
  }

  async function runRegionCountrySearch() {
    await onManualLocationSave({
      city: "",
      region: region.trim(),
      country: resolveCountryName(geolocationCountryCode),
      latitude: 0,
      longitude: 0
    });

    // Save price filters
    const minPriceValue = parseFloat(minPrice) || 0;
    const maxPriceValue = parseFloat(maxPrice) || 0;
    await onUpdateServerPreferences({
      minHourlyPrice: minPriceValue,
      maxHourlyPrice: maxPriceValue,
      minGpuCount: 1,
      minGpuRamGb: Number.parseInt(minGpuRamGb, 10) || 0,
      minCpuCores: Number.parseFloat(minCpuCores) || 0,
      minInetDownMbps: Number.parseFloat(minDown) || 0,
      minInetUpMbps: Number.parseFloat(minUp) || 0,
      geolocationCountryCode,
      requireStaticIp: false
    });

    await onSearchOffers(1);
  }

  async function toggleVerified() {
    await onUpdateServerPreferences({
      requireVerified: !serverPreferences.requireVerified
    });
  }

  async function toggleDatacenter() {
    await onUpdateServerPreferences({
      requireDatacenter: !serverPreferences.requireDatacenter
    });
  }

  async function toggleOfferType(key: "includeOnDemand" | "includeInterruptible" | "includeReserved") {
    await onUpdateServerPreferences({
      [key]: !serverPreferences[key]
    });
  }

  async function toggleAvx() {
    await onUpdateServerPreferences({
      requireAvx: !serverPreferences.requireAvx
    });
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-[#02040bdd] p-4">
      <div className="glass-panel pixel-frame max-h-[92vh] w-full max-w-6xl overflow-hidden">
        <div className="flex items-center justify-between border-b-2 border-[#3e4270] px-5 py-4">
          <div>
            <h2
              className="pixel-heading glitch-title font-display text-sm text-white md:text-base"
              data-text="Select Server"
            >
              Select Server
            </h2>
            <p className="text-[1.25rem] leading-none text-[#b4c8de]">
              Search uses region plus Vast geolocation country code.
            </p>
          </div>
          <Button variant="ghost" onClick={onClose}>
            Close
          </Button>
        </div>

        <div className="max-h-[80vh] overflow-y-auto px-5 py-4">
          <div className="mb-4 grid gap-3 md:grid-cols-[1fr_1fr_auto_auto]">
            <InputField
              label="Region / State / Province"
              value={region}
              onChange={(event) => setRegion(event.target.value)}
              placeholder="California"
            />
            <div>
              <span className="block pb-1 text-[1.2rem] text-[#b4c8de]">Country</span>
              <select
                className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
                value={geolocationCountryCode}
                onChange={(event) => setGeolocationCountryCode(event.target.value)}
              >
                {GEOLOCATION_OPTIONS.map((option) => (
                  <option key={option.code} value={option.code}>
                    {option.label} ({option.code})
                  </option>
                ))}
              </select>
            </div>

            <div className="flex items-end gap-2">
              <span className="pb-1 text-[1.2rem] text-[#b4c8de]">Storage</span>
              <input
                className="h-11 w-24 border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-right text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
                type="number"
                min={30}
                max={4000}
                value={selectedStorage}
                onChange={(event) => setSelectedStorage(Number.parseInt(event.target.value, 10) || 100)}
              />
              <span className="pb-1 text-[1.2rem] text-[#b4c8de]">GB</span>
            </div>

            <div className="flex items-end">
              <Button variant="secondary" disabled={disabledSearch} onClick={runRegionCountrySearch}>
                {searchingOffers ? "Searching..." : "Find Offers"}
              </Button>
            </div>
          </div>

          {/* Price Range and Filters */}
          <div className="mb-4 grid gap-3 md:grid-cols-[1fr_1fr_auto_auto] border border-[#3e4270] p-3 rounded">
            <div>
              <span className="block pb-1 text-[1.2rem] text-[#b4c8de]">Min Price ($/hr)</span>
              <input
                className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
                type="number"
                min={0}
                step={0.001}
                placeholder="0.000"
                value={minPrice}
                onChange={(event) => setMinPrice(event.target.value)}
              />
            </div>
            <div>
              <span className="block pb-1 text-[1.2rem] text-[#b4c8de]">Max Price ($/hr)</span>
              <input
                className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
                type="number"
                min={0}
                step={0.001}
                placeholder="No limit"
                value={maxPrice}
                onChange={(event) => setMaxPrice(event.target.value)}
              />
            </div>

            <div className="flex items-end gap-4">
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={serverPreferences.requireVerified}
                  onChange={toggleVerified}
                  className="h-5 w-5 accent-neon-cyan"
                />
                <span className="text-[1.2rem] text-[#b4c8de]">Verified Only</span>
              </label>
            </div>

            <div className="flex items-end gap-4">
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={serverPreferences.requireDatacenter}
                  onChange={toggleDatacenter}
                  className="h-5 w-5 accent-neon-cyan"
                />
                <span className="text-[1.2rem] text-[#b4c8de]">Datacenter Only</span>
              </label>
            </div>
          </div>

          <div className="mb-4 grid gap-3 md:grid-cols-3 border border-[#3e4270] p-3 rounded">
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={serverPreferences.includeOnDemand}
                onChange={() => toggleOfferType("includeOnDemand")}
                className="h-5 w-5 accent-neon-cyan"
              />
              <span className="text-[1.2rem] text-[#b4c8de]">On-demand</span>
            </label>
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={serverPreferences.includeInterruptible}
                onChange={() => toggleOfferType("includeInterruptible")}
                className="h-5 w-5 accent-neon-cyan"
              />
              <span className="text-[1.2rem] text-[#b4c8de]">Interruptible</span>
            </label>
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={serverPreferences.includeReserved}
                onChange={() => toggleOfferType("includeReserved")}
                className="h-5 w-5 accent-neon-cyan"
              />
              <span className="text-[1.2rem] text-[#b4c8de]">Reserved</span>
            </label>
          </div>

          <div className="mb-4 grid gap-3 md:grid-cols-5 border border-[#3e4270] p-3 rounded">
            <div>
              <span className="block pb-1 text-[1.2rem] text-[#b4c8de]">Min GPU Count</span>
              <input
                className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
                type="number"
                min={1}
                max={1}
                step={1}
                value={1}
                disabled
              />
            </div>
            <div>
              <span className="block pb-1 text-[1.2rem] text-[#b4c8de]">Min VRAM (GB)</span>
              <input
                className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
                type="number"
                min={0}
                step={1}
                value={minGpuRamGb}
                onChange={(event) => setMinGpuRamGb(event.target.value)}
              />
            </div>
            <div>
              <span className="block pb-1 text-[1.2rem] text-[#b4c8de]">Min CPU Cores</span>
              <input
                className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
                type="number"
                min={0}
                step={0.5}
                value={minCpuCores}
                onChange={(event) => setMinCpuCores(event.target.value)}
              />
            </div>
            <div>
              <span className="block pb-1 text-[1.2rem] text-[#b4c8de]">Min Down (Mbps)</span>
              <input
                className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
                type="number"
                min={0}
                step={1}
                value={minDown}
                onChange={(event) => setMinDown(event.target.value)}
              />
            </div>
            <div>
              <span className="block pb-1 text-[1.2rem] text-[#b4c8de]">Min Up (Mbps)</span>
              <input
                className="h-11 w-full border border-[#3f476c] bg-[#0b0f23] px-2 py-1 text-[1.35rem] text-[#dff8ff] shadow-[inset_0_0_0_2px_#121731]"
                type="number"
                min={0}
                step={1}
                value={minUp}
                onChange={(event) => setMinUp(event.target.value)}
              />
            </div>
          </div>

          <div className="mb-4 grid gap-3 md:grid-cols-2 border border-[#3e4270] p-3 rounded">
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked
                disabled
                className="h-5 w-5 accent-neon-cyan"
              />
              <span className="text-[1.2rem] text-[#b4c8de]">Static IP required</span>
            </label>
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={serverPreferences.requireAvx}
                onChange={toggleAvx}
                className="h-5 w-5 accent-neon-cyan"
              />
              <span className="text-[1.2rem] text-[#b4c8de]">AVX CPU only</span>
            </label>
          </div>

          <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
            {offers.length === 0 ? (
              <Card className="col-span-full text-[1.3rem] text-[#b4c8de]">
                No offers yet. Select a country and click Find Offers.
              </Card>
            ) : (
              offers.map((offer) => {
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
                      <h3 className="font-display text-[11px] leading-[1.45] text-white">{offer.hostLabel}</h3>
                      <span className="border border-[#43508b] bg-[#1a2042] px-2 py-1 font-display text-[10px] text-[#9ad9ff]">
                        ${offer.hourlyPrice.toFixed(3)}/hr
                      </span>
                    </div>

                    <p className="mt-2 text-[1.45rem] leading-[1.02] text-neon-cyan">{offer.gpuName}</p>

                    {/* Host badges */}
                    <div className="mt-2 flex flex-wrap gap-1">
                      {offer.isVerified && (
                        <span className="border border-neon-lime/50 bg-neon-lime/10 px-1.5 py-0.5 text-[10px] text-neon-lime">
                          ✓ Verified
                        </span>
                      )}
                      {offer.isDatacenter && (
                        <span className="border border-[#5a7fb5]/50 bg-[#5a7fb5]/10 px-1.5 py-0.5 text-[10px] text-[#9ad9ff]">
                          🏢 Datacenter
                        </span>
                      )}
                      {!offer.isDatacenter && (
                        <span className="border border-[#6b6f92]/50 bg-[#6b6f92]/10 px-1.5 py-0.5 text-[10px] text-[#c2c6df]">
                          🧩 Community Host
                        </span>
                      )}
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
                      <p>Cores: {offer.cpuCores > 0 ? offer.cpuCores.toFixed(1) : "n/a"}</p>
                      <p>Down: {formatSpeed(offer.internetDownMbps)}</p>
                      <p>Up: {formatSpeed(offer.internetUpMbps)}</p>
                      <p>Reliability: {(offer.reliability * 100).toFixed(1)}%</p>
                    </div>

                    <Button
                      className="mt-3 w-full"
                      variant={isSelected ? "secondary" : "primary"}
                      disabled={busy}
                      onClick={() => onSelectOffer(offer.id, selectedStorage)}
                    >
                      {isSelected ? "Selected" : "Select"}
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
                onClick={onPreviousPage}
              >
                Prev Page
              </Button>
              <Button
                variant="secondary"
                disabled={busy || searchingOffers || !offersHasNextPage}
                onClick={onNextPage}
              >
                Next Page
              </Button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
