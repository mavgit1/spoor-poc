import { invoke } from '@tauri-apps/api/core';
import type {
  CandidatesSnapshot,
  FilterOutcome,
  GenerateOutcome,
  GenerateSelection,
  SessionsSnapshot,
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

export function listSessions(): Promise<SessionsSnapshot> {
  return invoke('list_sessions');
}

export function loadSession(id: string): Promise<StatusSnapshot> {
  return invoke('load_session', { id });
}

export function deleteSession(id: string): Promise<SessionsSnapshot> {
  return invoke('delete_session', { id });
}

export function deleteAllSessions(): Promise<SessionsSnapshot> {
  return invoke('delete_all_sessions');
}
