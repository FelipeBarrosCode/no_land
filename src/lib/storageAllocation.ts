export const MIN_STORAGE_GB = 30;

export function storageAllocationLimit(availableStorageGb: number): number {
  return Number.isFinite(availableStorageGb)
    ? Math.max(0, Math.min(10000, Math.floor(availableStorageGb)))
    : 0;
}

export function isValidStorageAllocation(input: string, maxStorageGb: number): boolean {
  const value = Number(input);
  return input.trim() !== "" && Number.isInteger(value)
    && value >= MIN_STORAGE_GB && value <= maxStorageGb;
}
