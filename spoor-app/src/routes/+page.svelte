<script lang="ts">
  import { onMount } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import {
    addSite,
    listSessions,
    listSites,
    openSite,
    recordStart,
    recordStop,
    removeSite,
    siteStatus,
    stopAll,
    stopSite,
  } from '$lib/ipc';
  import type { SessionRow, SiteInfo } from '$lib/types';

  const POLL_MS = 2000;

  let sites = $state.raw<SiteInfo[]>([]);
  let sessions = $state.raw<SessionRow[]>([]);
  let connected = $state(false);
  let error = $state('');
  let busy = new SvelteMap<string, boolean>();
  /** Last check result per site: true / false / null (no check script). */
  let checks = new SvelteMap<string, boolean | null>();

  let newName = $state('');
  let newUrl = $state('');

  async function refresh() {
    try {
      sites = await listSites();
      sessions = (await listSessions()).slice(0, 8);
      connected = true;
    } catch (e) {
      connected = false;
      error = String(e);
    }
  }

  /** Run one action for a site, with its buttons disabled meanwhile. */
  async function act(site: string, fn: () => Promise<unknown>) {
    busy.set(site, true);
    error = '';
    try {
      await fn();
    } catch (e) {
      error = `${site}: ${e}`;
    } finally {
      busy.delete(site);
      await refresh();
    }
  }

  const check = (site: string) =>
    act(site, async () => {
      const s = await siteStatus(site);
      checks.set(site, s.logged_in);
      if (s.error) error = `${site}: ${s.error}`;
    });

  async function add(event: SubmitEvent) {
    event.preventDefault();
    const name = newName.trim();
    const url = newUrl.trim();
    if (!name || !url) return;
    await act(name, () => addSite(name, url, null));
    if (!error) {
      newName = '';
      newUrl = '';
    }
  }

  function checkLabel(site: SiteInfo): string {
    if (!site.check) return '';
    const v = checks.get(site.name);
    if (v === undefined) return 'not checked';
    return v ? 'logged in' : 'logged out';
  }

  onMount(() => {
    refresh();
    const timer = setInterval(refresh, POLL_MS);
    return () => clearInterval(timer);
  });
</script>

