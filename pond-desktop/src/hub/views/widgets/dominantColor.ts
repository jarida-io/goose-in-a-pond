// Samples an album-art image down to a small canvas and picks a rich,
// mid-dark color from it so the Now Playing card can tint itself per track.
// Canvas access throws on a cross-origin image the CDN didn't opt into CORS
// for — that's expected, not a bug, so callers get `null` and fall back.

function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];

  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h: number;
  switch (max) {
    case rn:
      h = (gn - bn) / d + (gn < bn ? 6 : 0);
      break;
    case gn:
      h = (bn - rn) / d + 2;
      break;
    default:
      h = (rn - gn) / d + 4;
  }
  return [h * 60, s, l];
}

/** Keeps the result saturated and mid-dark so white text/icons stay readable.
    Exported for mediaCardContrast.test.ts, which asserts that promise holds
    for every hue this can produce once composited under the glass tint. */
export function toCardColor(h: number, s: number): string {
  const sat = Math.max(s * 100, 55);
  return `hsl(${Math.round(h)}, ${Math.round(sat)}%, 32%)`;
}

export function extractDominantColor(src: string): Promise<string | null> {
  return new Promise((resolve) => {
    const img = new Image();
    img.crossOrigin = "anonymous";

    img.onload = () => {
      try {
        const size = 24;
        const canvas = document.createElement("canvas");
        canvas.width = size;
        canvas.height = size;
        const ctx = canvas.getContext("2d");
        if (!ctx) {
          resolve(null);
          return;
        }
        ctx.drawImage(img, 0, 0, size, size);
        const { data } = ctx.getImageData(0, 0, size, size);

        let rSum = 0;
        let gSum = 0;
        let bSum = 0;
        let n = 0;
        let bestScore = -1;
        let best = { r: 0, g: 0, b: 0 };

        for (let i = 0; i < data.length; i += 4) {
          const a = data[i + 3];
          if (a < 200) continue;
          const r = data[i];
          const g = data[i + 1];
          const b = data[i + 2];
          n++;
          rSum += r;
          gSum += g;
          bSum += b;

          const max = Math.max(r, g, b);
          const min = Math.min(r, g, b);
          const sat = max === 0 ? 0 : (max - min) / max;
          // Prefer saturated, mid-brightness pixels over near-black/near-white ones.
          const score = sat * (1 - Math.abs(max / 255 - 0.5));
          if (score > bestScore) {
            bestScore = score;
            best = { r, g, b };
          }
        }

        if (n === 0) {
          resolve(null);
          return;
        }

        // Blend the most vibrant pixel with the image average so one stray
        // bright pixel can't swing the whole card color.
        const mixed = {
          r: best.r * 0.6 + (rSum / n) * 0.4,
          g: best.g * 0.6 + (gSum / n) * 0.4,
          b: best.b * 0.6 + (bSum / n) * 0.4,
        };
        const [h, s] = rgbToHsl(mixed.r, mixed.g, mixed.b);
        resolve(toCardColor(h, s));
      } catch {
        resolve(null);
      }
    };

    img.onerror = () => resolve(null);
    img.src = src;
  });
}
