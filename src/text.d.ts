export function sanitizeOneLine(input: unknown): string;
export function truncateUtf8(input: unknown, maxBytes: number): string;
export function formatOneLineUtf8(input: unknown, maxBytes: number): string;
export function formatOneLineError(err: unknown, maxBytes: number, fallback?: string): string;
