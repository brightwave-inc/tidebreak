import { crc32, deflateSync } from "node:zlib";

import type { Locator } from "@playwright/test";

/**
 * A valid one-pixel PNG, built here rather than checked in, so the image a
 * flow attaches is real and nothing binary lives in the tree.
 */
export function onePixelPng(): Buffer {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(1, 0); // width
  header.writeUInt32BE(1, 4); // height
  header[8] = 8; // bits per sample
  header[9] = 2; // RGB
  // One scanline: filter type 0, then one RGB pixel.
  const pixels = Buffer.from([0, 0x2b, 0x6c, 0xb0]);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    pngChunk("IHDR", header),
    pngChunk("IDAT", deflateSync(pixels)),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

function pngChunk(type: string, data: Buffer): Buffer {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const checksum = Buffer.alloc(4);
  checksum.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, checksum]);
}

/**
 * Paste an image into a text field the way a person pastes a screenshot: a
 * `paste` event whose clipboard carries the file.
 */
export async function pasteImage(
  field: Locator,
  bytes: Buffer,
  type = "image/png",
): Promise<void> {
  await field.evaluate(
    (element, { data, mime }) => {
      const transfer = new DataTransfer();
      transfer.items.add(
        new File([new Uint8Array(data)], "screenshot.png", { type: mime }),
      );
      element.dispatchEvent(
        new ClipboardEvent("paste", {
          clipboardData: transfer,
          bubbles: true,
          cancelable: true,
        }),
      );
    },
    { data: [...bytes], mime: type },
  );
}
