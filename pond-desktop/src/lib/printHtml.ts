// Prints a page the pond wrote from a frame nobody sees, so the print window shows the page and not the app.

/** A print window that never reports back still releases the frame. */
const FRAME_LIFETIME_MS = 5 * 60_000;

/**
 * Resolves once the print window has been asked for. The frame has no `allow-scripts`, so a page
 * that was somehow not clean still runs nothing; `allow-same-origin` is only so this page may call
 * `print()`, and `allow-modals` is what lets a sandboxed frame print at all.
 */
export function printHtml(html: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const frame = document.createElement("iframe");
    frame.setAttribute("sandbox", "allow-same-origin allow-modals");
    frame.setAttribute("aria-hidden", "true");
    frame.tabIndex = -1;
    // Off the layout but not display:none, which some engines print as a blank page.
    Object.assign(frame.style, { position: "fixed", right: "0", bottom: "0", width: "0", height: "0", border: "0" });

    const release = () => frame.remove();
    frame.addEventListener(
      "load",
      () => {
        const view = frame.contentWindow;
        if (!view) {
          release();
          reject(new Error("The page to print could not be opened."));
          return;
        }
        view.addEventListener("afterprint", release, { once: true });
        setTimeout(release, FRAME_LIFETIME_MS);
        try {
          view.focus();
          view.print();
          resolve();
        } catch (e) {
          release();
          reject(e);
        }
      },
      { once: true },
    );
    frame.srcdoc = html;
    document.body.appendChild(frame);
  });
}
