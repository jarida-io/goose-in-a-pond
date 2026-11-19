# pond.jarida.io

The landing page for Goose In A Pond and Goose On The Go: a static page drawn with [Jarida Ink](https://github.com/jarida-io/ink). No build step; serve this folder as it is.

```bash
python3 -m http.server 8765 --directory site
```

| Path | What it is |
|---|---|
| `index.html`, `styles.css`, `app.js` | The page. `app.js` loads Ink's tokens and headless behaviour from jsDelivr (`@jarida/ink@1.1.0`); without it the page still renders from the fallback tokens in `styles.css`. |
| `vendor/ink.css` | Ink's stylesheet, copied from `@jarida/ink@1.1.0`, so the page needs no third-party stylesheet host. |
| `assets/shots/` | Screenshots of the current builds: the pond's web UI at 1440×900 and Goose On The Go on an Android emulator and an iOS simulator, all talking to a demo household on a pond running Gemma 4 E4B. |
| `assets/architecture.png` | The architecture diagram, from `docs/keynote/architecture.html`. |
| `assets/democratizing-ai-with-goose-in-a-pond.pdf` | The keynote slides. |
| `assets/og.png` | The link preview image, 1200×630. |

The demo household (Amina, Baraka, Pond House) is made up for the screenshots.
