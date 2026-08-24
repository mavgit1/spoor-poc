<script lang="ts">
  import type { ApiGroup, Candidate, OpState } from './types';
  import { opNoun, protocolLabel } from './groups';
  import { SvelteMap, SvelteSet } from 'svelte/reactivity';

  let {
    groups,
    opState,
    expanded,
    ontogglegroup,
    ontoggleop,
    onpattern,
    onprefer,
    ontoggleexpand,
  }: {
    groups: ApiGroup[];
    opState: SvelteMap<string, OpState>;
    expanded: SvelteSet<string>;
    ontogglegroup: (group: ApiGroup, checked: boolean) => void;
    ontoggleop: (id: string, checked: boolean) => void;
    onpattern: (id: string, pattern: string) => void;
    onprefer: (op: Candidate, group: ApiGroup, action: 'ignore' | 'allow') => void;
    ontoggleexpand: (key: string) => void;
  } = $props();

  function groupCheckState(group: ApiGroup): 'none' | 'all' | 'partial' {
    const checked = group.ops.filter((op) => opState.get(op.id)?.checked).length;
    if (checked === 0) return 'none';
    if (checked === group.ops.length) return 'all';
    return 'partial';
  }
</script>

<div class="api-list">
  {#each groups as group (group.key)}
    {@const state = groupCheckState(group)}
    {@const isOpen = expanded.has(group.key)}
    <div class="api-group">
      <div class="api-head">
        <input
          type="checkbox"
          class="api-cb"
          checked={state === 'all'}
          indeterminate={state === 'partial'}
          onclick={(e) => e.stopPropagation()}
          onchange={(e) => ontogglegroup(group, e.currentTarget.checked)}
        />
        <button type="button" class="api-info" onclick={() => ontoggleexpand(group.key)}>
          <div class="api-title">{protocolLabel(group.protocol)} API</div>
          <div class="api-meta">
            {group.origin} · {group.ops.length}
            {opNoun(group.protocol)} · {group.totalRequests}×
          </div>
        </button>
        <span class="tag {group.protocol}">{group.protocol}</span>
        <button
          type="button"
          class="expand-btn"
          onclick={() => ontoggleexpand(group.key)}
          title="Show individual operations"
        >
          {isOpen ? '▾' : '▸'}
        </button>
      </div>
      {#if isOpen}
        <div class="ops-list">
          {#each group.ops as op (op.id)}
            {@const saved = opState.get(op.id)}
            {@const checked = saved?.checked ?? false}
            {@const pattern = saved?.pattern ?? op.guessed_pattern}
            {@const savedOff = op.preference_ignored && !checked}
            <div class={['op-row', savedOff && 'saved-off']}>
              <div class="op-head">
                <input
                  type="checkbox"
                  class="op-cb"
                  {checked}
                  onchange={(e) => ontoggleop(op.id, e.currentTarget.checked)}
                />
                <span class="op-name">{op.label}</span>
                <span class="op-count">{op.request_count ?? 0}×</span>
                <span class="op-actions">
                  {#if op.preference_ignored}
                    <button
                      type="button"
                      class="btn-pref btn-allow"
                      title="Remove from ignore list"
                      onclick={() => onprefer(op, group, 'allow')}
                    >
                      Allow
                    </button>
                  {:else}
                    <button
                      type="button"
                      class="btn-pref btn-ignore"
                      title="Add to ignore list (unchecked next session)"
                      onclick={() => onprefer(op, group, 'ignore')}
                    >
                      Ignore
                    </button>
                  {/if}
                </span>
              </div>
              <div class="pattern-row">
                <label>
                  Pattern
                  <input
                    type="text"
                    class="pattern-input"
                    value={pattern}
                    oninput={(e) => onpattern(op.id, e.currentTarget.value)}
                  />
                </label>
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .api-list {
    max-height: 300px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 10px;
  }
  .api-group {
    background: #1a1a2e;
    border: 1px solid #2d3436;
    border-radius: 8px;
  }
  .api-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px;
  }
  .api-head input[type='checkbox'] {
    flex-shrink: 0;
  }
  .api-info {
    flex: 1;
    min-width: 0;
    background: transparent;
    border: none;
    color: inherit;
    text-align: left;
    padding: 0;
    cursor: pointer;
    font: inherit;
  }
  .api-title {
    font-weight: 600;
    line-height: 1.3;
    word-break: break-word;
  }
  .api-meta {
    font-size: 0.7rem;
    color: #636e72;
    margin-top: 2px;
  }
  .tag {
    display: inline-block;
    padding: 1px 5px;
    border-radius: 3px;
    font-size: 0.65rem;
    flex-shrink: 0;
    color: #fff;
  }
  .tag.rest {
    background: #0984e3;
  }
  .tag.graphql {
    background: #e17055;
  }
  .tag.jsonrpc {
    background: #a29bfe;
  }
  .tag.websocket {
    background: #00b894;
  }
  .tag.form {
    background: #fdcb6e;
    color: #2d3436;
  }
  .tag.grpcweb,
  .tag.protobuf {
    background: #636e72;
  }
  .expand-btn {
    background: transparent;
    color: #74b9ff;
    border: none;
    font-size: 0.7rem;
    padding: 4px 6px;
    cursor: pointer;
    flex-shrink: 0;
  }
  .ops-list {
    border-top: 1px solid #2d3436;
    padding: 6px 10px 8px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-height: 200px;
    overflow-x: hidden;
    overflow-y: auto;
  }
  .op-row {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px 0;
    border-bottom: 1px solid #2d3436;
  }
  .op-row:last-child {
    border-bottom: none;
  }
  .op-head {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .op-name {
    flex: 1;
    min-width: 0;
    font-size: 0.8rem;
    font-weight: 500;
    line-height: 1.25;
    word-break: break-word;
  }
  .op-count {
    flex-shrink: 0;
    font-size: 0.65rem;
    color: #00b894;
    white-space: nowrap;
  }
  .op-actions {
    flex-shrink: 0;
    min-width: 3.5rem;
    display: flex;
    justify-content: flex-end;
    align-items: center;
  }
  .btn-pref {
    background: transparent;
    cursor: pointer;
    border: 1px solid #2d3436;
    border-radius: 4px;
    padding: 3px 5px;
    font-size: 0.6rem;
    font-weight: 500;
    margin: 0;
    line-height: 1.2;
    white-space: nowrap;
  }
  .btn-ignore {
    color: #b2bec3;
  }
  .btn-ignore:hover {
    color: #dfe6e9;
    border-color: #636e72;
  }
  .btn-allow {
    color: #55efc4;
    border-color: #2d4a3e;
  }
  .btn-allow:hover {
    color: #81ecec;
    border-color: #00b894;
  }
  .saved-off .op-name {
    color: #636e72;
  }
  .pattern-row label {
    display: block;
    font-size: 0.65rem;
    color: #b2bec3;
  }
  .pattern-input {
    width: 100%;
    background: #16213e;
    border: 1px solid #2d3436;
    color: #eee;
    border-radius: 4px;
    padding: 5px 7px;
    font-size: 0.75rem;
    margin-top: 3px;
  }
</style>
