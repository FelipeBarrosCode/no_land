import { create } from "zustand";
import {
  completeOnboarding,
  getAppState,
  getRentedInstances,
  getProvisioningLogs,
  searchOffers,
  selectOffer,
  setManualLocation,
  setupWireguardClient,
  startPlayExistingInstance,
  startPlayFlow,
  submitPairingPin,
  skipPairingAndContinue,
  subscribeProvisioningEvents,
  updatePlatformCredentials,
  updateMoonlightPreferences,
  updateServerPreferences,
  updateSshCredentials,
  updateVastApiKey
} from "../lib/backend";
import type {
  ManualLocationInput,
  MoonlightPreferences,
  OfferCandidate,
  OnboardingPayload,
  PlatformCredentialsUpdate,
  PersistedAppState,
  ProvisioningEvent,
  RentedInstanceSummary,
  ServerPreferencesUpdate,
  SshCredentialsUpdate
} from "../lib/types";

interface AppStore {
  appState: PersistedAppState | null;
  offers: OfferCandidate[];
  rentedInstances: RentedInstanceSummary[];
  logs: ProvisioningEvent[];
  loading: boolean;
  searching: boolean;
  offersPage: number;
  offersPageSize: number;
  offersHasNextPage: boolean;
  busy: boolean;
  serverPickerOpen: boolean;
  error: string | null;
  _eventsBound: boolean;
  initialize: () => Promise<void>;
  bindEvents: () => Promise<void>;
  setServerPickerOpen: (open: boolean) => void;
  runOnboarding: (payload: OnboardingPayload) => Promise<void>;
  saveManualLocation: (payload: ManualLocationInput) => Promise<void>;
  discoverOffers: (page?: number) => Promise<void>;
  nextOffersPage: () => Promise<void>;
  previousOffersPage: () => Promise<void>;
  chooseOffer: (offerId: number, storageGb: number) => Promise<void>;
  startPlay: () => Promise<void>;
  startPlayExisting: (instanceId: number) => Promise<void>;
  loadRentedInstances: () => Promise<void>;
  saveVastApiKey: (apiKey: string) => Promise<void>;
  savePlatformCredentials: (payload: PlatformCredentialsUpdate) => Promise<void>;
  saveServerPreferences: (payload: Partial<ServerPreferencesUpdate>) => Promise<void>;
  saveMoonlightPreferences: (payload: MoonlightPreferences) => Promise<void>;
  saveSshCredentials: (payload: SshCredentialsUpdate) => Promise<void>;
  submitPin: (pin: string) => Promise<void>;
  skipPairing: () => Promise<void>;
  setupLocalWireguardClient: () => Promise<void>;
  clearError: () => void;
}

function mapError(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }

  if (typeof error === "object" && error !== null) {
    const details = Reflect.get(error, "details");
    if (typeof details === "string" && details.trim().length > 0) {
      return details;
    }

    const message = Reflect.get(error, "message");
    if (typeof message === "string") {
      return message;
    }
  }

  return "Something went wrong. Check logs and try again.";
}

