export function relTime(syncedAt: number | null, now: number): string {
  if (syncedAt === null) return "—";
  const s = Math.max(0, Math.round((now - syncedAt) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  return `${Math.round(s / 3600)}h ago`;
}
