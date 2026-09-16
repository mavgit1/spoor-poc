<script lang="ts">
  import type { SessionsSnapshot, StoredSessionItem } from './types';

  let {
    snapshot,
    recording,
    analyzing,
    busy,
    onload,
    ondelete,
    ondeleteall,
  }: {
    snapshot: SessionsSnapshot | null;
    recording: boolean;
    analyzing: boolean;
    busy: boolean;
    onload: (id: string) => void;
    ondelete: (id: string) => void;
    ondeleteall: () => void;
  } = $props();

  let open = $state(true);
  let confirmTarget = $state<string | 'all' | null>(null);

  let sessions = $derived(snapshot?.sessions ?? []);
  let locked = $derived(recording);
  let loadLocked = $derived(recording || analyzing || busy);
  let lockReason = $derived(
    recording
      ? 'Stop recording before loading or deleting sessions — the current capture is still being written.'
      : analyzing
        ? 'Wait for discovery to finish before loading another session.'
        : '',
  );

  function formatWhen(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
  }

  function askDelete(id: string) {
    if (locked) return;
    confirmTarget = id;
  }

  function askDeleteAll() {
    if (locked || sessions.length === 0) return;
    confirmTarget = 'all';
  }

  function confirmDelete() {
    const target = confirmTarget;
    confirmTarget = null;
    if (target === 'all') ondeleteall();
    else if (target) ondelete(target);
  }

  function sessionLabel(item: StoredSessionItem): string {
    return `${formatWhen(item.started_at)} (${item.flow_count} flows)`;
  }
</script>

<div class="card">
  <button
    type="button"
    class="head"
    onclick={() => (open = !open)}
    aria-expanded={open}
  >
    <h2>Sessions{sessions.length ? ` (${sessions.length})` : ''}</h2>
    <span class="chev">{open ? '▾' : '▸'}</span>
  </button>

  {#if open}
    {#if snapshot}
      <p class="hint">
        {snapshot.store_path}
        <br />
        Keep {snapshot.keep} newest · cap {snapshot.max_label}
        ({snapshot.total_label} used) — SPOOR_SESSION_KEEP / SPOOR_SESSION_MAX_MB
      </p>
      <p class="cred">
        Captured traffic can contain cookies and tokens. Deleting a session cannot
        be undone.
      </p>
    {/if}

    {#if lockReason}
      <p class="lock">{lockReason}</p>
    {/if}

    {#if confirmTarget}
      <div class="confirm" role="alertdialog" aria-labelledby="sess-confirm-title">
        <p id="sess-confirm-title">
          {#if confirmTarget === 'all'}
            Delete all {sessions.length} captured sessions?
          {:else}
            Delete this session?
          {/if}
        </p>
        <p>
          Captured traffic can contain cookies and tokens. This cannot be undone.
        </p>
        <div class="row">
          <button type="button" class="btn-ghost" onclick={() => (confirmTarget = null)}>
            Cancel
          </button>
          <button type="button" class="btn-danger" onclick={confirmDelete}>
            Delete
          </button>
        </div>
      </div>
    {:else if sessions.length === 0}
      <p class="empty">No stored sessions. Record, then Stop — captures are saved here.</p>
    {:else}
      <ul class="list">
        {#each sessions as s (s.id)}
          <li class="item">
            <div class="info">
              <div class="when">{formatWhen(s.started_at)}</div>
              <div class="meta">
                {s.flow_count} flows · {s.size_label}{s.flows_capped ? ' · capped' : ''}
              </div>
            </div>
            <div class="actions">
              <button
                type="button"
                class="btn-load"
                disabled={loadLocked}
                title={sessionLabel(s)}
                onclick={() => onload(s.id)}
              >
                Load
              </button>
              <button
                type="button"
                class="btn-ghost"
                disabled={locked || busy}
                onclick={() => askDelete(s.id)}
              >
                Delete
              </button>
            </div>
          </li>
        {/each}
      </ul>
      <button
        type="button"
        class="btn-link"
        disabled={locked || busy}
        onclick={askDeleteAll}
      >
        Delete all
      </button>
    {/if}
  {/if}
</div>

<style>
  .card {
    background: #16213e;
    border-radius: 10px;
    padding: 14px;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
    margin: 12px;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    background: transparent;
    border: none;
    color: inherit;
    padding: 0;
    cursor: pointer;
    font: inherit;
  }
  h2 {
    font-size: 0.95rem;
    color: #dfe6e9;
    margin: 0;
  }
  .chev {
    color: #74b9ff;
    font-size: 0.75rem;
  }
  .hint {
    font-size: 0.7rem;
    color: #636e72;
    line-height: 1.35;
    margin: 8px 0 6px;
    word-break: break-all;
  }
  .cred {
    font-size: 0.7rem;
    color: #b2bec3;
    line-height: 1.35;
    margin: 0 0 8px;
  }
  .lock {
    font-size: 0.75rem;
    color: #fdcb6e;
    margin: 0 0 8px;
  }
  .empty {
    font-size: 0.75rem;
    color: #b2bec3;
    margin: 0;
  }
  .list {
    list-style: none;
    margin: 0 0 8px;
    padding: 0;
    max-height: 180px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 8px;
    background: #1a1a2e;
    border: 1px solid #2d3436;
    border-radius: 8px;
    padding: 8px;
  }
  .info {
    flex: 1;
    min-width: 0;
  }
  .when {
    font-size: 0.75rem;
    font-weight: 600;
    line-height: 1.3;
  }
  .meta {
    font-size: 0.65rem;
    color: #636e72;
    margin-top: 2px;
  }
  .actions {
    display: flex;
    flex-shrink: 0;
    gap: 4px;
  }
  .row {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }
  .confirm {
    background: #1a1a2e;
    border: 1px solid #d63031;
    border-radius: 8px;
    padding: 10px;
  }
  .confirm p {
    font-size: 0.75rem;
    color: #dfe6e9;
    margin: 0 0 6px;
    line-height: 1.35;
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
  .btn-load {
    background: #6c5ce7;
    color: #fff;
  }
  .btn-danger {
    background: #d63031;
    color: #fff;
  }
  .btn-ghost {
    background: #2d3436;
    color: #dfe6e9;
    border: 1px solid #636e72;
  }
  .btn-link {
    background: transparent;
    color: #ff7675;
    padding: 4px 0;
    font-weight: 500;
    font-size: 0.75rem;
  }
</style>
