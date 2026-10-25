// Reading a values file (.rmw), the per-pixel palette weights a run's frame was made from.
//
// "RMW1" | u32 w | u32 h | u32 sub | f64 span | u32 k   (little-endian, 28-byte header)
// then per pixel, top row first: u8 n, then n x (u16 index, u16 weight).

export function parseRmw(buffer) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  if (bytes.length < 28) throw new Error('values file too short');
  const magic = String.fromCharCode(bytes[0], bytes[1], bytes[2], bytes[3]);
  if (magic !== 'RMW1') throw new Error('not a values file');
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const width = view.getUint32(4, true);
  const height = view.getUint32(8, true);
  const sub = view.getUint32(12, true);
  const span = view.getFloat64(16, true);
  const k = view.getUint32(24, true);
  const n = width * height;
  if (!width || !height || n > 64e6) throw new Error('values file has a bad size');
  const counts = new Uint8Array(n);
  // Worst case is 255 taps per pixel; grow as needed instead of reserving that.
  let taps = new Uint16Array(Math.max(16, n * 8));
  let t = 0;
  let pos = 28;
  for (let p = 0; p < n; p++) {
    if (pos >= bytes.length) throw new Error('values file ends early');
    const c = bytes[pos++];
    counts[p] = c;
    if (t + c * 2 > taps.length) {
      const bigger = new Uint16Array(Math.max(taps.length * 2, t + c * 2));
      bigger.set(taps);
      taps = bigger;
    }
    if (pos + c * 4 > bytes.length) throw new Error('values file ends early');
    for (let i = 0; i < c; i++) {
      taps[t++] = view.getUint16(pos, true);
      taps[t++] = view.getUint16(pos + 2, true);
      pos += 4;
    }
  }
  return { width, height, sub, span, k, counts, taps: taps.subarray(0, t) };
}
