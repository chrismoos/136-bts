import {tick} from 'svelte';

let restoring = false;
export function preserveViewport(update: () => void): void {
 if (restoring) { update(); return; }
 const route = location.hash;
 const x = window.scrollX, y = window.scrollY;
 if (y <= 1) { update(); return; }
 const anchors = Array.from(document.querySelectorAll<HTMLElement>('[data-live-row]'))
  .map(element => ({element, top:element.getBoundingClientRect().top, bottom:element.getBoundingClientRect().bottom}))
  .filter(anchor => anchor.bottom > 0 && anchor.top < window.innerHeight);
 restoring = true;
 update();
 void tick().then(() => {
  restoring = false;
  if (location.hash !== route) return;
  const anchor = anchors.find(anchor => anchor.element.isConnected);
  if (anchor) {
   const delta = anchor.element.getBoundingClientRect().top - anchor.top;
   if (Math.abs(delta) > 1) window.scrollBy(0, delta);
  } else if (Math.abs(window.scrollY - y) > 1) window.scrollTo(x,y);
 });
}
