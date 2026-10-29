import { translate } from "./i18n";

const PROVISIONING_MODAL_ACTIONS = new Set([
  "provisioning.flow",
  "wireguard.appHandoff",
  "wireguard.verify",
  "moonlightSunshine.setup",
  "postWireguard.retry",
  "instance.moonlight.pair.begin",
  "instance.moonlight.pair.complete",
]);

export function getProvisioningModalEstimate(action: {
  key: string;
  stage?: string | null;
}): string | null {
  return PROVISIONING_MODAL_ACTIONS.has(action.key)
    ? getProvisioningEstimate(action.stage)
    : null;
}

/** Informational stage estimates, not countdowns or operation timeouts. */
export function getProvisioningEstimate(stage?: string | null): string {
  switch (stage) {
    case "WaitingForInstance":
      return translate("provisioning.estimate.instanceReadiness");
    case "ConfiguringSunshine":
      return translate("provisioning.estimate.minutes", { minutes: 5 });
    case "ConfiguringWireGuard":
      return translate("provisioning.estimate.minutes", { minutes: 2 });
    case "ConfiguringNvidiaHeadless":
      return translate("provisioning.estimate.fewSeconds");
    default:
      return translate("provisioning.estimate.default");
  }
}
