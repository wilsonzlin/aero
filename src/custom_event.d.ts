export type CustomEventLike<T = unknown> = Event & { readonly detail: T };

export function createCustomEvent<T = unknown>(type: string, detail: T): CustomEventLike<T>;
