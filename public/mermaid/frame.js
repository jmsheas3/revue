'use strict';
// The diagram renderer: runs in mermaid/frame.html, a sandboxed frame with an origin of its own (an opaque one), which
// the page (the Mermaid section of app.js) asks to draw diagrams. mermaid styles its drawings with a <style> and style
// attributes, which the page's CSP refuses, and it is a large library reading the repo's text, so it runs here, where
// it can reach neither the page nor the API. It answers each request with the drawing as SVG text, which the page shows
// as an <img>: no script in it runs and it loads nothing, as for any SVG revue shows.
//
// A request is {id, source, dark}; the answer {id, svg, width, height, title, desc} or {id, error}. Requests are drawn
// one at a time, in order, since mermaid's configuration is global.

const BASE = {
  startOnLoad: false,
  securityLevel: 'strict', // labels sanitized, no click handlers or links that run script
  suppressErrorRendering: true, // a syntax error throws, rather than drawing mermaid's error picture into the page
  htmlLabels: false, // text as SVG text rather than HTML in a <foreignObject>, which an <img> draws less faithfully
  flowchart: { htmlLabels: false },
};

let queue = Promise.resolve();
addEventListener('message', (e) => {
  if (e.source !== parent) return;
  const { id, source, dark } = e.data ?? {};
  if (!Number.isSafeInteger(id) || typeof source !== 'string') return;
  queue = queue.then(() => draw(id, source, dark === true));
});

async function draw(id, source, dark) {
  const name = `revue-diagram-${id}`;
  try {
    mermaid.initialize({ ...BASE, theme: dark ? 'dark' : 'default' });
    const { svg } = await mermaid.render(name, source);
    parent.postMessage({ id, ...sized(svg) }, '*');
  } catch (err) {
    parent.postMessage({ id, error: String(err?.message ?? err) }, '*');
  } finally {
    // What mermaid leaves in the body when a drawing fails.
    for (const sel of [`#${name}`, `#d${name}`]) document.querySelector(sel)?.remove();
  }
}

// mermaid draws most diagrams at width 100% with a max-width style, which an <img> doesn't take as a size: it would be
// drawn at the browser's default size. The viewBox is the drawing's size, so it becomes its width and height. Its text
// is markup as innerHTML writes it (an xlink:href with no namespace declared, for one), so it is read as HTML, as it
// was written, and written out again as the XML document an image has to be.
function sized(text) {
  const svg = new DOMParser().parseFromString(text, 'text/html').body.firstElementChild;
  if (svg?.namespaceURI !== 'http://www.w3.org/2000/svg' || svg.localName !== 'svg') throw new Error('mermaid drew no SVG');
  const box = (svg.getAttribute('viewBox') ?? '').trim().split(/[\s,]+/).map(Number);
  let width = box[2], height = box[3];
  if (!(box.length === 4 && width > 0 && height > 0 && Number.isFinite(width) && Number.isFinite(height))) {
    width = parseFloat(svg.getAttribute('width'));
    height = parseFloat(svg.getAttribute('height'));
  }
  if (!(width > 0 && height > 0 && Number.isFinite(width) && Number.isFinite(height))) {
    throw new Error("the drawing's size is unknown");
  }
  width = Math.ceil(width);
  height = Math.ceil(height);
  svg.setAttribute('width', String(width));
  svg.setAttribute('height', String(height));
  svg.style.removeProperty('max-width');
  if (!svg.getAttribute('style')?.trim()) svg.removeAttribute('style');
  const child = (sel) => svg.querySelector(`:scope > ${sel}`)?.textContent.trim() || null;
  return { svg: new XMLSerializer().serializeToString(svg), width, height, title: child('title'), desc: child('desc') };
}

if (document.readyState === 'loading') {
  addEventListener('DOMContentLoaded', () => parent.postMessage({ ready: true }, '*'));
} else {
  parent.postMessage({ ready: true }, '*');
}
