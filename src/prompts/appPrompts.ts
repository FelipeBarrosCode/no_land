import moonlightCard from '../../prompts/moonlight-card.md?raw';
import wireguardCard from '../../prompts/wireguard-card.md?raw';
import tailscaleCard from '../../prompts/tailscale-card.md?raw';
import setServerCard from '../../prompts/set-server-card.md?raw';
import rentedServersSection from '../../prompts/rented-servers-section.md?raw';
import selectedServerSection from '../../prompts/selected-server-section.md?raw';
import playButtonSection from '../../prompts/play-button-section.md?raw';
import wireguardModalInfo from '../../prompts/wireguard-modal-info.md?raw';
import tailscaleModalInfo from '../../prompts/tailscale-modal-info.md?raw';
import serverPickerModalHeader from '../../prompts/server-picker-modal-header.md?raw';
import serverSearchPreferences from '../../prompts/server-search-preferences.md?raw';
import serverInstanceCard from '../../prompts/server-instance-card.md?raw';
import helpStep1 from '../../prompts/help-step-1.md?raw';
import helpStep2 from '../../prompts/help-step-2.md?raw';
import helpStep3 from '../../prompts/help-step-3.md?raw';
import helpStep4 from '../../prompts/help-step-4.md?raw';
import helpStep5 from '../../prompts/help-step-5.md?raw';
import helpStep6 from '../../prompts/help-step-6.md?raw';
import helpStep7 from '../../prompts/help-step-7.md?raw';
import helpStep8 from '../../prompts/help-step-8.md?raw';
import helpStep9 from '../../prompts/help-step-9.md?raw';
import helpStep10 from '../../prompts/help-step-10.md?raw';
import helpStep11 from '../../prompts/help-step-11.md?raw';
import helpStep12 from '../../prompts/help-step-12.md?raw';
import settingsPage from '../../prompts/settings-page.md?raw';

export const APP_PROMPTS = {
  moonlightCard,
  wireguardCard,
  tailscaleCard,
  setServerCard,
  rentedServersSection,
  selectedServerSection,
  playButtonSection,
  wireguardModalInfo,
  tailscaleModalInfo,
  serverPickerModalHeader,
  serverSearchPreferences,
  serverInstanceCard,
  helpStep1,
  helpStep2,
  helpStep3,
  helpStep4,
  helpStep5,
  helpStep6,
  helpStep7,
  helpStep8,
  helpStep9,
  helpStep10,
  helpStep11,
  helpStep12,
  settingsPage
};

export type AppPromptKey = keyof typeof APP_PROMPTS;
