export interface PendingApiKeySnapshot {
  value: string;
  version: number;
}

export interface ProbeCredentials {
  apiKey?: string;
  providerId?: string;
}

export function resolveProbeCredentials(
  pending: PendingApiKeySnapshot | null,
  inputValue: string,
  providerId: string,
  isDraft: boolean,
): ProbeCredentials {
  if (pending) {
    return pending.value ? { apiKey: pending.value } : {};
  }

  const apiKey = inputValue.trim();
  return {
    ...(apiKey ? { apiKey } : {}),
    ...(!isDraft ? { providerId } : {}),
  };
}

export function shouldClearApiKeySnapshot(
  pending: PendingApiKeySnapshot | null,
  savedVersion: number | undefined,
): boolean {
  return savedVersion !== undefined && pending?.version === savedVersion;
}

export function isCurrentProviderProbe(
  sequence: number,
  latestSequence: number,
  cardConnected: boolean,
  view: string,
): boolean {
  return sequence === latestSequence && cardConnected && view === "settings";
}

export function normalizeFetchedModels(ids: string[]): string[] {
  return [...new Set(ids.map((id) => id.trim()).filter(Boolean))];
}
