<script lang="ts">
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { SvelteMap, SvelteSet } from 'svelte/reactivity';
  import SurfaceList from '$lib/SurfaceList.svelte';
  import SessionsPanel from '$lib/SessionsPanel.svelte';
  import {
    buildApiGroups,
    formatStatusLine,
    preferencePattern,
  } from '$lib/groups';
  import {
    deleteAllSessions,
    deleteSession,
    generatePack,
    getStatus,
    listSessions,
    loadSession,
    saveDump,
    saveExport,
    setFilter,
    startRecording,
    stopRecording,
  } from '$lib/ipc';
  import type {
    ApiGroup,
    Candidate,
    DiscoverFinished,
    OpState,
    SessionsSnapshot,
    StatusSnapshot,
  } from '$lib/types';

  let status = $state<StatusSnapshot>({
    recording: false,
    analyzing: false,
    flow_count: 0,
    spec_ready: false,
    candidate_count: 0,
    graphql_ops: 0,
    jsonrpc_ops: 0,
    rest_endpoints: 0,
    websocket_ops: 0,
    form_ops: 0,
    grpc_ops: 0,
    traffic_graphql: 0,
    traffic_jsonrpc: 0,
    traffic_rest: 0,
    traffic_websocket: 0,
    flows_classified: 0,
    flows_filtered: 0,
    flows_capped: false,
    undecoded_binary: 0,
    websocket_frames: 0,
    grpc_or_protobuf: 0,
    filters_config: '',
  });
  let candidates = $state.raw<Candidate[]>([]);
  let opState = new SvelteMap<string, OpState>();
  let expanded = new SvelteSet<string>();
  let error = $state('');
  let warn = $state('');
  let redact = $state(false);
  let busy = $state(false);
  let sessions = $state.raw<SessionsSnapshot | null>(null);

  let apiGroups = $derived(buildApiGroups(candidates));
  let candidatesLoaded = $derived(candidates.length > 0);
  let statusLine = $derived(
    formatStatusLine(status, apiGroups.length, candidatesLoaded),
  );
  let selectedOps = $derived(
    candidates.filter((c) => opState.get(c.id)?.checked).length,
  );
  let selectedApis = $derived(
    apiGroups.filter((g) => groupCheckState(g) === 'all').length,
  );
  let partialApis = $derived(
    apiGroups.filter((g) => groupCheckState(g) === 'partial').length,
  );
  let selectedSummary = $derived.by(() => {
    let text = `${selectedOps} of ${candidates.length} operations`;
    if (apiGroups.length) {
      text += ` · ${selectedApis} API(s) full`;
      if (partialApis) text += `, ${partialApis} partial`;
    }
    return text;
  });
  let generateDisabled = $derived(
    status.recording || status.analyzing || !candidatesLoaded || busy,
  );
  let dumpVisible = $derived(!status.recording && status.flow_count > 0);
  let saveVisible = $derived(status.spec_ready);

  function groupCheckState(group: ApiGroup): 'none' | 'all' | 'partial' {
    const checked = group.ops.filter((op) => opState.get(op.id)?.checked).length;
    if (checked === 0) return 'none';
    if (checked === group.ops.length) return 'all';
    return 'partial';
  }

  function adoptCandidates(list: Candidate[]) {
    candidates = list;
    opState.clear();
    expanded.clear();
    for (const c of list) {
      // Product law: patterns are pre-filled, never pre-selected.
      opState.set(c.id, { checked: false, pattern: c.guessed_pattern });
    }
  }

  function setOp(id: string, patch: Partial<OpState>) {
    const prev = opState.get(id) ?? { checked: false, pattern: '' };
    opState.set(id, { ...prev, ...patch });
  }

  function toggleGroup(group: ApiGroup, checked: boolean) {
    for (const op of group.ops) {
      setOp(op.id, { checked });
    }
  }

  function toggleExpand(key: string) {
    if (expanded.has(key)) expanded.delete(key);
    else expanded.add(key);
  }

  function selectAll() {
    for (const g of apiGroups) toggleGroup(g, true);
  }

  function selectNone() {
    for (const g of apiGroups) toggleGroup(g, false);
  }

  async function refreshSessions() {
    try {
      sessions = await listSessions();
    } catch (e) {
      error = String(e);
    }
  }

  async function onLoadSession(id: string) {
    error = '';
    warn = '';
    adoptCandidates([]);
    busy = true;
    try {
      status = await loadSession(id);
      warn = `Loaded ${id}`;
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function onDeleteSession(id: string) {
    error = '';
    warn = '';
    try {
      sessions = await deleteSession(id);
      warn = `Deleted ${id}`;
    } catch (e) {
      error = String(e);
    }
  }

  async function onDeleteAllSessions() {
    error = '';
    warn = '';
    try {
      sessions = await deleteAllSessions();
      warn = 'Deleted all captured sessions';
    } catch (e) {
      error = String(e);
    }
  }

  async function onStart() {
    error = '';
    warn = '';
    adoptCandidates([]);
    busy = true;
    try {
      status = await startRecording();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function onStop() {
    error = '';
    warn = '';
    busy = true;
    try {
      status = await stopRecording();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function onGenerate() {
    const selected = candidates
      .filter((c) => opState.get(c.id)?.checked)
      .map((c) => ({
        id: c.id,
        pattern: opState.get(c.id)?.pattern || c.guessed_pattern || null,
      }));
    if (!selected.length) {
      error = 'Select at least one API or operation';
      return;
    }
    error = '';
    warn = '';
    busy = true;
    try {
      const outcome = await generatePack(selected, redact);
      if (outcome.warnings?.length) {
        warn = outcome.warnings.join(' · ');
      }
      status = await getStatus();
      const saved = await saveExport();
      if (saved) {
        warn = warn ? `${warn} · Saved ${saved}` : `Saved ${saved}`;
      }
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function onSaveZip() {
    error = '';
    try {
      const saved = await saveExport();
      if (saved) warn = `Saved ${saved}`;
    } catch (e) {
      error = String(e);
    }
  }

  async function onSaveDump() {
    error = '';
    try {
      const saved = await saveDump();
      if (saved) warn = `Saved capture ${saved}`;
    } catch (e) {
      error = String(e);
    }
  }

  async function onPrefer(
    op: Candidate,
    group: ApiGroup,
    action: 'ignore' | 'allow',
  ) {
    const pattern = preferencePattern(
      group,
      opState.get(op.id)?.pattern || op.guessed_pattern || '',
    );
    error = '';
    warn = '';
    try {
      const data = await setFilter(pattern, action);
      warn = `${data.message}: ${pattern} → ${data.config_path}`;
      const next = candidates.map((c) =>
        c.id === op.id
          ? {
              ...c,
              preference_ignored: action === 'ignore',
              default_selected: action === 'allow',
            }
          : c,
      );
      candidates = next;
      setOp(op.id, {
        checked: action === 'allow',
        pattern: opState.get(op.id)?.pattern || op.guessed_pattern,
      });
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => {
    const unsubs: Array<() => void> = [];
    let cancelled = false;

    getStatus()
      .then((s) => {
        status = s;
      })
      .catch((e) => {
        error = String(e);
      });
    refreshSessions();

    listen<StatusSnapshot>('status', (e) => {
      status = e.payload;
    }).then((u) => {
      if (cancelled) u();
      else unsubs.push(u);
    });

    listen<number>('flow-count', (e) => {
      status = { ...status, flow_count: e.payload };
    }).then((u) => {
      if (cancelled) u();
      else unsubs.push(u);
    });

    listen<DiscoverFinished>('discover-finished', (e) => {
      if (!e.payload.ok) {
        error = e.payload.error || 'Discover failed';
        void refreshSessions();
        return;
      }
      adoptCandidates(e.payload.candidates);
      void refreshSessions();
    }).then((u) => {
      if (cancelled) u();
      else unsubs.push(u);
    });

    return () => {
      cancelled = true;
      for (const u of unsubs) u();
    };
  });
</script>

<div class="card">
  <h1>Spoor</h1>
  <p class="hint">
    Start → browse → Stop → select APIs → Generate. Closing this window hides it
    to the tray (Quit from the tray to exit). <strong>Ignore</strong> adds to
    your filters file (still captured, listed, unchecked next time).
    <strong>Allow</strong> on grey ops removes that ignore.
  </p>
  <div class="status-row">
    <div
      class={['dot', status.recording && 'recording', status.analyzing && 'analyzing']}
    ></div>
    <span>
      {#if status.recording}
        Recording
      {:else if status.analyzing}
        Discovering…
      {:else}
        Ready
      {/if}
    </span>
  </div>
  <div class="counter" title={status.filters_config}>{statusLine}</div>
  <div class="buttons">
    <button
      class="btn-primary"
      disabled={status.recording || status.analyzing || busy}
      onclick={onStart}
    >
      Start
    </button>
    <button class="btn-danger" disabled={!status.recording} onclick={onStop}>
      Stop
    </button>
    {#if dumpVisible}
      <button class="btn-dump" type="button" onclick={onSaveDump}>
        Save capture
      </button>
    {/if}
  </div>
  {#if error}
    <div class="error">{error}</div>
  {/if}
  {#if warn}
    <div class="warn">{warn}</div>
  {/if}
</div>

{#if apiGroups.length}
  <div class="card">
    <h2>APIs</h2>
    <div class="toolbar">
      <button type="button" class="btn-link" onclick={selectAll}>
        Select all APIs
      </button>
      <button type="button" class="btn-link" onclick={selectNone}>
        Select none
      </button>
      <span class="selected-count">{selectedSummary}</span>
    </div>
    <SurfaceList
      groups={apiGroups}
      {opState}
      {expanded}
      ontogglegroup={toggleGroup}
      ontoggleop={(id, checked) => setOp(id, { checked })}
      onpattern={(id, pattern) => setOp(id, { pattern })}
      onprefer={onPrefer}
      ontoggleexpand={toggleExpand}
    />
    <div class="buttons">
      <button class="btn-accent" disabled={generateDisabled} onclick={onGenerate}>
        Generate
      </button>
      {#if saveVisible}
        <button class="btn-download" type="button" onclick={onSaveZip}>
          Save zip
        </button>
      {/if}
    </div>
    <label
      class="opt-row"
      title="When on, replaces known secret field names and JWT-shaped strings in examples."
    >
      <input type="checkbox" bind:checked={redact} />
      <span>Redact secrets in examples</span>
    </label>
  </div>
{/if}

<SessionsPanel
  snapshot={sessions}
  recording={status.recording}
  analyzing={status.analyzing}
  {busy}
  onload={onLoadSession}
  ondelete={onDeleteSession}
  ondeleteall={onDeleteAllSessions}
/>

<style>
  .card {
    background: #16213e;
    border-radius: 10px;
    padding: 14px;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
    margin: 12px;
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
  .hint {
    font-size: 0.75rem;
    color: #b2bec3;
    line-height: 1.35;
    margin: 0 0 10px;
  }
  .status-row {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 6px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #636e72;
    flex-shrink: 0;
  }
  .dot.recording {
    background: #00b894;
    animation: pulse 1.5s infinite;
  }
  .dot.analyzing {
    background: #fdcb6e;
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
  .counter {
    font-size: 0.8rem;
    color: #b2bec3;
    margin-bottom: 10px;
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  button {
    border: none;
    border-radius: 6px;
    padding: 8px 12px;
    font-size: 0.8rem;
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
  .btn-download {
    background: #0984e3;
    color: #fff;
  }
  .btn-dump {
    background: #2d3436;
    color: #dfe6e9;
    border: 1px solid #636e72;
  }
  .btn-link {
    background: transparent;
    color: #74b9ff;
    padding: 4px 0;
    font-weight: 500;
    font-size: 0.75rem;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 8px;
    flex-wrap: wrap;
  }
  .selected-count {
    color: #b2bec3;
    font-size: 0.75rem;
  }
  .error {
    color: #ff7675;
    font-size: 0.75rem;
    margin-top: 8px;
  }
  .warn {
    color: #fdcb6e;
    font-size: 0.75rem;
    margin-top: 8px;
  }
  .opt-row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 10px;
    font-size: 0.75rem;
    color: #dfe6e9;
    cursor: pointer;
  }
  .opt-row input {
    accent-color: #00b894;
  }
</style>
