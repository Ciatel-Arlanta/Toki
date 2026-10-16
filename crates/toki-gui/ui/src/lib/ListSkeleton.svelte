<script lang="ts">
  import Skeleton from "./Skeleton.svelte";
  let { table = false }: { table?: boolean } = $props();
</script>

<div class="list-loading" role="status" aria-label="Loading apps" aria-busy="true">
  {#if table}
    <table class="table-loading" aria-hidden="true">
      <thead><tr><th>App</th><th style="width:190px">Alias</th><th class="num" style="width:96px">Time</th><th style="width:100px">Share</th><th class="num" style="width:72px">Ignore</th></tr></thead>
      <tbody>
        {#each [1, 2, 3, 4, 5, 6] as row}
          <tr>
            <td><div class="app-cell"><Skeleton width="26px" height="26px" round /><div class="app-text"><Skeleton width={row % 2 ? "92px" : "112px"} /><Skeleton width="85%" height="9px" /></div></div></td>
            <td><Skeleton width="85%" height="28px" round /></td>
            <td><div class="num-placeholder"><Skeleton width="40px" /></div></td>
            <td><Skeleton width="90%" height="6px" /></td>
            <td><Skeleton width="26px" height="14px" round /></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else}
  <div class="head">
    <h2 class="heading">Apps</h2>
    <Skeleton width="48px" />
  </div>
  {#each [1, 2, 3, 4, 5, 6] as row}
    <div class="row">
      <Skeleton width="28px" height="28px" round />
      <div class="name"><Skeleton width={row % 2 ? "100px" : "128px"} /></div>
      <div class="bar"><Skeleton height="8px" /></div>
      <Skeleton width="48px" />
    </div>
  {/each}
  {/if}
</div>

<style>
  .head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 16px; }
  .row { display: flex; align-items: center; gap: 12px; height: 40px; }
  .name { flex: 1; min-width: 0; }
  .bar { width: 46%; margin-right: 12px; }
  .table-loading { width: 100%; min-width: 720px; table-layout: fixed; border-collapse: collapse; }
  th { padding: var(--card-padding) 8px 8px; border-bottom: 1px solid var(--border); text-align: left; color: var(--text-faint); font-size: 11px; font-weight: 510; letter-spacing: 0.04em; text-transform: uppercase; }
  th.num { text-align: right; }
  td { height: 52px; padding: 5px 8px; border-bottom: 1px solid var(--border); }
  .app-cell { display: flex; align-items: center; gap: 12px; }
  .app-text { display: flex; flex: 1; min-width: 0; flex-direction: column; gap: 8px; }
  .num-placeholder { display: flex; justify-content: flex-end; }
</style>
