// Pictures on the board in the host's board overlay (M27e, ADR 0013): those
// the board holds, decoded by the browser, and the host's own, made from the
// clipboard or a dropped file into what the board takes (at most
// MAX_PICTURE_SIDE on the longer side and MAX_PICTURE_BYTES, a PNG, or a JPEG
// for photos and large opaque pictures), as the stream window's
// `couch_media::picture::prepare` does. Drawing it on a canvas drops what a
// photo carries, such as where it was taken; the engine checks the result
// as the host checks anyone's.

import { MAX_BOARD_PICTURES, MAX_PICTURE_BYTES, MAX_PICTURE_SIDE } from "../bindings/boardRules";

/** A picture made for the board: its id (SHA-256 in hex), bytes in base64. */
export interface Prepared {
  id: string;
  data: string;
  width: number;
  height: number;
}

/** What a dropped file or the clipboard's picture may start at most. */
const MAX_INPUT_BYTES = 64 * 1024 * 1024;
const JPEG_QUALITIES = [0.85, 0.75, 0.65, 0.55];

function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(s);
}

function fromBase64(data: string): Uint8Array<ArrayBuffer> {
  const s = atob(data);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

async function sha256Hex(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(digest, (b) => b.toString(16).padStart(2, "0")).join("");
}

function blobOf(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob | null> {
  return new Promise((resolve) => canvas.toBlob(resolve, type, quality));
}

/** Whether every pixel of `canvas` is opaque. */
function opaque(canvas: HTMLCanvasElement): boolean {
  const data = canvas.getContext("2d")?.getImageData(0, 0, canvas.width, canvas.height).data;
  if (!data) return true;
  for (let i = 3; i < data.length; i += 4) if (data[i] !== 255) return false;
  return true;
}

/** A picture from a file or the clipboard, made what the board takes. */
export async function prepare(file: Blob): Promise<Prepared> {
  if (file.size === 0 || file.size > MAX_INPUT_BYTES) throw new Error("too large a picture");
  const bitmap = await createImageBitmap(file, { imageOrientation: "from-image" });
  try {
    return await fit(bitmap, file.type === "image/jpeg");
  } finally {
    bitmap.close();
  }
}

/** `bitmap` encoded within the limits, smaller each try until it fits. */
async function fit(bitmap: ImageBitmap, photo: boolean): Promise<Prepared> {
  let scale = Math.min(1, MAX_PICTURE_SIDE / Math.max(bitmap.width, bitmap.height));
  for (;;) {
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(bitmap.width * scale));
    canvas.height = Math.max(1, Math.round(bitmap.height * scale));
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("no canvas");
    ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    const candidates: Blob[] = [];
    if (!photo) {
      const png = await blobOf(canvas, "image/png");
      if (png) candidates.push(png);
    }
    if (candidates.every((b) => b.size > MAX_PICTURE_BYTES) && opaque(canvas)) {
      for (const q of JPEG_QUALITIES) {
        const jpeg = await blobOf(canvas, "image/jpeg", q);
        if (jpeg) candidates.push(jpeg);
        if (jpeg && jpeg.size <= MAX_PICTURE_BYTES) break;
      }
    }
    const fits = candidates.find((b) => b.size <= MAX_PICTURE_BYTES);
    if (fits) {
      const bytes = new Uint8Array(await fits.arrayBuffer());
      return { id: await sha256Hex(bytes), data: toBase64(bytes), width: canvas.width, height: canvas.height };
    }
    // Still too large: three quarters the size, and again.
    scale *= 0.75;
    if (bitmap.width * scale < 16 || bitmap.height * scale < 16) {
      throw new Error("too large a picture");
    }
  }
}

/** The pictures this page has, by id: their bytes, for sending ours, and
 * their images, once decoded, for painting. */
export class PictureStore {
  /** Oldest first (a `Map` keeps the order things were put in). */
  private data = new Map<string, string>();
  private images = new Map<string, ImageBitmap>();

  /** A picture's bytes (base64): kept, and decoded; `decoded` once it is.
   * Past twice the board's pictures, the oldest the board doesn't show
   * (`shown`) go, their images closed. */
  add(id: string, data: string, decoded: () => void, shown: Set<string>) {
    if (this.data.has(id)) return;
    this.data.set(id, data);
    for (const old of [...this.data.keys()]) {
      if (this.data.size <= 2 * MAX_BOARD_PICTURES) break;
      if (old === id || shown.has(old)) continue;
      this.data.delete(old);
      this.images.get(old)?.close();
      this.images.delete(old);
    }
    const bytes = fromBase64(data);
    void createImageBitmap(new Blob([bytes]))
      .then((image) => {
        if (!this.data.has(id)) return image.close();
        this.images.set(id, image);
        decoded();
      })
      .catch(() => {
        // Not a picture the browser reads: it shows as its frame.
      });
  }

  bytes(id: string): string | undefined {
    return this.data.get(id);
  }

  image(id: string): ImageBitmap | undefined {
    return this.images.get(id);
  }
}