<main>
  <section class="card">
    <div class="title-row">
      <h1>Spoor</h1>
      <span class="service" class:ok={connected}>
        {connected ? 'service running' : 'starting service…'}
      </span>
    </div>
    <p class="hint">
      One logged-in browser per site. Log in with <b>Open</b>; scripts and agents
      then use the session via <code>spoor exec</code> or the local API.
    </p>
    <button class="btn-danger" onclick={() => act('all', stopAll)} disabled={!connected}>
      Stop all sites
    </button>
  </section>

  <section class="card">
    <h2>Sites</h2>
    {#if sites.length === 0}
      <p class="hint">No sites yet. Add one below, or run <code>spoor site add</code>.</p>
    {/if}
    {#each sites as site (site.name)}
      {@const isBusy = busy.has(site.name)}
      <div class="site">
        <div class="site-head">
          <span class="dot" class:running={site.running} class:recording={!!site.recording}></span>
          <span class="name">{site.name}</span>
          {#if site.recording}<span class="badge rec">recording</span>{/if}
          {#if checkLabel(site)}
            <span class="badge" class:good={checks.get(site.name) === true}>{checkLabel(site)}</span>
          {/if}
        </div>
        <div class="url" title={site.url}>{site.url}</div>
        <div class="buttons">
          <button class="btn-primary" disabled={isBusy} onclick={() => act(site.name, () => openSite(site.name))}>
            Open
          </button>
          {#if site.recording}
            <button class="btn-danger" disabled={isBusy} onclick={() => act(site.name, () => recordStop(site.name))}>
              Stop recording
            </button>
          {:else}
            <button class="btn-accent" disabled={isBusy} onclick={() => act(site.name, () => recordStart(site.name))}>
              Record
            </button>
          {/if}
          {#if site.check}
            <button class="btn-plain" disabled={isBusy} onclick={() => check(site.name)}>Check</button>
          {/if}
          {#if site.running}
            <button class="btn-plain" disabled={isBusy} onclick={() => act(site.name, () => stopSite(site.name))}>
              Close browser
            </button>
          {:else}
            <button class="btn-link" disabled={isBusy} onclick={() => act(site.name, () => removeSite(site.name))}>
              Remove
            </button>
          {/if}
        </div>
      </div>
    {/each}

    <form class="add" onsubmit={add}>
      <input placeholder="name (e.g. cas)" bind:value={newName} spellcheck="false" />
      <input placeholder="https://…" bind:value={newUrl} spellcheck="false" />
      <button class="btn-primary" type="submit" disabled={!newName.trim() || !newUrl.trim()}>Add</button>
    </form>
    {#if error}<p class="error">{error}</p>{/if}
  </section>

  <section class="card">
    <h2>Recent recordings</h2>
    {#if sessions.length === 0}
      <p class="hint">None yet. <b>Record</b> a site, use it, then stop.</p>
    {/if}
    {#each sessions as s (s.id)}
      <div class="session" title={s.path}>
        <span class="name">{s.site ?? '—'}</span>
        <span class="meta">{s.started_at.replace('T', ' ').replace('Z', '')} · {s.flows} flows · {s.size}</span>
        <code class="sid">{s.id}</code>
      </div>
    {/each}
    {#if sessions.length}
      <p class="hint">Inspect with <code>spoor flows &lt;id&gt;</code> and <code>spoor trace &lt;id&gt; &lt;value&gt;</code>.</p>
    {/if}
  </section>
</main>

<style>
  main {
    padding: 4px 0 12px;
  }
  .card {
    background: #16213e;
    border-radius: 10px;
    padding: 14px;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
    margin: 12px;
  }
  .title-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }
  h1 {
    font-size: 1.1rem;
    color: #00b894;
    margin: 0 0 6px;
  }
  h2 {
    font-size: 0.95rem;
    color: #dfe6e9;
    margin: 0 0 8px;
  }
  .service {
    font-size: 0.7rem;
    color: #fdcb6e;
  }
  .service.ok {
    color: #00b894;
  }
  .hint {
    font-size: 0.75rem;
    color: #b2bec3;
    margin: 0 0 10px;
  }
  code {
    font-size: 0.72rem;
    color: #dfe6e9;
    background: #0f1830;
    padding: 1px 4px;
    border-radius: 4px;
  }
  .site {
    border-top: 1px solid #243056;
    padding: 10px 0;
  }
  .site-head {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .name {
    font-weight: 600;
  }
  .url {
    font-size: 0.72rem;
    color: #b2bec3;
    margin: 2px 0 8px 14px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #636e72;
    flex-shrink: 0;
  }
  .dot.running {
    background: #00b894;
  }
  .dot.recording {
    background: #d63031;
    animation: pulse 1.5s infinite;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.4;
    }
  }
  .badge {
    font-size: 0.65rem;
    padding: 1px 6px;
    border-radius: 8px;
    background: #2d3436;
    color: #fdcb6e;
  }
  .badge.good {
    color: #00b894;
  }
  .badge.rec {
    color: #ff7675;
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-left: 14px;
  }
  button {
    border: none;
    border-radius: 6px;
    padding: 6px 10px;
    font-size: 0.75rem;
    font-weight: 600;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .btn-primary {
    background: #00b894;
    color: #1a1a2e;
  }
  .btn-danger {
    background: #d63031;
    color: #fff;
  }
  .btn-accent {
    background: #6c5ce7;
    color: #fff;
  }
  .btn-plain {
    background: #2d3436;
    color: #dfe6e9;
    border: 1px solid #636e72;
  }
  .btn-link {
    background: transparent;
    color: #74b9ff;
    font-weight: 500;
  }
  .add {
    display: flex;
    gap: 6px;
    border-top: 1px solid #243056;
    padding-top: 10px;
  }
  input {
    flex: 1;
    min-width: 0;
    background: #0f1830;
    border: 1px solid #243056;
    border-radius: 6px;
    color: #eee;
    padding: 6px 8px;
    font-size: 0.75rem;
  }
  .error {
    color: #ff7675;
    font-size: 0.75rem;
    margin: 8px 0 0;
    word-break: break-word;
  }
  .session {
    display: grid;
    grid-template-columns: auto 1fr;
    column-gap: 8px;
    padding: 6px 0;
    border-top: 1px solid #243056;
    font-size: 0.75rem;
  }
  .session .meta {
    color: #b2bec3;
  }
  .sid {
    grid-column: 1 / -1;
    margin-top: 2px;
    justify-self: start;
  }
</style>
