/**
 * Strips `<think>…</think>` blocks from streamed chunks, carrying `inBlock` across chunk boundaries.
 * @returns [visibleText, updatedInBlock]
 */
export function filterThinking(
  chunk: string,
  inBlock: boolean,
): [string, boolean] {
  let visible = "";
  let rest = chunk;

  for (;;) {
    if (inBlock) {
      const end = rest.indexOf("</think>");
      if (end !== -1) {
        rest = rest.slice(end + "</think>".length);
        inBlock = false;
      } else {
        break;
      }
    } else {
      const start = rest.indexOf("<think>");
      if (start !== -1) {
        visible += rest.slice(0, start);
        rest = rest.slice(start + "<think>".length);
        inBlock = true;
      } else {
        visible += rest;
        break;
      }
    }
  }

  return [visible, inBlock];
}
