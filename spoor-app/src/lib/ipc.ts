import { invoke } from '@tauri-apps/api/core';
import type {
  CandidatesSnapshot,
  FilterOutcome,
  GenerateOutcome,
  GenerateSelection,
  StatusSnapshot,
} from './types';

export function startRecording(): Promise<StatusSnapshot> {
  return invoke('start');
}

export function stopRecording(): Promise<StatusSnapshot> {
  return invoke('stop');
}

export function getStatus(): Promise<StatusSnapshot> {
  return invoke('status');
}

export function getCandidates(): Promise<CandidatesSnapshot> {
  return invoke('candidates');
}

export function generatePack(
  selected: GenerateSelection[],
  redact: boolean,
): Promise<GenerateOutcome> {
  return invoke('generate', { selected, redact });
}

export function saveExport(): Promise<string | null> {
  return invoke('save_export');
}

export function saveDump(): Promise<string | null> {
  return invoke('save_dump');
}

export function setFilter(
  pattern: string,
  action: 'ignore' | 'allow',
): Promise<FilterOutcome> {
  return invoke('set_filter', { pattern, action });
}
