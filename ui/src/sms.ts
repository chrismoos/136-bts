export const maxSmsOctets = 248;
export function smsTextInfo(text: string) {
 const characters = Array.from(text);
 const values = characters.map(character => character.codePointAt(0)!);
 if (values.some(value => value > 0xffff || (value >= 0xd800 && value <= 0xdfff))) {
  return {octets: 0, encoding: 'Unsupported', error: 'Supplementary characters, including most emoji, are not supported.'};
 }
 const encoding = values.every(value => value <= 0x7f) ? 'IRA' : values.every(value => value <= 0xff) ? 'Latin-1' : 'BMP';
 const octets = characters.length * (encoding === 'BMP' ? 2 : 1);
 return {octets, encoding, error: octets > maxSmsOctets ? `Message exceeds ${maxSmsOctets} encoded bytes (${encoding === 'BMP' ? 124 : 248} characters in this encoding).` : ''};
}
