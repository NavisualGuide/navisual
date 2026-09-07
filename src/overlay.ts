// Same font as the panel, so the on-screen caption matches the instruction it echoes.
import "@fontsource-variable/inter";
import { mount } from 'svelte';
import Overlay from './Overlay.svelte';

const app = mount(Overlay, {
  target: document.getElementById('app')!,
});

export default app;
