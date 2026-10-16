<script lang="ts">
  let { width = "100%", height = "12px", round = false }: {
    width?: string; height?: string; round?: boolean;
  } = $props();
</script>

<span class="skeleton" class:round style:width style:height aria-hidden="true"></span>

<style>
  .skeleton {
    display: block;
    flex: none;
    max-width: 100%;
    border-radius: 3px;
    background: var(--wash-2);
    animation: breathe 1.8s ease-in-out infinite;
  }
  .round { border-radius: var(--radius-control); }
  @keyframes breathe { 50% { opacity: 0.45; } }
  @media (prefers-reduced-motion: reduce) { .skeleton { animation: none; } }
</style>
