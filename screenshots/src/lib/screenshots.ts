import { toPng } from 'html-to-image';
import { SLIDES, type SlideCopy } from './copy';
import { MAC_SIZE } from './sizes';

const W = MAC_SIZE.w;
const H = MAC_SIZE.h;

const THEME = {
  bg: '#dee6e9',
  fg: '#1d1d1f',
  muted: '#515154',
  accent: '#1f66dd',
};

const SANS = `-apple-system, BlinkMacSystemFont, "SF Pro Display", "Helvetica Neue", sans-serif`;

// Every length is a fraction of the canvas width, so a slide keeps its proportions at any size.
const METRICS = {
  captionTop: 0.05,
  headline: 0.05,
  sub: 0.0185,
  gap: 0.014,
  captureTop: 0.185,
  // A capture is 1784 px wide on a 2× display, so this width shows it at its real pixels.
  captureWidth: 1784 / 2880,
};

function capturePath(id: string): string {
  return `/captures/${id}.png`;
}

// Captures are inlined as data URLs before rendering: html-to-image cannot wait for a
// network image, and an export taken too early would have an empty frame.
const imageCache: Record<string, string> = {};

async function preloadImages(paths: string[]): Promise<void> {
  await Promise.all(
    paths
      .filter((path) => !(path in imageCache))
      .map(async (path) => {
        const response = await fetch(path);
        if (!response.ok) throw new Error(`screenshots: missing capture ${path}`);
        const blob = await response.blob();
        imageCache[path] = await new Promise<string>((resolve) => {
          const reader = new FileReader();
          reader.onloadend = () => resolve(reader.result as string);
          reader.readAsDataURL(blob);
        });
      }),
  );
}

type StyleObj = Record<string, string | number | null | undefined>;

interface ElProps {
  style?: StyleObj;
  src?: string;
  alt?: string;
  type?: string;
  onclick?: (e: Event) => void;
}

function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props?: ElProps,
  ...children: (Node | string | null | undefined)[]
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (props) {
    for (const [key, value] of Object.entries(props.style ?? {})) {
      if (value != null) (node.style as unknown as Record<string, string>)[key] = String(value);
    }
    if (props.src != null && node instanceof HTMLImageElement) node.src = props.src;
    if (props.alt != null && node instanceof HTMLImageElement) node.alt = props.alt;
    if (props.type != null) node.setAttribute('type', props.type);
    if (props.onclick) node.addEventListener('click', props.onclick);
  }
  for (const child of children) {
    if (child == null) continue;
    node.appendChild(typeof child === 'string' ? document.createTextNode(child) : child);
  }
  return node;
}

function escapeHtml(text: string): string {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function applyAccent(text: string): string {
  return escapeHtml(text).replace(
    /\[\[([^\]]+)\]\]/g,
    (_, inner: string) => `<span style="color:${THEME.accent}">${inner}</span>`,
  );
}

function Caption(cW: number, copy: SlideCopy): HTMLDivElement {
  const heading = h('h1', {
    style: {
      fontFamily: SANS,
      fontWeight: '700',
      fontSize: `${cW * METRICS.headline}px`,
      lineHeight: '1.05',
      letterSpacing: '-0.025em',
      color: THEME.fg,
      margin: '0',
    },
  });
  heading.innerHTML = applyAccent(copy.headline);

  return h(
    'div',
    {
      style: {
        position: 'absolute',
        top: `${cW * METRICS.captionTop}px`,
        left: '0',
        right: '0',
        textAlign: 'center',
      },
    },
    heading,
    h(
      'p',
      {
        style: {
          fontFamily: SANS,
          fontSize: `${cW * METRICS.sub}px`,
          lineHeight: '1.3',
          color: THEME.muted,
          margin: `${cW * METRICS.gap}px 0 0`,
        },
      },
      copy.sub,
    ),
  );
}

function Capture(cW: number, copy: SlideCopy): HTMLImageElement {
  const path = capturePath(copy.id);
  const width = cW * METRICS.captureWidth;
  return h('img', {
    src: imageCache[path] ?? path,
    alt: '',
    style: {
      position: 'absolute',
      top: `${cW * METRICS.captureTop}px`,
      left: `${(cW - width) / 2}px`,
      width: `${width}px`,
      height: 'auto',
    },
  });
}

