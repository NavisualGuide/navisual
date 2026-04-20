<!--
  Overlay window route — Phase D.1 scaffold.

  This renders nothing by default; the overlay window is click-through and
  invisible when there is no active draw command. Future work: listen for
  `overlay:update` events from Rust and draw arrow/box/subtitle on a canvas
  spanning the full virtual desktop.
-->
<svelte:head>
  <title>AI Navigator Overlay</title>
</svelte:head>

<style>
  :global(html, body) {
    margin: 0;
    padding: 0;
    background: transparent;
    overflow: hidden;
    width: 100vw;
    height: 100vh;
    pointer-events: none;
  }
</style>
