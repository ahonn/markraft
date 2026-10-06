# Markraft · Screenshots

A local tool that renders the Mac App Store screenshots and exports them as
2880×1800 PNGs. It is not deployed.

Stack: Astro and `html-to-image`. The screenshots are English only.

## Run

```sh
pnpm install
pnpm dev
```

Open `http://localhost:4331`. **Export** on a card downloads that slide.
**Export All** downloads every slide in order.

## Slides

`src/lib/copy.ts` lists the slides. The order in that file is the order on the
product page. Each slide has a headline, a line of text, and a capture at
`public/captures/<id>.png`.

A capture is the note window alone, with its shadow, on a transparent
background. Replace a file to change what a slide shows.

## Captures

The captures come from the running app on a 2× display. The window is 780×480
points and the text size is 22, so a capture is 1784×1184 pixels and a slide
shows it at its real pixels.

1. Start a build of the app on a folder of sample notes, with the global
   shortcuts cleared so that it does not compete with an installed copy.
2. Find the window ID of the note window with `CGWindowListCopyWindowInfo`.
   The note window is on layer 3.
3. Capture that window:

   ```sh
   screencapture -x -l "WINDOW_ID" public/captures/hero.png
   ```

The toolbar buttons are accessibility buttons. Pressing `Actions · ⌘K` or
`Browse Notes · ⌘P` through System Events opens the list without the keyboard.
Avoid words that the spelling checker underlines.

## Layout

`src/lib/screenshots.ts` holds the layout and the export. Every length in
`METRICS` is a fraction of the canvas width. `src/lib/sizes.ts` holds the
export size.

## Capture without the page

`/?slide=<id>` shows one slide alone at 2880×1800. A headless browser with a
window of that size can capture it directly:

```sh
pnpm build && pnpm preview
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless=new --hide-scrollbars --force-device-scale-factor=1 \
  --window-size=2880,1800 --virtual-time-budget=15000 \
  --screenshot=exports/01-hero.png "http://localhost:4331/?slide=hero"
```

Check the size of each file. A capture taken before the page finished rendering
is a blank image of about 30 KB.

## Upload

```sh
asc screenshots upload --version-localization "LOCALIZATION_ID" \
  --path exports --device-type APP_DESKTOP
```

`exports/` is not tracked.
