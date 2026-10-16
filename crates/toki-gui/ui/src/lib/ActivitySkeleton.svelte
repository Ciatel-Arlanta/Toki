<script lang="ts">
  import Skeleton from "./Skeleton.svelte";
  import ListSkeleton from "./ListSkeleton.svelte";
  let { week = false }: { week?: boolean } = $props();
</script>

<div class="activity-loading" role="status" aria-label={week ? "Loading weekly activity" : "Loading daily activity"} aria-busy="true">
  <header class="page-head">
    <div class="headline"><Skeleton width="170px" height="34px" /><Skeleton width="110px" /></div>
  </header>
  <div class="metrics">
    {#each [1, 2, 3, 4] as metric}
      <div class="metric"><Skeleton width={metric % 2 ? "90px" : "60px"} height="22px" /><div class="sub"><Skeleton width="72%" height="10px" /></div></div>
    {/each}
  </div>
  <section class="card chart">
    <h2 class="heading">{week ? "When" : "Day"}</h2>
    {#if week}
      <div class="heatmap">
        {#each [1, 2, 3, 4, 5, 6, 7] as day}<div class="heat-row"><Skeleton width="28px" height="10px" /><Skeleton height="22px" /><Skeleton width="42px" height="10px" /></div>{/each}
      </div>
    {:else}
      <div class="timeline"><Skeleton height="44px" /></div>
      <div class="ticks">{#each [1, 2, 3, 4, 5, 6, 7, 8, 9] as tick}<Skeleton width="14px" height="9px" />{/each}</div>
      <div class="legend">{#each [1, 2, 3, 4, 5] as item}<Skeleton width="86px" height="10px" />{/each}</div>
    {/if}
  </section>
  <section class="card apps"><ListSkeleton /></section>
</div>

<style>
  .activity-loading { display: contents; }
  .sub { margin-top: 10px; }
  .chart h2 { margin-bottom: 16px; }
  .timeline { margin-top: 16px; }
  .ticks { display: flex; justify-content: space-between; margin-top: 12px; }
  .legend { display: flex; justify-content: center; gap: 16px; margin: 24px 0 12px; overflow: hidden; }
  .heatmap { display: grid; gap: 7px; }
  .heat-row { display: grid; grid-template-columns: 28px 1fr 42px; align-items: center; gap: 12px; }
  .apps { flex: 1; min-height: 280px; overflow: hidden; }
</style>
