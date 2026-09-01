// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      fallback: "index.html",
    }),
    output: {
      // One file, no module graph. The default split build asks the webview
      // for a dozen scripts and stylesheets in sequence, and every one of them
      // is a round trip through Tauri's custom protocol before anything can be
      // drawn. There is one route here and it is all loaded up front anyway,
      // so splitting it buys nothing and costs the whole startup.
      bundleStrategy: "inline",
    },
  },
};

export default config;
