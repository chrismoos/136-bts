import { mount } from 'svelte';
import App from './App.svelte';
import '@fontsource/ibm-plex-sans/latin-400.css';
import '@fontsource/ibm-plex-sans/latin-700.css';
import '@fontsource/ibm-plex-mono/latin-400.css';
import '@fontsource/ibm-plex-mono/latin-700.css';
import './console.css';
mount(App, { target: document.getElementById('app')! });
