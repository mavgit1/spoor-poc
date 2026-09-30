import { invoke } from '@tauri-apps/api/core';
import type { RecordInfo, SessionRow, SiteInfo, SiteStatus } from './types';

export const listSites = (): Promise<SiteInfo[]> => invoke('sites');

export const siteStatus = (site: string): Promise<SiteStatus> =>
  invoke('site_status', { site });

export const openSite = (site: string): Promise<unknown> =>
  invoke('open_site', { site });

export const recordStart = (site: string): Promise<RecordInfo> =>
  invoke('record_start', { site });

export const recordStop = (site: string): Promise<RecordInfo> =>
  invoke('record_stop', { site });

export const stopSite = (site: string): Promise<unknown> =>
  invoke('stop_site', { site });

export const stopAll = (): Promise<unknown> => invoke('stop_all');

export const addSite = (
  name: string,
  url: string,
  minGapMs: number | null,
): Promise<void> => invoke('add_site', { name, url, minGapMs });

export const removeSite = (name: string): Promise<void> =>
  invoke('remove_site', { name });

export const listSessions = (): Promise<SessionRow[]> => invoke('sessions');