export const useAppStore = create<AppStore>((set, get) => ({
  appState: null,
  offers: [],
  rentedInstances: [],
  logs: [],
  loading: true,
  searching: false,
  offersPage: 1,
  offersPageSize: 24,
  offersHasNextPage: false,
  busy: false,
  serverPickerOpen: false,
  error: null,
  _eventsBound: false,

  initialize: async () => {
    set({ loading: true, error: null });
    try {
      const [appState, logs] = await Promise.all([getAppState(), getProvisioningLogs()]);
      let rentedInstances: RentedInstanceSummary[] = [];
      if (appState.onboardingCompleted && appState.credentials.vastApiKey.trim().length > 0) {
        rentedInstances = await getRentedInstances();
      }

      set({ appState, logs, rentedInstances, loading: false });
    } catch (error) {
      set({ loading: false, error: mapError(error) });
    }
  },

  bindEvents: async () => {
    if (get()._eventsBound) {
      return;
    }

    await subscribeProvisioningEvents((event) => {
      set((state) => {
        const nextLogs = [event, ...state.logs].slice(0, 500);
        const nextState = state.appState
          ? {
              ...state.appState,
              orchestrationState: event.state,
              lastError: event.isError ? event.message : state.appState.lastError
            }
          : state.appState;

        return {
          logs: nextLogs,
          appState: nextState,
          error: event.isError ? event.message : state.error
        };
      });
    });

    set({ _eventsBound: true });
  },

  setServerPickerOpen: (serverPickerOpen) => set({ serverPickerOpen }),

  runOnboarding: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await completeOnboarding(payload);
      const rentedInstances = await getRentedInstances();
      set({ appState, rentedInstances, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveManualLocation: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await setManualLocation(payload);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  discoverOffers: async (page) => {
    set({ searching: true, error: null });
    try {
      const state = get();
      const targetPage = Math.max(1, page ?? state.offersPage);
      const offers = await searchOffers(targetPage, state.offersPageSize);
      set({
        offers,
        offersPage: targetPage,
        offersHasNextPage: offers.length === state.offersPageSize,
        searching: false
      });
    } catch (error) {
      set({ searching: false, error: mapError(error) });
    }
  },

  nextOffersPage: async () => {
    const state = get();
    if (state.searching || !state.offersHasNextPage) {
      return;
    }

    await state.discoverOffers(state.offersPage + 1);
  },

  previousOffersPage: async () => {
    const state = get();
    if (state.searching || state.offersPage <= 1) {
      return;
    }

    await state.discoverOffers(state.offersPage - 1);
  },

  chooseOffer: async (offerId, storageGb) => {
    set({ busy: true, error: null });
    try {
      const appState = await selectOffer(offerId, storageGb);
      set({ appState, busy: false, serverPickerOpen: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  startPlay: async () => {
    set({ busy: true, error: null });
    try {
      await startPlayFlow();
      const appState = await getAppState();
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  startPlayExisting: async (instanceId) => {
    set({ busy: true, error: null });
    try {
      await startPlayExistingInstance(instanceId);
      const appState = await getAppState();
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  loadRentedInstances: async () => {
    set({ busy: true, error: null });
    try {
      const rentedInstances = await getRentedInstances();
      set({ rentedInstances, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveVastApiKey: async (apiKey) => {
    set({ busy: true, error: null });
    try {
      const appState = await updateVastApiKey(apiKey);
      const rentedInstances = await getRentedInstances();
      set({ appState, rentedInstances, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  savePlatformCredentials: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await updatePlatformCredentials(payload);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveServerPreferences: async (payload) => {
    set({ busy: true, error: null });
    try {
      const state = get();
      const current = state.appState?.serverPreferences;
      if (!current) {
        set({ busy: false, error: "App state not initialized" });
        return;
      }

      const fullPayload: ServerPreferencesUpdate = {
        minReliability: payload.minReliability ?? current.minReliability,
        storageGb: payload.storageGb ?? current.storageGb,
        templateHash: payload.templateHash ?? current.templateHash,
        maxHourlyPrice: payload.maxHourlyPrice ?? current.maxHourlyPrice,
        minHourlyPrice: payload.minHourlyPrice ?? current.minHourlyPrice,
        requireVerified: payload.requireVerified ?? current.requireVerified,
        requireDatacenter: payload.requireDatacenter ?? current.requireDatacenter,
        includeOnDemand: payload.includeOnDemand ?? current.includeOnDemand,
        includeInterruptible: payload.includeInterruptible ?? current.includeInterruptible,
        includeReserved: payload.includeReserved ?? current.includeReserved,
        requireStaticIp: payload.requireStaticIp ?? current.requireStaticIp,
        requireAvx: payload.requireAvx ?? current.requireAvx,
        minGpuCount: payload.minGpuCount ?? current.minGpuCount,
        minGpuRamGb: payload.minGpuRamGb ?? current.minGpuRamGb,
        minCpuCores: payload.minCpuCores ?? current.minCpuCores,
        minInetDownMbps: payload.minInetDownMbps ?? current.minInetDownMbps,
        minInetUpMbps: payload.minInetUpMbps ?? current.minInetUpMbps,
        geolocationCountryCode:
          payload.geolocationCountryCode ?? current.geolocationCountryCode,
      };

      const appState = await updateServerPreferences(fullPayload);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveMoonlightPreferences: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await updateMoonlightPreferences(payload);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveSshCredentials: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await updateSshCredentials(payload);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  submitPin: async (pin) => {
    set({ busy: true, error: null });
    try {
      const appState = await submitPairingPin(pin);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  skipPairing: async () => {
    set({ busy: true, error: null });
    try {
      const appState = await skipPairingAndContinue();
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  setupLocalWireguardClient: async () => {
    set({ busy: true, error: null });
    try {
      await setupWireguardClient();
      set({ busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  clearError: () => set({ error: null })
}));