function renderSlide(copy: SlideCopy, cW: number, cH: number): HTMLDivElement {
  return h(
    'div',
    {
      style: {
        width: `${cW}px`,
        height: `${cH}px`,
        position: 'relative',
        background: THEME.bg,
        overflow: 'hidden',
      },
    },
    Caption(cW, copy),
    Capture(cW, copy),
  );
}

// A slide is always laid out at its export size and scaled down to fit its card.
function Preview(child: HTMLElement): HTMLDivElement {
  return h(
    'div',
    {
      style: {
        containerType: 'inline-size',
        position: 'relative',
        width: '100%',
        aspectRatio: `${W}/${H}`,
        overflow: 'hidden',
      },
    },
    h(
      'div',
      {
        style: {
          position: 'absolute',
          top: '0',
          left: '0',
          width: `${W}px`,
          height: `${H}px`,
          transformOrigin: 'top left',
          transform: `scale(calc(100cqi / ${W}px))`,
        },
      },
      child,
    ),
  );
}

async function captureSlide(copy: SlideCopy): Promise<string> {
  const wrapper = h(
    'div',
    {
      style: {
        position: 'absolute',
        left: '0',
        top: '0',
        width: `${W}px`,
        height: `${H}px`,
        zIndex: '-1',
      },
    },
    renderSlide(copy, W, H),
  );
  document.body.appendChild(wrapper);
  try {
    const options = { width: W, height: H, pixelRatio: 1 };
    // The first call warms image decoding; the second yields the real PNG.
    await toPng(wrapper, options);
    return await toPng(wrapper, options);
  } finally {
    wrapper.remove();
  }
}

function exportFilename(index: number, copy: SlideCopy): string {
  return `${String(index + 1).padStart(2, '0')}-${copy.id}.png`;
}

function download(dataUrl: string, filename: string): void {
  const link = document.createElement('a');
  link.href = dataUrl;
  link.download = filename;
  link.click();
}

// `?slide=<id>` shows one slide alone at its export size, with nothing around it. A
// headless browser sized to the canvas can then capture it without the export buttons.
function mountSingle(id: string): void {
  const copy = SLIDES.find((slide) => slide.id === id);
  if (!copy) throw new Error(`screenshots: unknown slide "${id}"`);
  document.body.replaceChildren(renderSlide(copy, W, H));
  document.body.style.margin = '0';
  document.body.style.overflow = 'hidden';
  document.body.dataset.ready = 'true';
}

export async function mount(): Promise<void> {
  await preloadImages(SLIDES.map((slide) => capturePath(slide.id)));

  const single = new URLSearchParams(location.search).get('slide');
  if (single) {
    mountSingle(single);
    return;
  }

  const grid = document.getElementById('preview-grid');
  const loading = document.getElementById('loading');
  const exportAll = document.getElementById('export-all');
  if (!(grid instanceof HTMLElement) || !(loading instanceof HTMLElement) || !(exportAll instanceof HTMLButtonElement)) {
    throw new Error('screenshots: required DOM elements not found');
  }

  const buttons: HTMLButtonElement[] = [exportAll];
  let exporting = false;

  function setExporting(label: string | null): void {
    exporting = label !== null;
    exportAll.textContent = label ? `Exporting… ${label}` : 'Export All';
    for (const button of buttons) button.disabled = exporting;
  }

  async function exportSlides(indexes: number[]): Promise<void> {
    if (exporting) return;
    try {
      for (const index of indexes) {
        setExporting(`${index + 1}/${SLIDES.length}`);
        const copy = SLIDES[index];
        download(await captureSlide(copy), exportFilename(index, copy));
        // Browsers drop downloads that start in the same instant.
        await new Promise((resolve) => setTimeout(resolve, 300));
      }
    } finally {
      setExporting(null);
    }
  }

  SLIDES.forEach((copy, index) => {
    const button = h('button', { type: 'button', onclick: () => void exportSlides([index]) }, 'Export');
    button.className = 'export';
    buttons.push(button);

    const label = h('span', undefined, `${String(index + 1).padStart(2, '0')} · ${copy.id}`);
    const footer = h('div', undefined, label, button);
    footer.className = 'card-footer';

    const card = h('div', undefined, Preview(renderSlide(copy, W, H)), footer);
    card.className = 'card';
    grid.appendChild(card);
  });

  exportAll.addEventListener('click', () => void exportSlides(SLIDES.map((_, index) => index)));

  loading.hidden = true;
  grid.hidden = false;
}
