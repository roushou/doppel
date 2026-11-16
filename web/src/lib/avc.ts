// What WebCodecs needs to decode the daemon's H.264: the codec string and
// the `avcC` record, both made from a key frame's SPS and PPS. The frames
// themselves are AVCC already, as `avcC` says.

/** `avc1.PPCCLL`: profile, constraint flags and level, from the SPS. */
export function codecString(sps: Uint8Array): string {
  const hex = (byte: number | undefined) => (byte ?? 0).toString(16).padStart(2, "0");
  return `avc1.${hex(sps[1])}${hex(sps[2])}${hex(sps[3])}`;
}

/** An `AVCDecoderConfigurationRecord` for one SPS and one PPS, with 4-byte NAL lengths. */
export function avcC(sps: Uint8Array, pps: Uint8Array): Uint8Array {
  const record = new Uint8Array(11 + sps.length + pps.length);
  const view = new DataView(record.buffer);
  record.set([1, sps[1] ?? 0, sps[2] ?? 0, sps[3] ?? 0, 0xff, 0xe1]);
  view.setUint16(6, sps.length);
  record.set(sps, 8);
  record[8 + sps.length] = 1;
  view.setUint16(9 + sps.length, pps.length);
  record.set(pps, 11 + sps.length);
  return record;
}
