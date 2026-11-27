export const LIVE_TEXT_BYTES = 400;
export const PUBLIC_TEXT_BYTES = 2000;
export function utf8Length(text: string): number {return new TextEncoder().encode(text).byteLength;}
export function textFits(text: string, maximum: number): boolean {return text.trim().length > 0 && utf8Length(text) <= maximum;}
