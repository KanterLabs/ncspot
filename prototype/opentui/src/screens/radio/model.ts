import type { Row } from "../../workspace/contracts.js";

export type RecordValue = Record<string, unknown>;
export const record = (value: unknown): RecordValue => value && typeof value === "object" && !Array.isArray(value) ? value as RecordValue : {};
const number = (value: unknown): string => typeof value === "number" && Number.isFinite(value) ? String(value) : "unavailable";
const flag = (value: unknown): string => typeof value === "boolean" ? value ? "yes" : "no" : "unavailable";
const list = (value: unknown): unknown[] => Array.isArray(value) ? value : [];
const words = (value: unknown): string => list(value).filter(item => typeof item === "string").join(" · ");
export const discoveryLevel = (value: unknown): number | undefined => typeof value === "number" && Number.isFinite(value) ? Math.max(0, Math.min(100, Math.round(value))) : undefined;

/** Only backend-supplied evidence is displayed; absent scores stay unavailable. */
export function radioLines(status: RecordValue, diagnostic?: RecordValue): string[] {
  const report = record(diagnostic?.report);
  const discovery = record(report.discovery);
  const level = discoveryLevel(status.discovery);
  const catalogTracks = number(status.catalog_tracks);
  const likedTracks = number(status.cache_tracks);
  const lines = [
    `Active: ${flag(status.active)}   Waiting: ${flag(status.waiting)}`,
    `Discovery: ${level === undefined ? "unavailable" : `${level}%`}   Familiar 0 ─ Balanced 50 ─ Explore 100`,
    `Session exclusions: ${number(status.played_count)}   Catalog tracks: ${catalogTracks}   Liked tracks: ${likedTracks}`,
    "Radio avoids session repeats; explicitly queued tracks may repeat.",
  ];
  if (!diagnostic) return [...lines, "Diagnostics: press d to load a deterministic local report."];
  lines.push(`Unique catalog: ${number(diagnostic.source_count)}   Ranked: ${number(report.catalog_count)}   Candidates: ${number(report.candidate_count)}`);
  lines.push(`RNG: ${number(report.rng_seed)}   Shortlist cached: ${flag(diagnostic.shortlist_hit)}   Confidence: ${number(report.confidence)}`);
  if (typeof diagnostic.applied_count === "number") lines.push(`Applied picks: ${number(diagnostic.applied_count)}`);
  if (typeof diagnostic.discarded_reason === "string") lines.push(`Discarded: ${diagnostic.discarded_reason}`);
  if (typeof diagnostic.history_status === "string") lines.push(`History: ${diagnostic.history_status}`);
  if (typeof diagnostic.enrichment_status === "string") lines.push(`Cache coverage: ${diagnostic.enrichment_status}`);
  if (discovery.level !== undefined) lines.push(`Discovery unplayed: ${number(discovery.selected_unplayed)}/${number(discovery.target_unplayed)}   Shortfall: ${number(discovery.shortfall)}`);
  if (typeof report.fallback === "boolean") {
    lines.push(`Quality: ${report.fallback ? "broader-cache fallback" : "related candidates"}`);
  }
  const candidates = list(report.candidates).map(record);
  const rejected = candidates.filter(candidate => candidate.excluded === true).length;
  if (candidates.length > 0 && rejected > 0) {
    lines.push(`Rejected: ${rejected}/${candidates.length} cached candidates`);
  }
  const reasons = [...list(report.reasons), ...list(discovery.reasons)]
    .filter((reason): reason is string => typeof reason === "string")
    .filter((reason, index, values) => values.indexOf(reason) === index)
    .join(" · ");
  if (reasons) lines.push(reasons);
  return lines;
}

export function diagnosticRows(diagnostic: RecordValue): Row[] {
  const report = record(diagnostic.report);
  const selected = list(report.selected);
  const values = selected.length ? selected : list(report.candidates);
  return values.map((value, index) => {
    const entry = record(value);
    const track = record(entry.track);
    const uri = typeof track.uri === "string" ? track.uri : typeof entry.track_uri === "string" ? entry.track_uri : undefined;
    const title = typeof track.title === "string" ? track.title : uri ?? `Candidate ${index + 1}`;
    const components = Object.entries(record(entry.components)).filter(([, v]) => typeof v === "number" && Number.isFinite(v)).map(([k,v]) => `${k.replaceAll("_", " ")}: ${Number(v).toFixed(3)}`).join(" · ");
    return {
      id: `${index}:${uri ?? "candidate"}`, kind: "radio-candidate", title: `${index === 0 && selected.length ? "Next: " : ""}${title}`,
      subtitle: `Score: ${typeof entry.score === "number" && Number.isFinite(entry.score) ? entry.score.toFixed(3) : "unavailable"}${entry.excluded === true ? " · excluded" : ""}${words(entry.reasons) ? ` · ${words(entry.reasons)}` : ""}`,
      detail: components || "Score components unavailable", uri,
    };
  });
}
