// Inter, shipped with the app. It was named first in the font stack for months but
// never actually bundled, so on Windows every panel rendered in Segoe UI.
import "@fontsource-variable/inter";
import { mount } from 'svelte';
import App from './App.svelte';

const app = mount(App, {
  target: document.getElementById('app')!,
});

export default app;
