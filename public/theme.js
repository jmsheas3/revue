'use strict';
// Loaded without defer from <head> so the saved theme applies before first paint (no flash). 'auto' follows
// the OS. A file rather than an inline script so the Content-Security-Policy can forbid inline scripts.
function applyTheme(t) {
  const auto = t !== 'light' && t !== 'dark';
  if (auto) document.documentElement.removeAttribute('data-theme');
  else document.documentElement.dataset.theme = t;
  document.getElementById('hl-light').media = auto ? '(prefers-color-scheme: light)' : t === 'light' ? 'all' : 'not all';
  document.getElementById('hl-dark').media = auto ? '(prefers-color-scheme: dark)' : t === 'dark' ? 'all' : 'not all';
}
// Same lookup as prefs.get in app.js: the cookie, then a value saved in localStorage before prefs moved to cookies.
let savedTheme = (document.cookie.match(/(?:^|; )revue\.theme=([^;]*)/) || [])[1];
if (!savedTheme) try { savedTheme = localStorage.getItem('revue.theme'); } catch {}
applyTheme(savedTheme);
